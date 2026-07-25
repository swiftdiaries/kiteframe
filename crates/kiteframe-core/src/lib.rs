#![forbid(unsafe_code)]

mod yaml;

pub use yaml::{PackageLimits, parse_binding, parse_manifest};
