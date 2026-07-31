use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};

use kiteframe_contract::{
    AdmissionId, AdmissionRequest, AuthorityRevision, AuthorityRevisionSet, CapabilityCatalog,
    CapabilityDenial, CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, EffectiveCapabilityGrant,
    EffectiveCapabilityGrantParts, LockedCapability, PolicyRevision, RequestedCapability,
    RequiredEvidence, ResolvedCapabilityRequirement, Sha256Digest, Timestamp,
};

use crate::{AuthorityTerm, intersect_authority};

#[derive(Clone, Debug)]
pub struct AdmissionServiceConfig {
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub policy_revision: PolicyRevision,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AuthorityDomain {
    Package,
    Deployment,
    Human,
    Workload,
    Task,
    Session,
}

impl AuthorityDomain {
    pub const ALL: [Self; 6] = [
        Self::Package,
        Self::Deployment,
        Self::Human,
        Self::Workload,
        Self::Task,
        Self::Session,
    ];
}

#[derive(Clone, Debug)]
pub struct AuthorityPlane {
    domain: AuthorityDomain,
    terms: Vec<AuthorityTerm>,
}

impl AuthorityPlane {
    pub fn new(domain: AuthorityDomain, terms: Vec<AuthorityTerm>) -> Self {
        Self { domain, terms }
    }

    pub fn domain(&self) -> AuthorityDomain {
        self.domain
    }

    pub fn terms(&self) -> &[AuthorityTerm] {
        &self.terms
    }

