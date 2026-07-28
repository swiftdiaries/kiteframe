use std::{fs::File, io::Write, path::Path};

use kiteframe_contract::{
    CapabilityLock, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, FeatureId,
    FeatureSet, LockSchemaVersion, LockedCapability, Sha256Digest,
};
use kiteframe_core::AgentPackage;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    CandidatePolicy, ValidatedCatalog, catalog::select_capabilities_with_warnings,
    descriptor::validate_descriptor,
};

const RESOLVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Selects package capabilities once and records their exact, independently verifiable bundles.
pub fn lock_package(
    package: &AgentPackage,
    catalog: &ValidatedCatalog,
    policy: CandidatePolicy,
) -> Result<CapabilityLock, Vec<Diagnostic>> {
    let requested_features = package_requested_features(package)?;
    let selection =
        select_capabilities_with_warnings(&package.manifest().spec.capabilities, catalog, policy)?;

    let mut capabilities: Vec<_> = selection
        .selected()
        .iter()
        .map(|selected| {
            let descriptor = selected.validated_descriptor();
            LockedCapability::try_new(
                descriptor.identity().clone(),
                descriptor.descriptor().clone(),
                *descriptor.descriptor().descriptor_digest(),
                *descriptor.input_schema_digest(),
                *descriptor.output_schema_digest(),
                *descriptor.stable_error_set_digest(),
                *descriptor.safety_metadata_digest(),
            )
            .expect("validated descriptor must produce a locked capability")
        })
        .collect();
    capabilities.sort_by(|left, right| left.identity().cmp(right.identity()));

    let mut lock = CapabilityLock {
        schema_version: LockSchemaVersion::V1Alpha1,
        package_portable_digest: *package.portable_digest(),
        catalog_identity: catalog.identity().name.clone(),
        catalog_digest: *catalog.catalog_digest(),
        catalog_revision: catalog.identity().revision.clone(),
        resolver_version: RESOLVER_VERSION.to_owned(),
        // Compatibility field: this is the canonical package-requested set. Target negotiation
        // occurs only during resolution and is recorded in ResolvedAgent.
        resolved_features: requested_features,
        capabilities,
        lock_digest: Sha256Digest::from_bytes([0; Sha256Digest::BYTE_LENGTH]),
    };
    lock.lock_digest = lock_digest(&lock).map_err(|error| vec![error])?;
    Ok(lock)
}

/// Verifies a lock without network or provider access. An optional catalog is an already loaded
/// local verification input and is compared exactly; no selection or substitution is performed.
pub fn verify_lock(
    package: &AgentPackage,
    lock: &CapabilityLock,
    catalog: Option<&ValidatedCatalog>,
) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();

    if lock.package_portable_digest != *package.portable_digest() {
        diagnostics.push(stale(
            "package portable digest does not match capability lock",
        ));
    }
    if lock.schema_version != LockSchemaVersion::V1Alpha1 {
        diagnostics.push(stale("capability lock schema version is unsupported"));
    }
    if lock.resolver_version != RESOLVER_VERSION {
        diagnostics.push(stale("capability lock resolver version is unsupported"));
    }
    match package_requested_features(package) {
        Ok(requested_features) if lock.resolved_features != requested_features => diagnostics.push(
            stale("package requested features do not match capability lock"),
        ),
        Err(mut errors) => diagnostics.append(&mut errors),
        Ok(_) => {}
    }
    match lock_digest(lock) {
        Ok(digest) if digest != lock.lock_digest => {
            diagnostics.push(tampered(
                "capability lock digest does not match lock contents",
            ));
        }
        Err(diagnostic) => diagnostics.push(diagnostic),
        Ok(_) => {}
    }

    for capability in &lock.capabilities {
        verify_locked_capability(capability, &mut diagnostics);
    }
    verify_capability_order(&lock.capabilities, &mut diagnostics);

    if let Some(catalog) = catalog {
        verify_catalog(lock, catalog, &mut diagnostics);
    }

    sort_diagnostics(&mut diagnostics);
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

