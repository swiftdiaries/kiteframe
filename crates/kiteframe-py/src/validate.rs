use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    CapabilityLock, ComponentMetadataCatalog, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, FeatureSet, PackageIdentity, PackagePath, ResolvedAgent,
    RuntimeTargetDescriptor,
};
use kiteframe_core::{
    AgentPackage, PackageLimits, canonical_json, hash_domain, load_package, load_runtime_binding,
};
use kiteframe_resolver::{ResolutionInput, resolve_agent, verify_lock};
use pyo3::prelude::*;

use crate::{
    error::{canonical_ir_error, diagnostic_error, ir_parse_error},
    ir::PyResolvedAgent,
};

const TARGET_DIGEST_DOMAIN: &[u8] = b"runtime-target-catalog";

#[pyfunction]
pub(crate) fn load_resolved_agent(bytes: &[u8]) -> PyResult<PyResolvedAgent> {
    let resolved: ResolvedAgent = serde_json::from_slice(bytes).map_err(ir_parse_error)?;
    let canonical =
        canonical_json(&resolved).map_err(|diagnostic| diagnostic_error(vec![diagnostic]))?;
    if canonical != bytes {
        return Err(canonical_ir_error());
    }
    Ok(resolved.into())
}

#[pyfunction]
pub(crate) fn resolve_package(
    package: PathBuf,
    binding: PathBuf,
    target: PathBuf,
) -> PyResult<PyResolvedAgent> {
    resolve_package_inner(&package, &binding, &target)
        .map(PyResolvedAgent::from)
        .map_err(diagnostic_error)
}

fn resolve_package_inner(
    package_root: &Path,
    binding_path: &Path,
    target_path: &Path,
) -> Result<ResolvedAgent, Vec<Diagnostic>> {
    let package = load_package(package_root, PackageLimits::V1)?;
    let lock = read_lock(default_lock_path(&package))?;
    verify_lock(&package, &lock, None)?;
    let binding_path = package_relative_path(&package, binding_path)?;
    let binding = load_runtime_binding(package.root().as_path(), &binding_path, PackageLimits::V1)?;
    let (target, components) = read_target_catalog(target_path)?;
    let child_locks = read_child_locks(&package)?;

    resolve_agent(ResolutionInput {
        package,
        lock,
        child_locks,
        binding,
        target,
        components,
    })
}

fn read_target_catalog(
    path: &Path,
) -> Result<(RuntimeTargetDescriptor, ComponentMetadataCatalog), Vec<Diagnostic>> {
    let bytes = read_file(
        path,
        DiagnosticCode::ComponentUnresolved,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Resolve,
        "runtime target metadata cannot be read",
    )?;
    let components: ComponentMetadataCatalog = serde_json::from_slice(&bytes).map_err(|_| {
        single_diagnostic(
            DiagnosticCode::ComponentUnresolved,
            DiagnosticCategory::Runtime,
            DiagnosticStage::Resolve,
            "runtime target metadata is invalid",
        )
    })?;
    let canonical = canonical_json(&components).map_err(|_| {
        single_diagnostic(
            DiagnosticCode::ComponentUnresolved,
            DiagnosticCategory::Runtime,
            DiagnosticStage::Resolve,
            "runtime target metadata cannot be canonicalized",
        )
    })?;
    let supported_features: FeatureSet = components
        .components
        .values()
        .flat_map(|component| component.features.iter().cloned())
        .collect();
    let target = RuntimeTargetDescriptor {
        target: components.target.clone(),
        supported_features,
        target_digest: hash_domain(TARGET_DIGEST_DOMAIN, [canonical.as_slice()]),
    };
    Ok((target, components))
}

fn read_child_locks(
    package: &AgentPackage,
) -> Result<BTreeMap<PackageIdentity, CapabilityLock>, Vec<Diagnostic>> {
    let mut locks = BTreeMap::new();
    collect_child_locks(package, &mut locks)?;
    Ok(locks)
}

fn collect_child_locks(
    package: &AgentPackage,
    locks: &mut BTreeMap<PackageIdentity, CapabilityLock>,
) -> Result<(), Vec<Diagnostic>> {
    for child in package.subagents().values() {
        let lock = read_lock(default_lock_path(child))?;
        if locks
            .insert(child.manifest().metadata.clone(), lock)
            .is_some()
        {
            return Err(single_diagnostic(
                DiagnosticCode::LockStale,
                DiagnosticCategory::Lock,
                DiagnosticStage::Resolve,
                "duplicate child lock identity",
            ));
        }
        collect_child_locks(child, locks)?;
    }
    Ok(())
}

fn default_lock_path(package: &AgentPackage) -> PathBuf {
    package.root().as_path().join("capability.lock")
}

fn read_lock(path: PathBuf) -> Result<CapabilityLock, Vec<Diagnostic>> {
    let bytes = read_file(
        &path,
        DiagnosticCode::LockStale,
        DiagnosticCategory::Lock,
        DiagnosticStage::Lock,
        "capability lock cannot be read",
    )?;
    serde_json::from_slice(&bytes).map_err(|_| {
        single_diagnostic(
            DiagnosticCode::LockTampered,
            DiagnosticCategory::Lock,
            DiagnosticStage::Lock,
            "capability lock is invalid",
        )
    })
}

fn package_relative_path(
    package: &AgentPackage,
    selected: &Path,
) -> Result<PackagePath, Vec<Diagnostic>> {
    let canonical = selected.canonicalize().map_err(|_| {
        single_diagnostic(
            DiagnosticCode::PackageInvalid,
            DiagnosticCategory::Package,
            DiagnosticStage::Validate,
            "runtime binding cannot be opened",
        )
    })?;
    let relative = canonical
        .strip_prefix(package.root().as_path())
        .map_err(|_| {
            single_diagnostic(
                DiagnosticCode::PackageContainment,
                DiagnosticCategory::Package,
                DiagnosticStage::Validate,
                "runtime binding escapes package root",
            )
        })?;
    PackagePath::new(relative.to_string_lossy().replace('\\', "/")).map_err(|_| {
        single_diagnostic(
            DiagnosticCode::PackageContainment,
            DiagnosticCategory::Package,
            DiagnosticStage::Validate,
            "runtime binding path is invalid",
        )
    })
}

fn read_file(
    path: &Path,
    code: DiagnosticCode,
    category: DiagnosticCategory,
    stage: DiagnosticStage,
    message: &'static str,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    fs::read(path).map_err(|_| single_diagnostic(code, category, stage, message))
}

fn single_diagnostic(
    code: DiagnosticCode,
    category: DiagnosticCategory,
    stage: DiagnosticStage,
    message: &'static str,
) -> Vec<Diagnostic> {
    vec![Diagnostic::error(code, category, stage, message)]
}
