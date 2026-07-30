use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};

use kiteframe_contract::{
    AdmissionId, AdmissionRequest, AuthorityRevision, AuthorityRevisionSet, CapabilityCatalog,
    CapabilityDenial, CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, LockedCapability, PolicyRevision,
    RequestedCapability, ResolvedCapabilityRequirement, Sha256Digest, Timestamp,
};

use crate::{AuthorityTerm, intersect_authority};

#[derive(Clone, Debug)]
pub struct AdmissionServiceConfig {
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub policy_revision: PolicyRevision,
}

#[derive(Clone, Debug)]
pub struct AuthoritySource {
    revision: AuthorityRevision,
    terms: Vec<AuthorityTerm>,
}

impl AuthoritySource {
    pub fn try_new(
        source: impl Into<String>,
        revision: impl Into<String>,
        terms: Vec<AuthorityTerm>,
    ) -> Result<Self, String> {
        Ok(Self {
            revision: AuthorityRevision::try_new(source, revision)?,
            terms,
        })
    }

    pub fn revision(&self) -> &AuthorityRevision {
        &self.revision
    }

    pub fn terms(&self) -> &[AuthorityTerm] {
        &self.terms
    }

    fn terms_for(&self, identity: &CapabilityIdentity) -> Vec<AuthorityTerm> {
        self.terms
            .iter()
            .filter(|term| match term {
                AuthorityTerm::Allow(grant) => grant.capability() == identity,
                AuthorityTerm::Deny(name) => name == identity.name().as_str(),
            })
            .cloned()
            .collect()
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
        {
            return Err(vec![catalog_error(
                "authoritative catalog is not valid at admission issue time",
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

        let mut grants = Vec::new();
        for requested in request.required_capabilities() {
            let Some(grant) = self.admit_capability(requested, true, &request_requirements)? else {
                return Err(authorization_error("required capability was not admitted"));
            };
            grants.push(grant);
        }

        let mut optional_denials = Vec::new();
        for requested in request.optional_capabilities() {
            match self.admit_capability(requested, false, &request_requirements)? {
                Some(grant) => grants.push(grant),
                None => optional_denials.push(
                    CapabilityDenial::try_new(
                        requested.capability().clone(),
                        authorization_error("optional capability was not admitted"),
                    )
                    .map_err(authorization_error)?,
                ),
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
        required: bool,
        requirements: &[ResolvedCapabilityRequirement],
    ) -> Result<Option<kiteframe_contract::EffectiveCapabilityGrant>, Diagnostic> {
        let Some(client_requirement) = requirements
            .iter()
            .find(|requirement| requirement.identity() == requested.capability())
        else {
            return Err(capability_error(
                "requested capability has no resolved locked descriptor",
            ));
        };
        if client_requirement.required() != required {
            return Err(capability_error(
                "resolved capability requiredness does not match the admission request",
            ));
        }
        let authoritative = self
            .registry
            .get(requested.capability())
            .expect("all request requirements were registry validated")
            .clone();
        let narrowed_requirement = ResolvedCapabilityRequirement::try_new(
            authoritative,
            required,
            requested
                .resources()
                .iter()
                .map(|resource| resource.as_str().to_owned())
                .collect(),
        )
        .map_err(capability_error)?;

        let mut terms = Vec::new();
        for source in &self.sources {
            let source_terms = source.terms_for(requested.capability());
            if source_terms.is_empty() {
                return Ok(None);
            }
            terms.extend(source_terms);
        }

        intersect_authority(&narrowed_requirement, &terms).map_err(first_diagnostic)
    }

    fn lock_admissions(&self) -> MutexGuard<'_, BTreeMap<(String, String), PersistedAdmission>> {
        self.admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
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
