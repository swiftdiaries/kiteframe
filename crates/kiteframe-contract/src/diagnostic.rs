use std::{cmp::Ordering, collections::BTreeMap};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! define_diagnostic_codes {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        /// Stable V1 machine-readable diagnostic code.
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema,
        )]
        pub enum DiagnosticCode {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl DiagnosticCode {
            /// Complete stable V1 diagnostic-code inventory.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }
        }
    };
}

define_diagnostic_codes! {
    PackageInvalid => "KF-PKG-001",
    PackageContainment => "KF-PKG-002",
    LockStale => "KF-LOCK-001",
    LockTampered => "KF-LOCK-002",
    CatalogIncompatible => "KF-CAT-001",
    FeatureUnsupported => "KF-FEAT-001",
    AdmissionDenied => "KF-AUTH-001",
    AdmissionExpired => "KF-AUTH-002",
    InvocationDenied => "KF-AUTH-003",
    PolicyStale => "KF-AUTH-004",
    PreconditionMissing => "KF-CAP-001",
    ResultInvalid => "KF-CAP-002",
    OutcomeUnknown => "KF-CAP-003",
    AuditUnavailable => "KF-AUDIT-001",
    ComponentUnresolved => "KF-RUNTIME-001",
    RuntimeConstruction => "KF-RUNTIME-002",
    CompileOutput => "KF-CLI-001",
    LockOutput => "KF-CLI-002",
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

impl SafeMessage {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
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

impl PartialEq for Diagnostic {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Diagnostic {}

impl PartialOrd for Diagnostic {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
