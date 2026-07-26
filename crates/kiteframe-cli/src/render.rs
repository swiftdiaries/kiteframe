use std::{
    collections::BTreeMap,
    io::{self, Write},
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

pub(crate) fn write_human_status(message: &str) -> io::Result<()> {
    writeln!(io::stderr().lock(), "{message}")
}
