use std::{cmp::Ordering, collections::BTreeMap};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable V1 machine-readable diagnostic code.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema,
)]
pub enum DiagnosticCode {
    #[serde(rename = "KF-PKG-001")]
    PackageInvalid,
    #[serde(rename = "KF-PKG-002")]
    PackageContainment,
    #[serde(rename = "KF-LOCK-001")]
    LockStale,
    #[serde(rename = "KF-LOCK-002")]
    LockTampered,
    #[serde(rename = "KF-CAT-001")]
    CatalogIncompatible,
    #[serde(rename = "KF-FEAT-001")]
    FeatureUnsupported,
    #[serde(rename = "KF-AUTH-001")]
    AdmissionDenied,
    #[serde(rename = "KF-AUTH-002")]
    AdmissionExpired,
    #[serde(rename = "KF-AUTH-003")]
    InvocationDenied,
    #[serde(rename = "KF-AUTH-004")]
    PolicyStale,
    #[serde(rename = "KF-CAP-001")]
    PreconditionMissing,
    #[serde(rename = "KF-CAP-002")]
    ResultInvalid,
    #[serde(rename = "KF-CAP-003")]
    OutcomeUnknown,
    #[serde(rename = "KF-AUDIT-001")]
    AuditUnavailable,
    #[serde(rename = "KF-RUNTIME-001")]
    ComponentUnresolved,
    #[serde(rename = "KF-RUNTIME-002")]
    RuntimeConstruction,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCategory {
    Package,
    Lock,
    Catalog,
    Feature,
    Authorization,
    Capability,
    Audit,
    Runtime,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    Parse,
    Validate,
    Lock,
    Resolve,
    Admit,
    Invoke,
    Audit,
    Runtime,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    Never,
    AfterRefresh,
    AfterUserAction,
    StatusFirst,
}

/// A caller-supplied message that is safe to render to users.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct SafeMessage(pub String);

impl From<&str> for SafeMessage {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for SafeMessage {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// A half-open byte range within a package source file.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct SourceRange {
    pub start: u32,
    pub end: u32,
}

/// A portable, redacted diagnostic. Details must be populated only with values safe for callers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub category: DiagnosticCategory,
    pub severity: DiagnosticSeverity,
    pub stage: DiagnosticStage,
    pub package_path: Option<String>,
    pub source_range: Option<SourceRange>,
    pub message: SafeMessage,
    pub help: Option<SafeMessage>,
    pub retry: RetryClass,
    pub details: BTreeMap<String, serde_json::Value>,
}

impl Diagnostic {
    pub fn error(
        code: DiagnosticCode,
        category: DiagnosticCategory,
        stage: DiagnosticStage,
        message: impl Into<SafeMessage>,
    ) -> Self {
        Self {
            code,
            category,
            severity: DiagnosticSeverity::Error,
            stage,
            package_path: None,
            source_range: None,
            message: message.into(),
            help: None,
            retry: RetryClass::Never,
            details: BTreeMap::new(),
        }
    }
}

impl Ord for Diagnostic {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.stage,
            self.package_path.as_deref(),
            self.source_range,
            self.code,
        )
            .cmp(&(
                other.stage,
                other.package_path.as_deref(),
                other.source_range,
                other.code,
            ))
    }
}

impl PartialOrd for Diagnostic {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
