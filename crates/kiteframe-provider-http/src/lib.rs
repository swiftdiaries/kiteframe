#![forbid(unsafe_code)]

mod auth;
mod response;
mod routes;
mod trace;

use std::{net::SocketAddr, path::PathBuf};

use axum::Router;
use url::Url;

pub use auth::{
    ProviderPrincipalVerifier, ProviderRequestContext, VerifiedHumanAuthentication,
    VerifiedWorkloadAuthentication,
};
pub use kiteframe_provider::VerifiedProviderPrincipals;
pub use response::{DiagnosticEnvelope, HttpErrorKind, ProviderHttpError};
pub use routes::{
    AuthenticatedStatusRequest, EnforcedAdmissionPlane, ProviderHttpServices, ProviderHttpState,
    provider_router,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerBindConfig {
    address: SocketAddr,
    certificate: Option<PathBuf>,
    private_key: Option<PathBuf>,
    insecure_loopback: bool,
    origin: Url,
}

impl ServerBindConfig {
    pub fn tls(
        address: impl AsRef<str>,
        certificate: impl Into<PathBuf>,
        private_key: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let address = parse_address(address.as_ref())?;
        let certificate = certificate.into();
        let private_key = private_key.into();
        if certificate.as_os_str().is_empty() || private_key.as_os_str().is_empty() {
            return Err("TLS certificate and private key paths are required".to_owned());
        }
        Ok(Self {
            address,
            certificate: Some(certificate),
            private_key: Some(private_key),
            insecure_loopback: false,
            origin: Self::origin(&format!("https://{address}"))?,
        })
    }

    pub fn insecure_loopback(address: impl AsRef<str>) -> Result<Self, String> {
        let address = parse_address(address.as_ref())?;
        if address.ip().to_string() != "127.0.0.1" {
            return Err("plaintext test server must bind exactly 127.0.0.1".to_owned());
        }
        Ok(Self {
            address,
            certificate: None,
            private_key: None,
            insecure_loopback: true,
            origin: Self::origin(&format!("http://{address}"))?,
        })
    }

    pub fn origin(value: &str) -> Result<Url, String> {
        let origin = Url::parse(value).map_err(|_| "provider origin must be an absolute URL")?;
        let secure_or_test_loopback = origin.scheme() == "https"
            || (origin.scheme() == "http" && origin.host_str() == Some("127.0.0.1"));
        if !secure_or_test_loopback
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(
                "provider origin must be credential-free HTTPS (or test-only HTTP 127.0.0.1) without path, query, or fragment"
                    .to_owned(),
            );
        }
        Ok(origin)
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn with_origin(mut self, value: &str) -> Result<Self, String> {
        self.origin = Self::origin(value)?;
        Ok(self)
    }

    pub fn origin_url(&self) -> &Url {
        &self.origin
    }

    pub fn is_insecure_loopback(&self) -> bool {
        self.insecure_loopback
    }
}

pub async fn serve(router: Router, config: ServerBindConfig) -> Result<(), String> {
    if config.insecure_loopback {
        let listener = tokio::net::TcpListener::bind(config.address)
            .await
            .map_err(|_| "failed to bind insecure loopback listener")?;
        return axum::serve(listener, router)
            .await
            .map_err(|_| "insecure loopback server failed".to_owned());
    }

    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        config
            .certificate
            .expect("validated TLS configuration has a certificate"),
        config
            .private_key
            .expect("validated TLS configuration has a private key"),
    )
    .await
    .map_err(|_| "failed to load TLS certificate or private key")?;
    axum_server::bind_rustls(config.address, tls)
        .serve(router.into_make_service())
        .await
        .map_err(|_| "TLS provider server failed".to_owned())
}

fn parse_address(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|_| "provider bind address must be a socket address".to_owned())
}
