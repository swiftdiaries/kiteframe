use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use kiteframe_contract::{
    AgentManifest, Diagnostic, PackagePath, RuntimeBinding, Sha256Digest, ValidatedTextAsset,
};

use crate::{
    PackageLimits,
    canonical::portable_digest,
    discover::{PathCollisionTracker, discover_portable_references, top_package_relative},
    parse_binding, parse_manifest,
    path::{
        ByteBudget, CanonicalPackageRoot, open_referenced_text, open_referenced_yaml,
        package_invalid,
    },
};

/// A package whose content and portable digest were produced by validation.
///
/// Validated content is intentionally read-only outside this crate.
///
/// ```compile_fail
/// use kiteframe_contract::Sha256Digest;
/// use kiteframe_core::AgentPackage;
///
/// fn overwrite_digest(package: &mut AgentPackage, digest: Sha256Digest) {
///     package.portable_digest = digest;
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPackage {
    root: CanonicalPackageRoot,
    manifest: AgentManifest,
    prompt_assets: BTreeMap<PackagePath, ValidatedTextAsset>,
    skill_assets: BTreeMap<PackagePath, ValidatedTextAsset>,
    subagents: BTreeMap<PackagePath, AgentPackage>,
    portable_digest: Sha256Digest,
}

impl AgentPackage {
    fn from_validated_parts(
        root: CanonicalPackageRoot,
        manifest: AgentManifest,
        prompt_assets: BTreeMap<PackagePath, ValidatedTextAsset>,
        skill_assets: BTreeMap<PackagePath, ValidatedTextAsset>,
        subagents: BTreeMap<PackagePath, AgentPackage>,
    ) -> Result<Self, Diagnostic> {
        let portable_digest =
            portable_digest(&manifest, &prompt_assets, &skill_assets, &subagents)?;
        Ok(Self {
            root,
            manifest,
            prompt_assets,
            skill_assets,
            subagents,
            portable_digest,
        })
    }

    pub fn root(&self) -> &CanonicalPackageRoot {
        &self.root
    }

    pub fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    pub fn prompt_assets(&self) -> &BTreeMap<PackagePath, ValidatedTextAsset> {
        &self.prompt_assets
    }

    pub fn skill_assets(&self) -> &BTreeMap<PackagePath, ValidatedTextAsset> {
        &self.skill_assets
    }

    pub fn subagents(&self) -> &BTreeMap<PackagePath, AgentPackage> {
        &self.subagents
    }

    pub fn portable_digest(&self) -> &Sha256Digest {
        &self.portable_digest
    }
}

pub fn load_package(root: &Path, limits: PackageLimits) -> Result<AgentPackage, Vec<Diagnostic>> {
    let root = CanonicalPackageRoot::open(root).map_err(single)?;
    let mut context = LoadContext::new(limits);
    let manifest_path = PackagePath::new("agent.yaml").expect("static package path");
    let manifest_bytes = open_referenced_yaml(
        &root,
        &manifest_path,
        limits.max_yaml_bytes,
        &mut context.budget,
    )
    .map_err(single)?;
    let manifest = parse_manifest(&manifest_bytes.bytes, limits)?;
    load_parsed_package(root, manifest, "", 0, &mut context)
}

pub fn load_runtime_binding(
    root: &Path,
    selected: &PackagePath,
    limits: PackageLimits,
) -> Result<RuntimeBinding, Vec<Diagnostic>> {
    let root = CanonicalPackageRoot::open(root).map_err(single)?;
    let mut budget = ByteBudget::new(limits.max_total_referenced_bytes);
    let binding_bytes = open_referenced_yaml(&root, selected, limits.max_yaml_bytes, &mut budget)
        .map_err(single)?;
    parse_binding(&binding_bytes.bytes, limits)
}

fn load_parsed_package(
    root: CanonicalPackageRoot,
    manifest: AgentManifest,
    package_prefix: &str,
    depth: usize,
    context: &mut LoadContext,
) -> Result<AgentPackage, Vec<Diagnostic>> {
    let identity = Identity::from_manifest(&manifest);
    context.identities.enter(&identity)?;

    let result = assemble_package(root, manifest, package_prefix, depth, context);
    context.identities.leave(&identity);
    result
}

