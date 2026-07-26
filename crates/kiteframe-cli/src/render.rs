use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
};

use kiteframe_contract::{
    CompilationDecision, CompilationWarning, Diagnostic, Feature, PackageIdentity, Sha256Digest,
};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckResult<'a> {
    pub(crate) status: &'static str,
    pub(crate) package_identity: &'a PackageIdentity,
    pub(crate) portable_digest: &'a Sha256Digest,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LockResult<'a> {
    pub(crate) status: &'static str,
    pub(crate) lock_digest: &'a Sha256Digest,
    pub(crate) capability_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InvalidResult<'a> {
    pub(crate) status: &'static str,
    pub(crate) diagnostics: &'a [Diagnostic],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExplainResult {
    pub(crate) status: &'static str,
    pub(crate) package_identity: PackageIdentity,
    pub(crate) portable_digest: Sha256Digest,
    pub(crate) lock_digest: Sha256Digest,
    pub(crate) capabilities: Vec<ExplainCapability>,
    pub(crate) models: BTreeMap<String, String>,
    pub(crate) features: ExplainFeatures,
    pub(crate) precedence_decisions: Vec<CompilationDecision>,
    pub(crate) child_delegation_boundaries: Vec<ExplainChild>,
    pub(crate) diagnostics: Vec<CompilationWarning>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExplainCapability {
    pub(crate) identity: kiteframe_contract::CapabilityIdentity,
    pub(crate) required: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExplainFeatures {
    pub(crate) required: Vec<String>,
    pub(crate) enabled_optional: Vec<String>,
    pub(crate) omitted_optional: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExplainChild {
    pub(crate) package_identity: PackageIdentity,
    pub(crate) delegated_capabilities: Vec<String>,
    pub(crate) resolved_digest: Sha256Digest,
}

pub(crate) fn feature_names(features: impl IntoIterator<Item = Feature>) -> Vec<String> {
    features
        .into_iter()
        .map(|feature| feature.as_str().to_owned())
        .collect()
}

pub(crate) fn write_json(value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .map_err(|error| io::Error::other(error.to_string()))?;
    io::stdout().lock().write_all(&bytes)
}

pub(crate) fn write_json_to_path(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .map_err(|error| io::Error::other(error.to_string()))?;
    fs::write(path, bytes)
}

pub(crate) fn write_human_diagnostics(diagnostics: &[Diagnostic]) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    for diagnostic in diagnostics {
        writeln!(
            stderr,
            "{} {:?}: {}",
            diagnostic.code.as_str(),
            diagnostic.severity,
            diagnostic.message.as_str()
        )?;
    }
    Ok(())
}

pub(crate) fn write_human_explain(explanation: &ExplainResult) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "status: {}", explanation.status)?;
    writeln!(
        stderr,
        "package: {} {}",
        explanation.package_identity.name, explanation.package_identity.version
    )?;
    writeln!(stderr, "portable digest: {}", explanation.portable_digest)?;
    writeln!(stderr, "lock digest: {}", explanation.lock_digest)?;

    if explanation.capabilities.is_empty() {
        writeln!(stderr, "capabilities: none")?;
    } else {
        writeln!(stderr, "capabilities:")?;
        for capability in &explanation.capabilities {
            let required = if capability.required {
                "required"
            } else {
                "optional"
            };
            writeln!(
                stderr,
                "  {}@{} ({required})",
                capability.identity.name(),
                capability.identity.version()
            )?;
        }
    }

    if explanation.models.is_empty() {
        writeln!(stderr, "models: none")?;
    } else {
        writeln!(stderr, "models:")?;
        for (role, symbol) in &explanation.models {
            writeln!(stderr, "  {role} -> {symbol}")?;
        }
    }

    writeln!(stderr, "features:")?;
    writeln!(
        stderr,
        "  required: {}",
        human_list(&explanation.features.required)
    )?;
    writeln!(
        stderr,
        "  enabled optional: {}",
        human_list(&explanation.features.enabled_optional)
    )?;
    writeln!(
        stderr,
        "  omitted optional: {}",
        human_list(&explanation.features.omitted_optional)
    )?;

    if explanation.precedence_decisions.is_empty() {
        writeln!(stderr, "precedence decisions: none")?;
    } else {
        writeln!(stderr, "precedence decisions:")?;
        for decision in &explanation.precedence_decisions {
            writeln!(stderr, "  {}: {}", decision.subject, decision.outcome)?;
        }
    }

    if explanation.child_delegation_boundaries.is_empty() {
        writeln!(stderr, "child delegation boundaries: none")?;
    } else {
        writeln!(stderr, "child delegation boundaries:")?;
        for child in &explanation.child_delegation_boundaries {
            writeln!(
                stderr,
                "  {} {} -> {}",
                child.package_identity.name, child.package_identity.version, child.resolved_digest
            )?;
            writeln!(
                stderr,
                "    delegated capabilities: {}",
                human_list(&child.delegated_capabilities)
            )?;
        }
    }

    if explanation.diagnostics.is_empty() {
        writeln!(stderr, "diagnostics: none")?;
    } else {
        writeln!(stderr, "diagnostics:")?;
        for diagnostic in &explanation.diagnostics {
            writeln!(
                stderr,
                "  {} Warning: {}",
                diagnostic.code, diagnostic.message
            )?;
        }
    }
    Ok(())
}

pub(crate) fn write_human_status(message: &str) -> io::Result<()> {
    writeln!(io::stderr().lock(), "{message}")
}

fn human_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}
