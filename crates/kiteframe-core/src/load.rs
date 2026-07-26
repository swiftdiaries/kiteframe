use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use kiteframe_contract::{
    AgentManifest, Diagnostic, PackagePath, RuntimeBinding, ValidatedTextAsset,
};

use crate::{
    PackageLimits,
    discover::discover_portable_references,
    parse_binding, parse_manifest,
    path::{
        ByteBudget, CanonicalPackageRoot, open_referenced_text, open_referenced_yaml,
        package_invalid,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPackage {
    pub root: CanonicalPackageRoot,
    pub manifest: AgentManifest,
    pub prompt_assets: BTreeMap<PackagePath, ValidatedTextAsset>,
    pub skill_assets: BTreeMap<PackagePath, ValidatedTextAsset>,
    pub subagents: BTreeMap<PackagePath, AgentPackage>,
}

pub fn load_package(root: &Path, limits: PackageLimits) -> Result<AgentPackage, Vec<Diagnostic>> {
    let root = CanonicalPackageRoot::open(root).map_err(single)?;
    let mut budget = ByteBudget::new(limits.max_total_referenced_bytes);
    let manifest_path = PackagePath::new("agent.yaml").expect("static package path");
    let manifest_bytes =
        open_referenced_yaml(&root, &manifest_path, limits.max_yaml_bytes, &mut budget)
            .map_err(single)?;
    let manifest = parse_manifest(&manifest_bytes.bytes, limits)?;
    let mut identities = IdentityTracker::default();
    load_parsed_package(root, manifest, 0, limits, &mut budget, &mut identities)
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
    depth: usize,
    limits: PackageLimits,
    budget: &mut ByteBudget,
    identities: &mut IdentityTracker,
) -> Result<AgentPackage, Vec<Diagnostic>> {
    let identity = Identity::from_manifest(&manifest);
    identities.enter(&identity)?;

    let result = assemble_package(root, manifest, depth, limits, budget, identities);
    identities.leave(&identity);
    result
}

fn assemble_package(
    root: CanonicalPackageRoot,
    manifest: AgentManifest,
    depth: usize,
    limits: PackageLimits,
    budget: &mut ByteBudget,
    identities: &mut IdentityTracker,
) -> Result<AgentPackage, Vec<Diagnostic>> {
    let references = discover_portable_references(&manifest).map_err(single)?;

    let prompt = open_referenced_text(
        &root,
        &references.prompt,
        limits.max_text_asset_bytes,
        budget,
    )
    .map_err(single)?;
    let mut prompt_assets = BTreeMap::new();
    prompt_assets.insert(references.prompt, prompt);

    let mut skill_assets = BTreeMap::new();
    for path in references.skills {
        let asset = open_referenced_text(&root, &path, limits.max_text_asset_bytes, budget)
            .map_err(single)?;
        skill_assets.insert(path, asset);
    }

    let mut subagents = BTreeMap::new();
    for path in references.subagents {
        let child_depth = depth.saturating_add(1);
        if child_depth > limits.max_subagent_depth {
            return Err(single(package_invalid(
                Some(&path),
                "nested agent nesting limit exceeded",
            )));
        }
        let referenced =
            open_referenced_yaml(&root, &path, limits.max_yaml_bytes, budget).map_err(single)?;
        let child_manifest = parse_manifest(&referenced.bytes, limits)?;
        let child_root_path = referenced
            .canonical_path
            .parent()
            .expect("a referenced manifest always has a parent");
        let child_root = CanonicalPackageRoot::open(child_root_path).map_err(single)?;
        let child = load_parsed_package(
            child_root,
            child_manifest,
            child_depth,
            limits,
            budget,
            identities,
        )?;
        subagents.insert(path, child);
    }

    Ok(AgentPackage {
        root,
        manifest,
        prompt_assets,
        skill_assets,
        subagents,
    })
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
