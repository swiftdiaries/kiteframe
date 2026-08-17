/// A byte range in a UTF-8 workflow source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

/// A bounded syntax diagnostic produced while lexing or parsing Kite source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub span: Span,
}

impl Diagnostic {
    pub(crate) fn error(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            severity: DiagnosticSeverity::Error,
            span,
        }
    }

    pub(crate) fn warning(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            severity: DiagnosticSeverity::Warning,
            span,
        }
    }

    pub(crate) fn new(message: impl Into<String>, span: Span) -> Self {
        Self::error("compiler_error", message, span)
    }
}
