#![forbid(unsafe_code)]

mod canonical;
mod discover;
mod load;
mod path;
mod yaml;

pub use canonical::{canonical_json, hash_domain};
pub use load::{AgentPackage, load_package, load_runtime_binding};
pub use path::CanonicalPackageRoot;
pub use yaml::{PackageLimits, parse_binding, parse_manifest};
