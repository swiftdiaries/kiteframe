use std::collections::BTreeMap;

use kiteframe_contract::{AgentManifest, Diagnostic, PackagePath};

use crate::path::{containment, package_invalid};

#[derive(Default)]
pub(crate) struct PathCollisionTracker {
    paths: BTreeMap<String, TrackedPath>,
}

struct TrackedPath {
    path: PackagePath,
    package_prefix: String,
}

impl PathCollisionTracker {
    pub(crate) fn for_root() -> Self {
        let manifest_path = PackagePath::new("agent.yaml").expect("static package path");
        Self {
            paths: BTreeMap::from([(
                manifest_path.as_str().to_owned(),
                TrackedPath {
                    path: manifest_path,
                    package_prefix: String::new(),
                },
            )]),
        }
    }

    fn register(
        &mut self,
        package_prefix: &str,
        local_path: &PackagePath,
    ) -> Result<(), Diagnostic> {
        let full_path = top_package_relative(package_prefix, local_path);
        let collision_key = full_path.as_str().to_lowercase();
        if let Some(existing) = self.paths.get(&collision_key)
            && (existing.path != full_path || existing.package_prefix != package_prefix)
        {
            return Err(containment(
                &full_path,
                "referenced paths collide across the package tree",
            ));
        }
        self.paths.insert(
            collision_key,
            TrackedPath {
                path: full_path,
                package_prefix: package_prefix.to_owned(),
            },
        );
        Ok(())
    }
}

pub(crate) struct PortableReferences {
    pub(crate) prompt: PackagePath,
    pub(crate) skills: Vec<PackagePath>,
    pub(crate) subagents: Vec<PackagePath>,
}

pub(crate) fn discover_portable_references(
    manifest: &AgentManifest,
    package_prefix: &str,
    collision_tracker: &mut PathCollisionTracker,
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

    for path in std::iter::once(&prompt)
        .chain(skills.iter())
        .chain(subagents.iter())
    {
        collision_tracker.register(package_prefix, path)?;
    }

    Ok(PortableReferences {
        prompt,
        skills,
        subagents,
    })
}

pub(crate) fn top_package_relative(package_prefix: &str, local_path: &PackagePath) -> PackagePath {
    if package_prefix.is_empty() {
        return local_path.clone();
    }
    PackagePath::new(format!("{package_prefix}/{}", local_path.as_str()))
        .expect("validated package prefix joined to a validated local path")
}
