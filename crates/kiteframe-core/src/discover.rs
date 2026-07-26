use std::collections::BTreeMap;

use kiteframe_contract::{AgentManifest, Diagnostic, PackagePath};

use crate::path::{containment, package_invalid};

pub(crate) struct PortableReferences {
    pub(crate) prompt: PackagePath,
    pub(crate) skills: Vec<PackagePath>,
    pub(crate) subagents: Vec<PackagePath>,
}

pub(crate) fn discover_portable_references(
    manifest: &AgentManifest,
) -> Result<PortableReferences, Diagnostic> {
    let prompt = manifest.spec.prompt.system.clone();
    let skills = manifest.spec.skills.clone();
    let subagents = manifest
        .spec
        .delegation
        .iter()
        .map(|delegation| delegation.agent.clone())
        .collect::<Vec<_>>();
    for path in &subagents {
        if path.as_str().rsplit('/').next() != Some("agent.yaml") {
            return Err(package_invalid(
                Some(path),
                "nested agent manifest must be named agent.yaml",
            ));
        }
    }

    let manifest_path = PackagePath::new("agent.yaml").expect("static package path");
    let mut case_insensitive_paths =
        BTreeMap::from([(manifest_path.as_str().to_owned(), manifest_path)]);
    for path in std::iter::once(&prompt)
        .chain(skills.iter())
        .chain(subagents.iter())
    {
        let collision_key = path.as_str().to_lowercase();
        if let Some(existing) = case_insensitive_paths.get(&collision_key)
            && existing != path
        {
            return Err(containment(
                path,
                "referenced paths collide when compared case-insensitively",
            ));
        }
        case_insensitive_paths.insert(collision_key, path.clone());
    }

    Ok(PortableReferences {
        prompt,
        skills,
        subagents,
    })
}
