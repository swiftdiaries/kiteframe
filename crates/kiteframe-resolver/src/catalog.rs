use std::collections::BTreeSet;

use kiteframe_contract::{
    CapabilityCatalog, CapabilityDescriptor, CapabilityRequirement, CompilationWarning, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage,
};
use semver::{Version, VersionReq};

use crate::descriptor::{ValidatedDescriptor, catalog_invalid, validate_descriptor};

/// A catalog whose canonical digest, descriptor digests, and schemas have been verified.
#[derive(Clone, Debug)]
pub struct ValidatedCatalog {
    catalog: CapabilityCatalog,
    validated_descriptors: Vec<ValidatedDescriptor>,
}

impl ValidatedCatalog {
    pub fn identity(&self) -> &kiteframe_contract::CatalogIdentity {
        self.catalog.identity()
    }

    pub fn catalog_digest(&self) -> &kiteframe_contract::Sha256Digest {
        self.catalog.catalog_digest()
    }

    pub fn descriptors(&self) -> &[CapabilityDescriptor] {
        self.catalog.descriptors()
    }

    pub fn validated_descriptors(&self) -> &[ValidatedDescriptor] {
        &self.validated_descriptors
    }
}

/// A monotonic candidate filter. It can remove catalog candidates but cannot introduce one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidatePolicy {
    AllowAll,
    Exact(BTreeSet<String>),
}

impl CandidatePolicy {
    pub fn exact(values: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self::Exact(
            values
                .into_iter()
                .map(|value| value.as_ref().to_owned())
                .collect(),
        )
    }

    fn allows(&self, descriptor: &ValidatedDescriptor) -> bool {
        match self {
            Self::AllowAll => true,
            Self::Exact(identities) => identities.contains(&format!(
                "{}@{}",
                descriptor.identity().name(),
                descriptor.identity().version()
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedCapability {
    requirement: CapabilityRequirement,
    descriptor: ValidatedDescriptor,
}

impl SelectedCapability {
    pub fn requirement(&self) -> &CapabilityRequirement {
        &self.requirement
    }

    pub fn descriptor(&self) -> &CapabilityDescriptor {
        self.descriptor.descriptor()
    }

    pub fn validated_descriptor(&self) -> &ValidatedDescriptor {
        &self.descriptor
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionOutcome {
    selected: Vec<SelectedCapability>,
    warnings: Vec<CompilationWarning>,
}

impl SelectionOutcome {
    pub fn selected(&self) -> &[SelectedCapability] {
        &self.selected
    }

    pub fn warnings(&self) -> &[CompilationWarning] {
        &self.warnings
    }
}

pub fn validate_catalog(bytes: &[u8]) -> Result<ValidatedCatalog, Vec<Diagnostic>> {
    let catalog: CapabilityCatalog = serde_json::from_slice(bytes).map_err(|_| {
        vec![catalog_invalid(
            "catalog bytes are not a valid canonical capability catalog",
        )]
    })?;

    let validated_descriptors: Result<Vec<_>, _> = catalog
        .descriptors()
        .iter()
        .cloned()
        .map(validate_descriptor)
        .collect();
    validated_descriptors
        .map(|validated_descriptors| ValidatedCatalog {
            catalog,
            validated_descriptors,
        })
        .map_err(|error| vec![error])
}

/// Compatibility helper that discards optional-miss warnings.
///
/// New resolution and IR assembly must use [`select_capabilities_with_warnings`].
#[deprecated(
    note = "use select_capabilities_with_warnings so optional capability misses are retained"
)]
pub fn select_capabilities(
    requirements: &[CapabilityRequirement],
    catalog: &ValidatedCatalog,
    policy_filter: CandidatePolicy,
) -> Result<Vec<SelectedCapability>, Vec<Diagnostic>> {
    select_capabilities_with_warnings(requirements, catalog, policy_filter)
        .map(|outcome| outcome.selected)
}

/// Select exact descriptors while retaining optional misses as deterministic warnings for IR assembly.
pub fn select_capabilities_with_warnings(
    requirements: &[CapabilityRequirement],
    catalog: &ValidatedCatalog,
    policy_filter: CandidatePolicy,
) -> Result<SelectionOutcome, Vec<Diagnostic>> {
    let mut requirements = requirements.to_vec();
    requirements.sort_by(|left, right| {
        (
            left.name.as_str(),
            left.version.as_str(),
            left.required,
            &left.resources,
        )
            .cmp(&(
                right.name.as_str(),
                right.version.as_str(),
                right.required,
                &right.resources,
            ))
    });
    requirements.dedup();

    let mut selected = Vec::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for requirement in requirements {
        let version_requirement =
            VersionReq::parse(requirement.version.as_str()).map_err(|_| {
                vec![catalog_invalid(
                    "capability requirement has an invalid SemVer range",
                )]
            })?;
        let selected_descriptor = catalog
            .validated_descriptors()
            .iter()
            .filter(|descriptor| descriptor.identity().name() == &requirement.name)
            .filter(|descriptor| policy_filter.allows(descriptor))
            .filter_map(|descriptor| {
                Version::parse(descriptor.identity().version().as_str())
                    .ok()
                    .filter(|version| version_requirement.matches(version))
                    .map(|version| (version, descriptor))
            })
            .max_by(|(left, _), (right, _)| left.cmp(right))
            .map(|(_, descriptor)| descriptor.clone());

        match selected_descriptor {
            Some(descriptor) => selected.push(SelectedCapability {
                requirement,
                descriptor,
            }),
            None if requirement.required => errors.push(incompatible_requirement(&requirement)),
            None => warnings.push(optional_miss_warning(&requirement)),
        }
    }

    if errors.is_empty() {
        warnings.sort();
        warnings.dedup();
        Ok(SelectionOutcome { selected, warnings })
    } else {
        errors.sort();
        Err(errors)
    }
}

fn incompatible_requirement(requirement: &CapabilityRequirement) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::CatalogIncompatible,
        DiagnosticCategory::Catalog,
        DiagnosticStage::Resolve,
        format!(
            "no compatible catalog capability for {} {}",
            requirement.name, requirement.version
        ),
    )
}

fn optional_miss_warning(requirement: &CapabilityRequirement) -> CompilationWarning {
    CompilationWarning {
        code: DiagnosticCode::CatalogIncompatible.as_str().to_owned(),
        message: format!(
            "optional capability {} {} is unavailable",
            requirement.name, requirement.version
        ),
    }
}
