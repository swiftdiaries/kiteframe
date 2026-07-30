#![forbid(unsafe_code)]

mod admission;
mod authority;

pub use admission::{
    AdmissionService, AdmissionServiceConfig, AuthoritySource, PersistedAdmission,
};
pub use authority::{AuthorityTerm, EffectiveGrantSubset, intersect_authority};
