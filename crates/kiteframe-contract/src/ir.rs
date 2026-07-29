use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use sha2::{Digest, Sha256};

use crate::{
    CapabilityDescriptor, CapabilityIdentity, CatalogIdentity, CompilationReport,
    DataClassification, DelegationRequirement, FeatureSet, LockedCapability, ModelRequirement,
    ModelRole, PackageIdentity, PackagePath, RegistrySymbol, Sha256Digest, ValidatedTextAsset,
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCapabilityRequirement {
    locked_capability: LockedCapability,
    required: bool,
    #[schemars(length(min = 1))]
    resources: Vec<String>,
}
impl ResolvedCapabilityRequirement {
    pub fn try_new(
        locked_capability: LockedCapability,
        required: bool,
        mut resources: Vec<String>,
    ) -> Result<Self, String> {
        resources.sort();
        resources.dedup();
        if resources.is_empty() {
            return Err("resolved capability requires at least one resource selector".to_owned());
        }
        Ok(Self {
            locked_capability,
            required,
            resources,
        })
    }
    pub fn identity(&self) -> &CapabilityIdentity {
        self.locked_capability.identity()
    }
    pub fn locked_capability(&self) -> &LockedCapability {
        &self.locked_capability
    }
    pub fn descriptor(&self) -> &CapabilityDescriptor {
        self.locked_capability.descriptor()
    }
    pub fn descriptor_digest(&self) -> &Sha256Digest {
        self.locked_capability.descriptor_digest()
    }
    pub fn input_schema_digest(&self) -> &Sha256Digest {
        self.locked_capability.input_schema_digest()
    }
    pub fn output_schema_digest(&self) -> &Sha256Digest {
        self.locked_capability.output_schema_digest()
    }
    pub fn stable_error_set_digest(&self) -> &Sha256Digest {
        self.locked_capability.stable_error_set_digest()
    }
    pub fn safety_metadata_digest(&self) -> &Sha256Digest {
        self.locked_capability.safety_metadata_digest()
    }
    pub fn required(&self) -> bool {
        self.required
    }
    pub fn resources(&self) -> &[String] {
        &self.resources
    }
}
impl<'de> Deserialize<'de> for ResolvedCapabilityRequirement {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            locked_capability: LockedCapability,
            required: bool,
            resources: Vec<String>,
        }
        let raw = Raw::deserialize(d)?;
        Self::try_new(raw.locked_capability, raw.required, raw.resources).map_err(D::Error::custom)
    }
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
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
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
    catalog_identity: CatalogIdentity,
    catalog_digest: Sha256Digest,
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
        parts.capability_requirements.sort_by(|a, b| {
            a.identity()
                .cmp(b.identity())
                .then(a.resources().cmp(b.resources()))
                .then(a.required().cmp(&b.required()))
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
            catalog_identity: parts.catalog_identity,
            catalog_digest: parts.catalog_digest,
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
    pub fn portable_digest(&self) -> &Sha256Digest {
        &self.portable_digest
    }
    pub fn package_identity(&self) -> &PackageIdentity {
        &self.package_identity
    }
    pub fn catalog_identity(&self) -> &CatalogIdentity {
        &self.catalog_identity
    }
    pub fn catalog_digest(&self) -> &Sha256Digest {
        &self.catalog_digest
    }
    pub fn capability_requirements(&self) -> &[ResolvedCapabilityRequirement] {
        &self.capability_requirements
    }
    pub fn models(&self) -> &BTreeMap<ModelRole, ResolvedModelRequirement> {
        &self.models
    }
    pub fn prompts(&self) -> &BTreeMap<PackagePath, ValidatedTextAsset> {
        &self.prompts
    }
    pub fn skills(&self) -> &BTreeMap<PackagePath, ValidatedTextAsset> {
        &self.skills
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
    pub fn lock_digest(&self) -> &Sha256Digest {
        &self.lock_digest
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
    let catalog = canonical_component(
        b"resolved/catalog",
        &(&parts.catalog_identity, &parts.catalog_digest),
    )?;
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
        catalog,
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
            catalog_identity: CatalogIdentity,
            catalog_digest: Sha256Digest,
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
            catalog_identity: raw.catalog_identity,
            catalog_digest: raw.catalog_digest,
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