    fn terms_for(&self, identity: &CapabilityIdentity) -> Vec<AuthorityTerm> {
        self.terms
            .iter()
            .filter(|term| term_matches(term, identity))
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct AuthoritySource {
    revision: AuthorityRevision,
    planes: BTreeMap<AuthorityDomain, AuthorityPlane>,
}

impl AuthoritySource {
    pub fn try_new(
        source: impl Into<String>,
        revision: impl Into<String>,
        planes: Vec<AuthorityPlane>,
    ) -> Result<Self, String> {
        let mut indexed_planes = BTreeMap::new();
        for plane in planes {
            let domain = plane.domain();
            if indexed_planes.insert(domain, plane).is_some() {
                return Err("authority source contains a duplicate domain".to_owned());
            }
        }
        Ok(Self {
            revision: AuthorityRevision::try_new(source, revision)?,
            planes: indexed_planes,
        })
    }

    pub fn revision(&self) -> &AuthorityRevision {
        &self.revision
    }

    pub fn planes(&self) -> impl Iterator<Item = &AuthorityPlane> {
        self.planes.values()
    }

    fn plane(&self, domain: AuthorityDomain) -> Option<&AuthorityPlane> {
        self.planes.get(&domain)
    }
}

#[derive(Clone, Debug)]
pub struct PersistedAdmission {
    locked_capabilities: BTreeMap<CapabilityIdentity, LockedCapability>,
}

impl PersistedAdmission {
    pub fn locked_capability(&self, identity: &CapabilityIdentity) -> Option<&LockedCapability> {
        self.locked_capabilities.get(identity)
    }

    pub fn locked_capabilities(&self) -> &BTreeMap<CapabilityIdentity, LockedCapability> {
        &self.locked_capabilities
    }
}

pub struct AdmissionService {
    catalog: CapabilityCatalog,
    registry: BTreeMap<CapabilityIdentity, LockedCapability>,
    sources: Vec<AuthoritySource>,
    authority_revisions: AuthorityRevisionSet,
    config: AdmissionServiceConfig,
    admissions: Mutex<BTreeMap<(String, String), PersistedAdmission>>,
}

impl AdmissionService {
    pub fn try_new(
        catalog: CapabilityCatalog,
        locked_capabilities: Vec<LockedCapability>,
        sources: Vec<AuthoritySource>,
        config: AdmissionServiceConfig,
    ) -> Result<Self, Vec<Diagnostic>> {
        if config.expires_at <= config.issued_at {
            return Err(vec![catalog_error(
                "admission service expiry must be after issue time",
            )]);
        }
        if catalog.issued_at() > config.issued_at
            || catalog
                .expires_at()
                .is_some_and(|expiry| expiry <= config.issued_at)
            || catalog
                .expires_at()
                .is_some_and(|expiry| config.expires_at > expiry)
        {
            return Err(vec![catalog_error(
                "admission validity must be contained by the authoritative catalog",
            )]);
        }

        let mut registry = BTreeMap::new();
        for locked in locked_capabilities {
            let identity = locked.identity().clone();
            if registry.insert(identity, locked).is_some() {
                return Err(vec![capability_error(
                    "locked descriptor registry contains duplicate capability identities",
                )]);
            }
        }
        for descriptor in catalog.descriptors() {
            let Some(locked) = registry.get(descriptor.identity()) else {
                return Err(vec![capability_error(
                    "catalog descriptor is missing from the locked descriptor registry",
                )]);
            };
            if locked.descriptor() != descriptor {
                return Err(vec![capability_error(
                    "locked descriptor registry does not match the authoritative catalog",
                )]);
            }
        }
        if registry.len() != catalog.descriptors().len() {
            return Err(vec![capability_error(
                "locked descriptor registry contains a capability outside the catalog",
            )]);
        }

        let authority_revisions = AuthorityRevisionSet::try_new(
            sources
                .iter()
                .map(|source| source.revision.clone())
                .collect(),
        )
        .map_err(|message| vec![authorization_error(message)])?;
        let mut domain_owners = BTreeMap::new();
        for (source_index, source) in sources.iter().enumerate() {
            for domain in source.planes.keys() {
                if domain_owners.insert(*domain, source_index).is_some() {
                    return Err(vec![authorization_error(
                        "mandatory authority domain has multiple owners",
                    )]);
                }
            }
        }

        Ok(Self {
            catalog,
            registry,
            sources,
            authority_revisions,
            config,
            admissions: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn authority_revisions(&self) -> &AuthorityRevisionSet {
        &self.authority_revisions
    }

    pub async fn admit(&self, request: AdmissionRequest) -> Result<CapabilityGrantSet, Diagnostic> {
        self.validate_catalog_binding(&request)?;
        let request_requirements = request_requirements(&request)?;
        let validated_locks = self.validate_request_locks(&request_requirements)?;
        validate_requirement_mapping(&request, &request_requirements)?;

        let mut grants = Vec::new();
        let mut optional_denials = Vec::new();
        for requirement in &request_requirements {
            let requested = request
                .required_capabilities()
                .iter()
                .chain(request.optional_capabilities())
                .find(|requested| requested.capability() == requirement.identity())
                .expect("one-to-one requirement mapping was validated");
            if requirement.required() {
                let Some(grant) = self.admit_capability(requested, requirement)? else {
                    return Err(authorization_error("required capability was not admitted"));
                };
                grants.push(grant);
                continue;
            }

            match self.admit_capability(requested, requirement) {
                Ok(Some(grant)) => grants.push(grant),
                Ok(None) => optional_denials.push(optional_denial(requested)?),
                Err(error) if error.code == DiagnosticCode::AdmissionDenied => {
                    optional_denials.push(optional_denial(requested)?);
                }
                Err(error) => return Err(error),
            }
        }

        let admission_id = AdmissionId::new(format!("admission-{}", request.request_digest()))
            .map_err(capability_error)?;
        let grant_set = CapabilityGrantSet::try_new(CapabilityGrantSetParts {
            admission_id,
            admission_request_digest: *request.request_digest(),
            delegation_ancestry_digest: *request.delegation_ancestry_digest(),
            actor: request.actor().clone(),
            agent: request.agent().clone(),
            task: request.task().clone(),
            session: request.session().clone(),
            policy_revision: self.config.policy_revision.clone(),
            catalog_identity: self.catalog.identity().clone(),
            catalog_digest: *self.catalog.catalog_digest(),
            authority_revisions: self.authority_revisions.clone(),
            issued_at: self.config.issued_at,
            expires_at: self.config.expires_at,
            grants,
            optional_denials,
        })
        .map_err(first_diagnostic)?;
        grant_set.validate_against(&request)?;

        self.lock_admissions().insert(
            (
                grant_set.admission_id().as_str().to_owned(),
                grant_set.grant_digest().to_string(),
            ),
            PersistedAdmission {
                locked_capabilities: validated_locks,
            },
        );
        Ok(grant_set)
    }

    pub async fn load_admission(
        &self,
        admission_id: &AdmissionId,
        grant_digest: &Sha256Digest,
    ) -> Result<PersistedAdmission, Diagnostic> {
        self.lock_admissions()
            .get(&(admission_id.as_str().to_owned(), grant_digest.to_string()))
            .cloned()
            .ok_or_else(|| capability_error("persisted admission does not match the grant digest"))
    }

    fn validate_catalog_binding(&self, request: &AdmissionRequest) -> Result<(), Diagnostic> {
        if request.catalog_identity() != self.catalog.identity()
            || request.catalog_digest() != self.catalog.catalog_digest()
        {
            return Err(catalog_error(
                "admission request catalog identity or digest is stale",
            ));
        }
        Ok(())
    }

    fn validate_request_locks(
        &self,
        requirements: &[ResolvedCapabilityRequirement],
    ) -> Result<BTreeMap<CapabilityIdentity, LockedCapability>, Diagnostic> {
        let mut validated = BTreeMap::new();
        for requirement in requirements {
            let Some(authoritative) = self.registry.get(requirement.identity()) else {
                return Err(capability_error(
                    "resolved capability is absent from the provider registry",
                ));
            };
            if requirement.locked_capability() != authoritative {
                return Err(capability_error(
                    "client locked descriptor does not match the provider registry",
                ));
            }
            if validated
                .insert(requirement.identity().clone(), authoritative.clone())
                .is_some()
            {
                return Err(capability_error(
                    "resolved requirements contain a duplicate capability identity",
                ));
            }
        }
        Ok(validated)
    }

    fn admit_capability(
        &self,
        requested: &RequestedCapability,
        requirement: &ResolvedCapabilityRequirement,
    ) -> Result<Option<kiteframe_contract::EffectiveCapabilityGrant>, Diagnostic> {
        if requirement.identity() != requested.capability() {
            return Err(capability_error(
                "resolved capability does not match the admission request entry",
            ));
        }

        let mut terms = Vec::new();
        for domain in AuthorityDomain::ALL {
            let Some(plane) = self.sources.iter().find_map(|source| source.plane(domain)) else {
                return Ok(None);
            };
            let plane_terms = plane.terms_for(requested.capability());
            if plane_terms.is_empty() {
                return Ok(None);
            }
            terms.extend(plane_terms);
        }
        terms.push(AuthorityTerm::allow(request_boundary(
            requirement,
            requested,
            self.config.expires_at,
        )?));

        intersect_authority(requirement, &terms).map_err(first_diagnostic)
    }

    fn lock_admissions(&self) -> MutexGuard<'_, BTreeMap<(String, String), PersistedAdmission>> {
        self.admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn term_matches(term: &AuthorityTerm, identity: &CapabilityIdentity) -> bool {
    match term {
        AuthorityTerm::Allow(grant) => grant.capability() == identity,
        AuthorityTerm::Deny(name) => name == identity.name().as_str(),
    }
}

fn validate_requirement_mapping(
    request: &AdmissionRequest,
    requirements: &[ResolvedCapabilityRequirement],
) -> Result<(), Diagnostic> {
    let requested = request
        .required_capabilities()
        .iter()
        .map(|entry| (entry.capability(), true))
        .chain(
            request
                .optional_capabilities()
                .iter()
                .map(|entry| (entry.capability(), false)),
        )
        .collect::<Vec<_>>();
    if requirements.len() != requested.len() {
        return Err(capability_error(
            "resolved requirements must map one-to-one to admission entries",
        ));
    }
    for requirement in requirements {
        if requested
            .iter()
            .filter(|(identity, required)| {
                *identity == requirement.identity() && *required == requirement.required()
            })
            .count()
            != 1
        {
            return Err(capability_error(
                "resolved requirement identity or requiredness does not map exactly",
            ));
        }
    }
    Ok(())
}

fn request_boundary(
    requirement: &ResolvedCapabilityRequirement,
    requested: &RequestedCapability,
    session_expiry: Timestamp,
) -> Result<EffectiveCapabilityGrant, Diagnostic> {
    EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: requirement.identity().clone(),
        resources: requested.resources().to_vec(),
        execution_modes: requirement.descriptor().execution_modes().clone(),
        maximum_effect: requirement.descriptor().effect(),
        expires_at: session_expiry,
        required_evidence: RequiredEvidence::new(
            requirement.descriptor().confirmation().clone(),
            requirement.descriptor().approval().clone(),
            requirement.descriptor().consent().clone(),
        ),
        freshness: requirement.descriptor().freshness().clone(),
        preconditions: requirement.descriptor().preconditions().to_vec(),
    })
    .map_err(capability_error)
}

fn optional_denial(requested: &RequestedCapability) -> Result<CapabilityDenial, Diagnostic> {
    CapabilityDenial::try_new(
        requested.capability().clone(),
        authorization_error("optional capability was not admitted"),
    )
    .map_err(authorization_error)
}

fn request_requirements(
    request: &AdmissionRequest,
) -> Result<Vec<ResolvedCapabilityRequirement>, Diagnostic> {
    let serialized = serde_json::to_value(request)
        .map_err(|_| capability_error("admission request cannot be inspected"))?;
    serde_json::from_value(
        serialized
            .get("resolvedRequirements")
            .cloned()
            .ok_or_else(|| capability_error("admission request has no resolved requirements"))?,
    )
    .map_err(|_| capability_error("admission request resolved requirements are invalid"))
}

fn first_diagnostic(mut diagnostics: Vec<Diagnostic>) -> Diagnostic {
    if diagnostics.is_empty() {
        capability_error("provider operation failed without a diagnostic")
    } else {
        diagnostics.remove(0)
    }
}

fn catalog_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::CatalogIncompatible,
        DiagnosticCategory::Catalog,
        DiagnosticStage::Admit,
        message.into(),
    )
}

fn capability_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PreconditionMissing,
        DiagnosticCategory::Capability,
        DiagnosticStage::Admit,
        message.into(),
    )
}

fn authorization_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::AdmissionDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Admit,
        message.into(),
    )
}
