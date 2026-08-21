use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{ModelRole, canonical_sha256};

use super::types::{
    HARNESS_PROTOCOL_VERSION, HarnessCapabilities, HarnessCapability, HarnessFailure,
    HarnessProfileId, HarnessTransport,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawHarnessProfile {
    id: String,
    transport: HarnessTransport,
    source_repository: String,
    source_revision: String,
    program: String,
    args: Vec<String>,
    required: Vec<HarnessCapability>,
    optional: Vec<HarnessCapability>,
    fresh_sessions_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessProfile {
    pub id: HarnessProfileId,
    pub transport: HarnessTransport,
    pub source_repository: String,
    pub source_revision: String,
    pub program: String,
    pub args: Vec<String>,
    pub required: BTreeSet<HarnessCapability>,
    pub optional: BTreeSet<HarnessCapability>,
    pub fresh_sessions_only: bool,
}

impl HarnessProfile {
    pub fn from_json(bytes: &[u8]) -> Result<Self, HarnessFailure> {
        let mut raw: RawHarnessProfile = serde_json::from_slice(bytes)
            .map_err(|e| HarnessFailure::new("harness_profile_unavailable", e.to_string()))?;
        let id = HarnessProfileId::new(std::mem::take(&mut raw.id))?;
        let required = unique_capabilities(std::mem::take(&mut raw.required), "required")?;
        let optional = unique_capabilities(std::mem::take(&mut raw.optional), "optional")?;
        if !required.is_disjoint(&optional) {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "required and optional capabilities overlap",
            ));
        }
        if raw.source_revision.len() != 40
            || !raw.source_revision.bytes().all(|b| b.is_ascii_hexdigit())
            || raw.source_revision.bytes().all(|b| b == b'0')
        {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "source revision must be a nonzero full commit hash",
            ));
        }
        let expected_revision = match id.as_str() {
            "codex-acp-v1" => Some("296069e841634cd4bb9bc4515602d836e49231ec"),
            "deepseek-harness-acp-v1" => Some("141eb6fef83422698aef7a981029e843e8161534"),
            _ => None,
        };
        if expected_revision.is_some_and(|expected| expected != raw.source_revision) {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "source revision does not match the pinned profile",
            ));
        }
        validate_pinned_profile(id.as_str(), &raw, &required, &optional)?;
        if raw.program.is_empty() || raw.source_repository.is_empty() {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "profile command and source repository are required",
            ));
        }
        Ok(Self {
            id,
            transport: raw.transport,
            source_repository: raw.source_repository,
            source_revision: raw.source_revision,
            program: raw.program,
            args: raw.args,
            required,
            optional,
            fresh_sessions_only: raw.fresh_sessions_only,
        })
    }

    pub fn validate_observed(&self, observed: &HarnessCapabilities) -> Result<(), HarnessFailure> {
        if observed.protocol_version != HARNESS_PROTOCOL_VERSION {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                format!(
                    "expected {}, observed {}",
                    HARNESS_PROTOCOL_VERSION, observed.protocol_version
                ),
            ));
        }
        if !self.required.is_subset(&observed.supported) {
            return Err(HarnessFailure::new(
                "harness_capability_unsupported",
                "required portable ACP capability is absent",
            ));
        }
        Ok(())
    }

    pub fn require(&self, capability: HarnessCapability) -> Result<(), HarnessFailure> {
        if self.required.contains(&capability) {
            Ok(())
        } else {
            Err(HarnessFailure::new(
                "harness_capability_unsupported",
                format!(
                    "capability {:?} requires observed handshake support",
                    capability
                ),
            ))
        }
    }

    /// Require a declared capability and the same capability in the observed handshake.
    pub fn require_observed(
        &self,
        capability: HarnessCapability,
        observed: &HarnessCapabilities,
    ) -> Result<(), HarnessFailure> {
        if !self.required.contains(&capability) && !self.optional.contains(&capability) {
            return Err(HarnessFailure::new(
                "harness_capability_unsupported",
                format!("capability {:?} is not declared by profile", capability),
            ));
        }
        if observed.protocol_version != HARNESS_PROTOCOL_VERSION {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                format!(
                    "expected {}, observed {}",
                    HARNESS_PROTOCOL_VERSION, observed.protocol_version
                ),
            ));
        }
        if !observed.supported.contains(&capability) {
            return Err(HarnessFailure::new(
                "harness_capability_unsupported",
                format!(
                    "capability {:?} is absent from observed handshake",
                    capability
                ),
            ));
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, HarnessFailure> {
        canonical_sha256(self)
            .map_err(|e| HarnessFailure::new("harness_profile_unavailable", e.to_string()))
    }
}

fn unique_capabilities(
    values: Vec<HarnessCapability>,
    field: &str,
) -> Result<BTreeSet<HarnessCapability>, HarnessFailure> {
    let set: BTreeSet<_> = values.iter().copied().collect();
    if set.len() != values.len() {
        return Err(HarnessFailure::new(
            "harness_profile_unavailable",
            format!("duplicate capability in {field}"),
        ));
    }
    Ok(set)
}

fn validate_pinned_profile(
    id: &str,
    profile: &RawHarnessProfile,
    required: &BTreeSet<HarnessCapability>,
    optional: &BTreeSet<HarnessCapability>,
) -> Result<(), HarnessFailure> {
    let core = BTreeSet::from([
        HarnessCapability::FreshSession,
        HarnessCapability::TextPrompt,
        HarnessCapability::PermissionOnce,
        HarnessCapability::Cancel,
        HarnessCapability::CommittedOutput,
    ]);
    let (
        expected_transport,
        expected_repo,
        expected_program,
        expected_args,
        expected_optional,
        expected_fresh,
    ) = match id {
        "codex-acp-v1" => (
            HarnessTransport::AcpStdio,
            "https://github.com/zed-industries/codex-acp.git",
            "codex-acp",
            Vec::<String>::new(),
            BTreeSet::from([
                HarnessCapability::SessionLoad,
                HarnessCapability::EmbeddedContext,
                HarnessCapability::ImagePrompt,
                HarnessCapability::McpStdio,
                HarnessCapability::McpHttp,
                HarnessCapability::ToolPresentation,
            ]),
            false,
        ),
        "deepseek-harness-acp-v1" => (
            HarnessTransport::AcpStdio,
            "https://github.com/deepseek-ai/deepseek-harness.git",
            "pnpm",
            vec!["--dir", "vendor/deepseek-harness", "run", "demo:acp"]
                .into_iter()
                .map(String::from)
                .collect(),
            BTreeSet::new(),
            true,
        ),
        _ => return Ok(()),
    };
    if profile.transport != expected_transport
        || profile.source_repository != expected_repo
        || profile.program != expected_program
        || profile.args != expected_args
        || required != &core
        || optional != &expected_optional
        || profile.fresh_sessions_only != expected_fresh
    {
        return Err(HarnessFailure::new(
            "harness_profile_unavailable",
            "profile fields do not match the pinned V1 contract",
        ));
    }
    Ok(())
}

#[allow(dead_code)]
fn _model_role_is_linked(_: ModelRole) {}
