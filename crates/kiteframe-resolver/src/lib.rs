#![forbid(unsafe_code)]

mod catalog;
mod descriptor;

pub use catalog::{
    CandidatePolicy, SelectedCapability, SelectionOutcome, ValidatedCatalog, select_capabilities,
    select_capabilities_with_warnings, validate_catalog,
};
