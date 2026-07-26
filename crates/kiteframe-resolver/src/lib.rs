#![forbid(unsafe_code)]

mod catalog;
mod descriptor;

#[allow(deprecated)]
pub use catalog::select_capabilities;
pub use catalog::{
    CandidatePolicy, SelectedCapability, SelectionOutcome, ValidatedCatalog,
    select_capabilities_with_warnings, validate_catalog,
};
pub use descriptor::ValidatedDescriptor;