fn assemble_package(
    root: CanonicalPackageRoot,
    manifest: AgentManifest,
    package_prefix: &str,
    depth: usize,
    context: &mut LoadContext,
) -> Result<AgentPackage, Vec<Diagnostic>> {
    let references =
        discover_portable_references(&manifest, package_prefix, &mut context.collision_tracker)
            .map_err(single)?;

    let prompt = open_referenced_text(
        &root,
        &references.prompt,
        context.limits.max_text_asset_bytes,
        &mut context.budget,
    )
    .map_err(single)?;
    let mut prompt_assets = BTreeMap::new();
    prompt_assets.insert(references.prompt, prompt);

    let mut skill_assets = BTreeMap::new();
    for path in references.skills {
        let asset = open_referenced_text(
            &root,
            &path,
            context.limits.max_text_asset_bytes,
            &mut context.budget,
        )
        .map_err(single)?;
        skill_assets.insert(path, asset);
    }

    let mut subagents = BTreeMap::new();
    for path in references.subagents {
        let child_depth = depth.saturating_add(1);
        if child_depth > context.limits.max_subagent_depth {
            return Err(single(package_invalid(
                Some(&path),
                "nested agent nesting limit exceeded",
            )));
        }
        let referenced = open_referenced_yaml(
            &root,
            &path,
            context.limits.max_yaml_bytes,
            &mut context.budget,
        )
        .map_err(single)?;
        let child_manifest = parse_manifest(&referenced.bytes, context.limits)?;
        let child_root_path = referenced
            .canonical_path
            .parent()
            .expect("a referenced manifest always has a parent");
        let child_root = CanonicalPackageRoot::open(child_root_path).map_err(single)?;
        let child_manifest_path = top_package_relative(package_prefix, &path);
        let child_package_prefix = child_manifest_path
            .as_str()
            .rsplit_once('/')
            .map_or("", |(prefix, _)| prefix);
        let child = load_parsed_package(
            child_root,
            child_manifest,
            child_package_prefix,
            child_depth,
            context,
        )?;
        subagents.insert(path, child);
    }

    AgentPackage::from_validated_parts(root, manifest, prompt_assets, skill_assets, subagents)
        .map_err(single)
}

struct LoadContext {
    limits: PackageLimits,
    budget: ByteBudget,
    identities: IdentityTracker,
    collision_tracker: PathCollisionTracker,
}

impl LoadContext {
    fn new(limits: PackageLimits) -> Self {
        Self {
            limits,
            budget: ByteBudget::new(limits.max_total_referenced_bytes),
            identities: IdentityTracker::default(),
            collision_tracker: PathCollisionTracker::for_root(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Identity {
    name: String,
    version: String,
}

impl Identity {
    fn from_manifest(manifest: &AgentManifest) -> Self {
        Self {
            name: manifest.metadata.name.as_str().to_owned(),
            version: manifest.metadata.version.as_str().to_owned(),
        }
    }
}

#[derive(Default)]
struct IdentityTracker {
    active: BTreeSet<Identity>,
    seen: BTreeSet<Identity>,
}

impl IdentityTracker {
    fn enter(&mut self, identity: &Identity) -> Result<(), Vec<Diagnostic>> {
        if self.active.contains(identity) {
            return Err(single(package_invalid(
                None,
                "nested agent identity cycle detected",
            )));
        }
        if self.seen.contains(identity) {
            return Err(single(package_invalid(
                None,
                "duplicate nested agent identity detected",
            )));
        }
        self.active.insert(identity.clone());
        self.seen.insert(identity.clone());
        Ok(())
    }

    fn leave(&mut self, identity: &Identity) {
        self.active.remove(identity);
    }
}

fn single(diagnostic: Diagnostic) -> Vec<Diagnostic> {
    vec![diagnostic]
}
