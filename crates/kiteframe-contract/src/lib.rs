#![forbid(unsafe_code)]

mod diagnostic;

pub use diagnostic::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticSeverity, DiagnosticStage,
    RetryClass, SafeMessage, SourceRange,
};
