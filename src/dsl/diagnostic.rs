/// A byte range in a UTF-8 workflow source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A bounded syntax diagnostic produced while lexing or parsing workflow source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub(crate) fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}