fn package_requested_features(package: &AgentPackage) -> Result<FeatureSet, Vec<Diagnostic>> {
    package
        .manifest()
        .spec
        .features
        .required
        .iter()
        .chain(&package.manifest().spec.features.optional)
        .map(|feature| {
            FeatureId::new(feature.as_str()).map_err(|_| {
                vec![Diagnostic::error(
                    DiagnosticCode::PackageInvalid,
                    DiagnosticCategory::Package,
                    DiagnosticStage::Lock,
                    "validated package contains an invalid feature identifier",
                )]
            })
        })
        .collect()
}

/// Canonically serializes and atomically replaces a lock only after its self-contained contents
/// have passed verification. The parent directory is synchronized after replacement.
pub fn write_lock_atomic(path: &Path, lock: &CapabilityLock) -> Result<(), Diagnostic> {
    let parent = path.parent().ok_or_else(lock_parent_diagnostic)?;
    verify_lock_contents(lock)?;
    let bytes = canonical_json(lock)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(lock_io_diagnostic)?;
    temporary.write_all(&bytes).map_err(lock_io_diagnostic)?;
    temporary.as_file().sync_all().map_err(lock_io_diagnostic)?;
    temporary
        .persist(path)
        .map_err(|error| lock_io_diagnostic(error.error))?;
    sync_parent_directory(parent)?;
    Ok(())
}

fn verify_catalog(
    lock: &CapabilityLock,
    catalog: &ValidatedCatalog,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if catalog.identity().name != lock.catalog_identity {
        diagnostics.push(stale(
            "capability catalog identity does not match capability lock",
        ));
    }
    if catalog.identity().revision != lock.catalog_revision {
        diagnostics.push(stale(
            "capability catalog revision does not match capability lock",
        ));
    }

    for locked in &lock.capabilities {
        let Some(current) = catalog
            .validated_descriptors()
            .iter()
            .find(|descriptor| descriptor.identity() == locked.identity())
        else {
            diagnostics.push(stale(
                "locked capability version is absent from capability catalog",
            ));
            continue;
        };
        if current.descriptor() != locked.descriptor()
            || current.descriptor().descriptor_digest() != locked.descriptor_digest()
            || current.input_schema_digest() != locked.input_schema_digest()
            || current.output_schema_digest() != locked.output_schema_digest()
            || current.stable_error_set_digest() != locked.stable_error_set_digest()
            || current.safety_metadata_digest() != locked.safety_metadata_digest()
        {
            diagnostics.push(tampered(
                "capability catalog descriptor does not match locked descriptor",
            ));
        }
    }

    if catalog.catalog_digest() != &lock.catalog_digest {
        diagnostics.push(stale(
            "capability catalog digest does not match capability lock",
        ));
    }
}

fn verify_lock_contents(lock: &CapabilityLock) -> Result<(), Diagnostic> {
    if lock.schema_version != LockSchemaVersion::V1Alpha1 {
        return Err(stale("capability lock schema version is unsupported"));
    }
    if lock.resolver_version != RESOLVER_VERSION {
        return Err(stale("capability lock resolver version is unsupported"));
    }
    if lock_digest(lock)? != lock.lock_digest {
        return Err(tampered(
            "capability lock digest does not match lock contents",
        ));
    }
    let mut diagnostics = Vec::new();
    for capability in &lock.capabilities {
        verify_locked_capability(capability, &mut diagnostics);
    }
    verify_capability_order(&lock.capabilities, &mut diagnostics);
    sort_diagnostics(&mut diagnostics);
    diagnostics.into_iter().next().map_or(Ok(()), Err)
}

