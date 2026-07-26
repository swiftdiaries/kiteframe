use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{
    CapabilityIdentity, CompilationReport, DataClassification, DelegationRequirement, FeatureSet,
    ModelRequirement, ModelRole, PackageIdentity, PackagePath, Sha256Digest, ValidatedTextAsset,
    capability::digest,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IrSchemaVersion {
    #[serde(rename = "kiteframe.dev/ir/v1alpha1")]
    V1Alpha1,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedModelRequirement {
    pub requirement: ModelRequirement,
    pub symbol: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCapabilityRequirement {
    pub identity: CapabilityIdentity,
    pub required: bool,
    pub resources: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedSubagent {
    pub package_identity: PackageIdentity,
    pub delegation: DelegationRequirement,
    pub resolved_digest: Sha256Digest,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedContentCaptureRequirement {
    pub allowed: bool,
    #[serde(default)]
    pub classifications: Vec<DataClassification>,
}
#[derive(Clone, Debug)]
pub struct ResolvedAgentParts {
    pub schema_version: IrSchemaVersion,
    pub package_identity: PackageIdentity,
    pub portable_digest: Sha256Digest,
    pub lock_digest: Sha256Digest,
    pub binding_digest: Sha256Digest,
    pub prompts: BTreeMap<PackagePath, ValidatedTextAsset>,
    pub skills: BTreeMap<PackagePath, ValidatedTextAsset>,
    pub models: BTreeMap<ModelRole, ResolvedModelRequirement>,
    pub capability_requirements: Vec<ResolvedCapabilityRequirement>,
    pub subagents: Vec<ResolvedSubagent>,
    pub required_features: FeatureSet,
    pub optional_features: FeatureSet,
    pub content_capture: ResolvedContentCaptureRequirement,
    pub compilation_report: CompilationReport,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedAgent {
    schema_version: IrSchemaVersion,
    package_identity: PackageIdentity,
    portable_digest: Sha256Digest,
    lock_digest: Sha256Digest,
    binding_digest: Sha256Digest,
    resolved_digest: Sha256Digest,
    prompts: BTreeMap<PackagePath, ValidatedTextAsset>,
    skills: BTreeMap<PackagePath, ValidatedTextAsset>,
    models: BTreeMap<ModelRole, ResolvedModelRequirement>,
    capability_requirements: Vec<ResolvedCapabilityRequirement>,
    subagents: Vec<ResolvedSubagent>,
    required_features: FeatureSet,
    optional_features: FeatureSet,
    content_capture: ResolvedContentCaptureRequirement,
    compilation_report: CompilationReport,
}
impl ResolvedAgent {
    pub fn try_new(mut parts: ResolvedAgentParts) -> Result<Self, String> {
        parts
            .capability_requirements
            .sort_by(|a, b| a.identity.cmp(&b.identity));
        parts.capability_requirements.dedup();
        parts
            .subagents
            .sort_by(|a, b| a.package_identity.name.cmp(&b.package_identity.name));
        parts
            .subagents
            .dedup_by(|a, b| a.package_identity == b.package_identity);
        parts.content_capture.classifications.sort();
        parts.content_capture.classifications.dedup();
        let digest_value = ResolvedWire {
            schema_version: parts.schema_version,
            package_identity: parts.package_identity.clone(),
            portable_digest: parts.portable_digest,
            lock_digest: parts.lock_digest,
            binding_digest: parts.binding_digest,
            prompts: &parts.prompts,
            skills: &parts.skills,
            models: &parts.models,
            capability_requirements: &parts.capability_requirements,
            subagents: &parts.subagents,
            required_features: &parts.required_features,
            optional_features: &parts.optional_features,
            content_capture: &parts.content_capture,
            compilation_report: &parts.compilation_report,
        };
        let resolved_digest = digest(&digest_value)?;
        Ok(Self {
            schema_version: parts.schema_version,
            package_identity: parts.package_identity,
            portable_digest: parts.portable_digest,
            lock_digest: parts.lock_digest,
            binding_digest: parts.binding_digest,
            resolved_digest,
            prompts: parts.prompts,
            skills: parts.skills,
            models: parts.models,
            capability_requirements: parts.capability_requirements,
            subagents: parts.subagents,
            required_features: parts.required_features,
            optional_features: parts.optional_features,
            content_capture: parts.content_capture,
            compilation_report: parts.compilation_report,
        })
    }
    pub fn resolved_digest(&self) -> &Sha256Digest {
        &self.resolved_digest
    }
    pub fn package_identity(&self) -> &PackageIdentity {
        &self.package_identity
    }
    pub fn capability_requirements(&self) -> &[ResolvedCapabilityRequirement] {
        &self.capability_requirements
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolvedWire<'a> {
    schema_version: IrSchemaVersion,
    package_identity: PackageIdentity,
    portable_digest: Sha256Digest,
    lock_digest: Sha256Digest,
    binding_digest: Sha256Digest,
    prompts: &'a BTreeMap<PackagePath, ValidatedTextAsset>,
    skills: &'a BTreeMap<PackagePath, ValidatedTextAsset>,
    models: &'a BTreeMap<ModelRole, ResolvedModelRequirement>,
    capability_requirements: &'a [ResolvedCapabilityRequirement],
    subagents: &'a [ResolvedSubagent],
    required_features: &'a FeatureSet,
    optional_features: &'a FeatureSet,
    content_capture: &'a ResolvedContentCaptureRequirement,
    compilation_report: &'a CompilationReport,
}
impl<'de> Deserialize<'de> for ResolvedAgent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            schema_version: IrSchemaVersion,
            package_identity: PackageIdentity,
            portable_digest: Sha256Digest,
            lock_digest: Sha256Digest,
            binding_digest: Sha256Digest,
            resolved_digest: Sha256Digest,
            prompts: BTreeMap<PackagePath, ValidatedTextAsset>,
            skills: BTreeMap<PackagePath, ValidatedTextAsset>,
            models: BTreeMap<ModelRole, ResolvedModelRequirement>,
            capability_requirements: Vec<ResolvedCapabilityRequirement>,
            subagents: Vec<ResolvedSubagent>,
            required_features: FeatureSet,
            optional_features: FeatureSet,
            content_capture: ResolvedContentCaptureRequirement,
            compilation_report: CompilationReport,
        }
        let raw = Raw::deserialize(d)?;
        let value = Self::try_new(ResolvedAgentParts {
            schema_version: raw.schema_version,
            package_identity: raw.package_identity,
            portable_digest: raw.portable_digest,
            lock_digest: raw.lock_digest,
            binding_digest: raw.binding_digest,
            prompts: raw.prompts,
            skills: raw.skills,
            models: raw.models,
            capability_requirements: raw.capability_requirements,
            subagents: raw.subagents,
            required_features: raw.required_features,
            optional_features: raw.optional_features,
            content_capture: raw.content_capture,
            compilation_report: raw.compilation_report,
        })
        .map_err(D::Error::custom)?;
        if value.resolved_digest != raw.resolved_digest {
            return Err(D::Error::custom(
                "resolved digest does not match canonical IR",
            ));
        }
        Ok(value)
    }
}
