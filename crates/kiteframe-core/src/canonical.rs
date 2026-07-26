use std::collections::BTreeMap;

use kiteframe_contract::{
    AgentManifest, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, PackagePath,
    Sha256Digest, ValidatedTextAsset,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::load::AgentPackage;

const PORTABLE_ASSET_DOMAIN: &[u8] = b"portable-asset";
const PORTABLE_CHILD_DOMAIN: &[u8] = b"portable-child";
const PORTABLE_PACKAGE_DOMAIN: &[u8] = b"portable-package";

pub fn hash_domain<'a>(
    domain: &'static [u8],
    chunks: impl IntoIterator<Item = &'a [u8]>,
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"kiteframe:v1\0");
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    for chunk in chunks {
        hasher.update((chunk.len() as u64).to_be_bytes());
        hasher.update(chunk);
    }
    Sha256Digest::from_bytes(hasher.finalize().into())
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, Diagnostic> {
    serde_json_canonicalizer::to_vec(value).map_err(|_| canonicalization_diagnostic())
}

pub(crate) fn portable_digest(
    manifest: &AgentManifest,
    prompt_assets: &BTreeMap<PackagePath, ValidatedTextAsset>,
    skill_assets: &BTreeMap<PackagePath, ValidatedTextAsset>,
    subagents: &BTreeMap<PackagePath, AgentPackage>,
) -> Result<Sha256Digest, Diagnostic> {
    let mut chunks = vec![canonical_json(manifest)?];

    let mut assets = BTreeMap::new();
    assets.extend(prompt_assets);
    assets.extend(skill_assets);
    for (path, asset) in assets {
        let entry = hash_domain(
            PORTABLE_ASSET_DOMAIN,
            [path.as_str().as_bytes(), asset.text.as_bytes()],
        );
        chunks.push(entry.as_bytes().to_vec());
    }

    for (path, child) in subagents {
        let entry = hash_domain(
            PORTABLE_CHILD_DOMAIN,
            [
                path.as_str().as_bytes(),
                child.portable_digest().as_bytes().as_slice(),
            ],
        );
        chunks.push(entry.as_bytes().to_vec());
    }

    Ok(hash_domain(
        PORTABLE_PACKAGE_DOMAIN,
        chunks.iter().map(Vec::as_slice),
    ))
}

fn canonicalization_diagnostic() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Validate,
        "portable package semantics cannot be canonicalized",
    )
}
