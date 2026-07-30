use std::sync::Arc;

use async_trait::async_trait;
use axum::http::HeaderMap;
use clap::Parser;
use kiteframe_contract::{
    AdmissionRequest, CapabilityCatalog, CapabilityGrantSet, Diagnostic, DiagnosticCategory,
    DiagnosticCode, DiagnosticStage, InvocationOutcome, InvocationRequest, InvocationStatus,
};
use kiteframe_provider_http::{
    AuthenticatedStatusRequest, HttpErrorKind, ProviderHttpError, ProviderHttpServices,
    ProviderHttpState, ProviderPrincipalVerifier, ProviderRequestContext, ServerBindConfig,
    VerifiedHumanAuthentication, VerifiedWorkloadAuthentication, provider_router, serve,
};

#[derive(Debug, Parser)]
#[command(name = "kiteframe-provider")]
#[command(about = "Kiteframe authenticated TLS provider profile")]
struct Arguments {
    #[arg(long, default_value = "127.0.0.1:8443")]
    bind: String,

    #[arg(long, requires = "private_key", conflicts_with = "insecure_loopback")]
    certificate: Option<String>,

    #[arg(long, requires = "certificate", conflicts_with = "insecure_loopback")]
    private_key: Option<String>,

    #[arg(long, hide = true)]
    insecure_loopback: bool,

    #[arg(long)]
    origin: Option<String>,
}

#[tokio::main]
async fn main() {
    let arguments = Arguments::parse();
    let config = if arguments.insecure_loopback {
        ServerBindConfig::insecure_loopback(arguments.bind)
    } else {
        match (arguments.certificate, arguments.private_key) {
            (Some(certificate), Some(private_key)) => {
                ServerBindConfig::tls(arguments.bind, certificate, private_key)
            }
            _ => Err(
                "TLS certificate and private key are required unless --insecure-loopback is used"
                    .to_owned(),
            ),
        }
    };
    let mut config = config.unwrap_or_else(|message| exit_with_error(message));
    if let Some(origin) = arguments.origin.as_deref() {
        config = config
            .with_origin(origin)
            .unwrap_or_else(|message| exit_with_error(message));
    }
    let origin = config.origin_url().as_str().to_owned();
    let router = provider_router(
        ProviderHttpState::new(Arc::new(UnconfiguredServices))
            .with_origin(&origin)
            .unwrap_or_else(|message| exit_with_error(message)),
        Arc::new(RejectingVerifier),
    );
    if let Err(message) = serve(router, config).await {
        exit_with_error(message);
    }
}

fn exit_with_error(message: String) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

struct RejectingVerifier;

#[async_trait]
impl ProviderPrincipalVerifier for RejectingVerifier {
    async fn verify_human(
        &self,
        _headers: &HeaderMap,
    ) -> Result<VerifiedHumanAuthentication, Diagnostic> {
        Err(unconfigured_diagnostic())
    }

    async fn verify_workload(
        &self,
        _headers: &HeaderMap,
    ) -> Result<VerifiedWorkloadAuthentication, Diagnostic> {
        Err(unconfigured_diagnostic())
    }
}

struct UnconfiguredServices;

#[async_trait]
impl ProviderHttpServices for UnconfiguredServices {
    async fn catalog(
        &self,
        _context: &ProviderRequestContext,
    ) -> Result<CapabilityCatalog, ProviderHttpError> {
        Err(unconfigured_error())
    }

    async fn admit(
        &self,
        _context: &ProviderRequestContext,
        _request: AdmissionRequest,
    ) -> Result<CapabilityGrantSet, ProviderHttpError> {
        Err(unconfigured_error())
    }

    async fn invoke(
        &self,
        _context: &ProviderRequestContext,
        _request: InvocationRequest,
    ) -> Result<InvocationOutcome, ProviderHttpError> {
        Err(unconfigured_error())
    }

    async fn status(
        &self,
        _request: AuthenticatedStatusRequest,
    ) -> Result<InvocationStatus, ProviderHttpError> {
        Err(unconfigured_error())
    }
}

fn unconfigured_error() -> ProviderHttpError {
    ProviderHttpError::new(HttpErrorKind::ServiceFailure, unconfigured_diagnostic())
}

fn unconfigured_diagnostic() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::RuntimeConstruction,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Runtime,
        "provider deployment extensions are not configured",
    )
}
