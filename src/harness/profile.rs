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
        let raw: RawHarnessProfile = serde_json::from_slice(bytes)
            .map_err(|e| HarnessFailure::new("harness_profile_unavailable", e.to_string()))?;
        let id = HarnessProfileId::new(raw.id)?;
        let required = unique_capabilities(raw.required, "required")?;
        let optional = unique_capabilities(raw.optional, "optional")?;
        if !required.is_disjoint(&optional) {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "required and optional capabilities overlap",
            ));
        }
        if raw.source_revision.len() != 40
            || !raw.source_revision.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "source revision must be a full commit hash",
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
        if self.required.contains(&capability) || self.optional.contains(&capability) {
            Ok(())
        } else {
            Err(HarnessFailure::new(
                "harness_capability_unsupported",
                format!("capability {:?} is not declared by profile", capability),
            ))
        }
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

#[allow(dead_code)]
fn _model_role_is_linked(_: ModelRole) {}
