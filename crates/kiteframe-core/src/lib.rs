#![forbid(unsafe_code)]

mod discover;
mod load;
mod path;
mod yaml;

pub use load::{AgentPackage, load_package, load_runtime_binding};
pub use path::CanonicalPackageRoot;
pub use yaml::{PackageLimits, parse_binding, parse_manifest};
