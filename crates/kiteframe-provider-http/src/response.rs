use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use kiteframe_contract::{Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpErrorKind {
    Malformed,
    Authentication,
    IdentityMismatch,
    NotFound,
    Conflict,
    Timeout,
    ServiceFailure,
    PayloadTooLarge,
}

impl HttpErrorKind {
    fn status(self) -> StatusCode {
        match self {
            Self::Malformed => StatusCode::BAD_REQUEST,
            Self::Authentication => StatusCode::UNAUTHORIZED,
            Self::IdentityMismatch => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ServiceFailure => StatusCode::SERVICE_UNAVAILABLE,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderHttpError {
    kind: HttpErrorKind,
    diagnostic: Box<Diagnostic>,
}

impl ProviderHttpError {
    pub fn new(kind: HttpErrorKind, diagnostic: Diagnostic) -> Self {
        Self {
            kind,
            diagnostic: Box::new(diagnostic),
        }
    }

    pub(crate) fn malformed() -> Self {
        Self::new(
            HttpErrorKind::Malformed,
            Diagnostic::error(
                DiagnosticCode::PackageInvalid,
                DiagnosticCategory::Package,
                DiagnosticStage::Parse,
                "provider request is malformed",
            ),
        )
    }

    pub(crate) fn payload_too_large() -> Self {
        Self::new(
            HttpErrorKind::PayloadTooLarge,
            Diagnostic::error(
                DiagnosticCode::PackageInvalid,
                DiagnosticCategory::Package,
                DiagnosticStage::Parse,
                "provider request body exceeds the 1 MiB limit",
            ),
        )
    }

    pub(crate) fn authentication_failed() -> Self {
        Self::new(
            HttpErrorKind::Authentication,
            Diagnostic::error(
                DiagnosticCode::InvocationDenied,
                DiagnosticCategory::Authorization,
                DiagnosticStage::Invoke,
                "provider principal verification failed",
            ),
        )
    }

    pub(crate) fn identity_mismatch() -> Self {
        Self::new(
            HttpErrorKind::IdentityMismatch,
            Diagnostic::error(
                DiagnosticCode::InvocationDenied,
                DiagnosticCategory::Authorization,
                DiagnosticStage::Invoke,
                "authenticated provider identity does not match the request",
            ),
        )
    }

    pub(crate) fn missing_trace_context() -> Self {
        Self::new(
            HttpErrorKind::Malformed,
            Diagnostic::error(
                DiagnosticCode::PackageInvalid,
                DiagnosticCategory::Package,
                DiagnosticStage::Parse,
                "provider trace context is required",
            ),
        )
    }

    pub(crate) fn not_found() -> Self {
        Self::new(
            HttpErrorKind::NotFound,
            Diagnostic::error(
                DiagnosticCode::RuntimeConstruction,
                DiagnosticCategory::Runtime,
                DiagnosticStage::Runtime,
                "provider route was not found",
            ),
        )
    }

    pub(crate) fn method_not_allowed() -> Self {
        Self::new(
            HttpErrorKind::Malformed,
            Diagnostic::error(
                DiagnosticCode::RuntimeConstruction,
                DiagnosticCategory::Runtime,
                DiagnosticStage::Runtime,
                "provider route does not allow this method",
            ),
        )
    }

    pub(crate) fn trace_invalid() -> Self {
        Self::new(
            HttpErrorKind::Malformed,
            Diagnostic::error(
                DiagnosticCode::PackageInvalid,
                DiagnosticCategory::Package,
                DiagnosticStage::Parse,
                "provider trace context is invalid",
            ),
        )
    }
}

impl IntoResponse for ProviderHttpError {
    fn into_response(self) -> Response {
        (
            self.kind.status(),
            Json(DiagnosticEnvelope {
                diagnostics: vec![safe_diagnostic(self.diagnostic)],
            }),
        )
            .into_response()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticEnvelope {
    pub diagnostics: Vec<Diagnostic>,
}

fn safe_diagnostic(diagnostic: Box<Diagnostic>) -> Diagnostic {
    let serialized = serde_json::to_string(&diagnostic)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if [
        "bearer ",
        "cookie",
        "token",
        "secret",
        "api-key",
        "api_key",
        "credential",
        "claim",
        "prompt",
        "argument",
        "result",
        "evidence",
    ]
    .iter()
    .any(|marker| serialized.contains(marker))
    {
        let mut redacted = Diagnostic::error(
            diagnostic.code,
            diagnostic.category,
            diagnostic.stage,
            "provider request failed",
        );
        redacted.retry = diagnostic.retry;
        return redacted;
    }
    *diagnostic
}