fn verify_capability_order(capabilities: &[LockedCapability], diagnostics: &mut Vec<Diagnostic>) {
    for pair in capabilities.windows(2) {
        match pair[0].identity().cmp(pair[1].identity()) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => {
                diagnostics.push(tampered("locked capabilities contain duplicate identity"))
            }
            std::cmp::Ordering::Greater => {
                diagnostics.push(tampered("locked capabilities are not sorted by identity"))
            }
        }
    }
}

fn sort_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|left, right| {
        (
            left.stage,
            left.package_path.as_deref(),
            left.source_range,
            left.code.as_str(),
            left.message.as_str(),
        )
            .cmp(&(
                right.stage,
                right.package_path.as_deref(),
                right.source_range,
                right.code.as_str(),
                right.message.as_str(),
            ))
    });
}

fn verify_locked_capability(capability: &LockedCapability, diagnostics: &mut Vec<Diagnostic>) {
    if capability.identity() != capability.descriptor().identity()
        || capability.descriptor_digest() != capability.descriptor().descriptor_digest()
    {
        diagnostics.push(tampered(
            "locked capability descriptor identity or digest does not match",
        ));
        return;
    }
    match validate_descriptor(capability.descriptor().clone()) {
        Ok(descriptor)
            if descriptor.input_schema_digest() == capability.input_schema_digest()
                && descriptor.output_schema_digest() == capability.output_schema_digest()
                && descriptor.stable_error_set_digest() == capability.stable_error_set_digest()
                && descriptor.safety_metadata_digest() == capability.safety_metadata_digest() => {}
        Ok(_) => diagnostics.push(tampered(
            "locked capability descriptor part digest does not match",
        )),
        Err(_) => diagnostics.push(tampered("locked capability descriptor bundle is invalid")),
    }
}

fn lock_digest(lock: &CapabilityLock) -> Result<Sha256Digest, Diagnostic> {
    let bytes = serde_json_canonicalizer::to_vec(&LockMaterial::from(lock))
        .map_err(|_| tampered("capability lock contents cannot be canonicalized"))?;
    Ok(Sha256Digest::from_bytes(Sha256::digest(bytes).into()))
}

fn canonical_json(lock: &CapabilityLock) -> Result<Vec<u8>, Diagnostic> {
    serde_json_canonicalizer::to_vec(lock)
        .map_err(|_| tampered("capability lock cannot be serialized canonically"))
}

fn sync_parent_directory(parent: &Path) -> Result<(), Diagnostic> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(lock_io_diagnostic)
}

fn lock_parent_diagnostic() -> Diagnostic {
    lock_io_diagnostic(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "capability lock path has no parent directory",
    ))
}

fn lock_io_diagnostic(_: std::io::Error) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::LockStale,
        DiagnosticCategory::Lock,
        DiagnosticStage::Lock,
        "capability lock could not be written atomically",
    )
}

fn stale(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::LockStale,
        DiagnosticCategory::Lock,
        DiagnosticStage::Lock,
        message.into(),
    )
}

fn tampered(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::LockTampered,
        DiagnosticCategory::Lock,
        DiagnosticStage::Lock,
        message.into(),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LockMaterial<'a> {
    schema_version: LockSchemaVersion,
    package_portable_digest: Sha256Digest,
    catalog_identity: &'a str,
    catalog_digest: Sha256Digest,
    catalog_revision: &'a str,
    resolver_version: &'a str,
    resolved_features: &'a FeatureSet,
    capabilities: &'a [LockedCapability],
}

impl<'a> From<&'a CapabilityLock> for LockMaterial<'a> {
    fn from(lock: &'a CapabilityLock) -> Self {
        Self {
            schema_version: lock.schema_version,
            package_portable_digest: lock.package_portable_digest,
            catalog_identity: &lock.catalog_identity,
            catalog_digest: lock.catalog_digest,
            catalog_revision: &lock.catalog_revision,
            resolver_version: &lock.resolver_version,
            resolved_features: &lock.resolved_features,
            capabilities: &lock.capabilities,
        }
    }
}
