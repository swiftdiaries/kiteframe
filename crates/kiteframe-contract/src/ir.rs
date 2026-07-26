use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use sha2::{Digest, Sha256};

use crate::{
    CapabilityIdentity, CompilationReport, DataClassification, DelegationRequirement, FeatureSet,
    ModelRequirement, ModelRole, PackageIdentity, PackagePath, RegistrySymbol, Sha256Digest,
    ValidatedTextAsset,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IrSchemaVersion {
    #[serde(rename = "kiteframe.dev/ir/v1alpha1")]
    V1Alpha1,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedModelRequirement {
    requirement: ModelRequirement,
    symbol: RegistrySymbol,
}
impl ResolvedModelRequirement {
    pub fn new(requirement: ModelRequirement, symbol: RegistrySymbol) -> Self {
        Self {
            requirement,
            symbol,
        }
    }
    pub fn requirement(&self) -> &ModelRequirement {
        &self.requirement
    }
    pub fn symbol(&self) -> &RegistrySymbol {
        &self.symbol
    }
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
        for requirement in &mut parts.capability_requirements {
            requirement.resources.sort();
            requirement.resources.dedup();
        }
        parts.capability_requirements.sort_by(|a, b| {
            a.identity
                .cmp(&b.identity)
                .then(a.resources.cmp(&b.resources))
                .then(a.required.cmp(&b.required))
        });
        parts.capability_requirements.dedup();
        parts.subagents.sort_by(|a, b| {
            a.package_identity
                .name
                .cmp(&b.package_identity.name)
                .then(a.package_identity.version.cmp(&b.package_identity.version))
        });
        if parts
            .subagents
            .windows(2)
            .any(|pair| pair[0].package_identity == pair[1].package_identity)
        {
            return Err("resolved subagent identities must be unique".to_owned());
        }
        parts.content_capture.classifications.sort();
        parts.content_capture.classifications.dedup();
        parts.compilation_report = parts.compilation_report.normalized();
        let resolved_digest = resolved_digest(&parts)?;
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
    pub fn models(&self) -> &BTreeMap<ModelRole, ResolvedModelRequirement> {
        &self.models
    }
    pub fn subagents(&self) -> &[ResolvedSubagent] {
        &self.subagents
    }
    pub fn required_features(&self) -> &FeatureSet {
        &self.required_features
    }
    pub fn optional_features(&self) -> &FeatureSet {
        &self.optional_features
    }
    pub fn content_capture(&self) -> &ResolvedContentCaptureRequirement {
        &self.content_capture
    }
    pub fn compilation_report(&self) -> &CompilationReport {
        &self.compilation_report
    }
    pub fn binding_digest(&self) -> &Sha256Digest {
        &self.binding_digest
    }
}

fn resolved_digest(parts: &ResolvedAgentParts) -> Result<Sha256Digest, String> {
    let identity = canonical_component(
        b"resolved/identity",
        &(parts.schema_version, &parts.package_identity),
    )?;
    let portable = hash_domain(
        b"resolved/portable",
        [parts.portable_digest.as_bytes().as_slice()],
    );
    let lock = hash_domain(b"resolved/lock", [parts.lock_digest.as_bytes().as_slice()]);
    let binding = hash_domain(
        b"resolved/binding",
        [parts.binding_digest.as_bytes().as_slice()],
    );
    let prompts = canonical_component(b"resolved/prompts", &parts.prompts)?;
    let skills = canonical_component(b"resolved/skills", &parts.skills)?;
    let features = canonical_component(
        b"resolved/features",
        &(&parts.required_features, &parts.optional_features),
    )?;
    let models = canonical_component(b"resolved/models", &parts.models)?;
    let capabilities =
        canonical_component(b"resolved/capabilities", &parts.capability_requirements)?;
    let children = canonical_component(b"resolved/children", &parts.subagents)?;
    let content_capture = canonical_component(b"resolved/content-capture", &parts.content_capture)?;
    let report = canonical_component(b"resolved/report", &parts.compilation_report)?;
    let components = [
        identity,
        portable,
        lock,
        binding,
        prompts,
        skills,
        features,
        models,
        capabilities,
        children,
        content_capture,
        report,
    ];

    Ok(hash_domain(
        b"resolved-agent",
        components
            .iter()
            .map(|component| component.as_bytes().as_slice()),
    ))
}

fn canonical_component<T: Serialize>(
    domain: &'static [u8],
    value: &T,
) -> Result<Sha256Digest, String> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| "resolved component cannot be canonicalized".to_owned())?;
    Ok(hash_domain(domain, [bytes.as_slice()]))
}

fn hash_domain<'a>(
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
