use std::{fs, path::Path};

use kiteframe_contract::{
    ComponentMetadataCatalog, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage,
    FeatureSet, RuntimeTargetDescriptor,
};

use crate::{canonical_json, hash_domain};

const TARGET_DIGEST_DOMAIN: &[u8] = b"runtime-target-catalog";

/// Loads component metadata and derives its canonical runtime target descriptor.
pub fn load_runtime_target_catalog(
    path: &Path,
) -> Result<(RuntimeTargetDescriptor, ComponentMetadataCatalog), Vec<Diagnostic>> {
    let bytes =
        fs::read(path).map_err(|_| single_diagnostic("runtime target metadata cannot be read"))?;
    let components: ComponentMetadataCatalog = serde_json::from_slice(&bytes)
        .map_err(|_| single_diagnostic("runtime target metadata is invalid"))?;
    let canonical = canonical_json(&components)
        .map_err(|_| single_diagnostic("runtime target metadata cannot be canonicalized"))?;
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

fn single_diagnostic(message: &'static str) -> Vec<Diagnostic> {
    vec![Diagnostic::error(
        DiagnosticCode::ComponentUnresolved,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Resolve,
        message,
    )]
}
