use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HashError {
    #[error("could not serialize canonical JSON: {0}")]
    CanonicalJson(#[from] serde_json::Error),
}

/// RFC 8785 canonical JSON encoded as UTF-8 bytes.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, HashError> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}

/// SHA-256 of RFC 8785 canonical UTF-8 JSON, rendered as lowercase hexadecimal.
pub fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, HashError> {
    let digest = Sha256::digest(canonical_json(value)?);
    Ok(format!("{digest:x}"))
}
