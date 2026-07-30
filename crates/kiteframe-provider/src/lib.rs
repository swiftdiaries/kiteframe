#![forbid(unsafe_code)]

mod admission;
mod authority;

pub use admission::{
    AdmissionService, AdmissionServiceConfig, AuthorityDomain, AuthorityPlane, AuthoritySource,
    PersistedAdmission,
};
pub use authority::{AuthorityTerm, EffectiveGrantSubset, intersect_authority};
