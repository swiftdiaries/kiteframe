use http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::PrincipalId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind { Stdio, WebSocket, StreamableHttp }

#[derive(Clone, Debug)]
pub struct AuthenticatedConnection { pub principal: PrincipalId, pub transport: TransportKind }

pub trait ConnectionAuthenticator: Send + Sync { fn authenticate(&self, headers: &HeaderMap) -> Result<PrincipalId, AuthError>; }

#[derive(Clone, Debug)]
pub struct StaticBearerAuthenticator { token: String, principal: PrincipalId }

impl StaticBearerAuthenticator { #[must_use] pub fn new(token: String, principal: PrincipalId) -> Self { Self { token, principal } } }

impl ConnectionAuthenticator for StaticBearerAuthenticator {
    fn authenticate(&self, headers: &HeaderMap) -> Result<PrincipalId, AuthError> {
        let value = headers.get(AUTHORIZATION).ok_or(AuthError::MissingAuthorization)?.to_str().map_err(|_| AuthError::InvalidAuthorization)?;
        let token = value.strip_prefix("Bearer ").ok_or(AuthError::InvalidAuthorization)?;
        if self.token.as_bytes().ct_eq(token.as_bytes()).unwrap_u8() != 1 { return Err(AuthError::Unauthorized); }
        Ok(self.principal.clone())
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("authorization header is required")] MissingAuthorization,
    #[error("authorization header must use Bearer authentication")] InvalidAuthorization,
    #[error("authorization is not accepted")] Unauthorized,
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
    use crate::PrincipalId;
    use super::{ConnectionAuthenticator, StaticBearerAuthenticator};

    #[test]
    fn static_bearer_authenticator_accepts_only_the_configured_token() {
        let auth = StaticBearerAuthenticator::new("token".to_owned(), PrincipalId::new("alice").unwrap());
        let mut headers = HeaderMap::new(); headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token"));
        assert_eq!(auth.authenticate(&headers).unwrap().as_str(), "alice");
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong")); assert!(auth.authenticate(&headers).is_err());
    }
}
