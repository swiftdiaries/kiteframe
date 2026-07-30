use std::{collections::BTreeSet, num::NonZeroU64};

use kiteframe_contract::{
    ApprovalRequirement, ConfirmationRequirement, ConsentRequirement, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, EffectiveCapabilityGrant,
    EffectiveCapabilityGrantParts, FreshnessRequirement, NonEmptySet, NormalizedResourceSelector,
    RequiredEvidence, ResolvedCapabilityRequirement,
};

#[derive(Clone, Debug)]
pub enum AuthorityTerm {
    Allow(Box<EffectiveCapabilityGrant>),
    Deny(String),
}

impl AuthorityTerm {
    pub fn allow(grant: EffectiveCapabilityGrant) -> Self {
        Self::Allow(Box::new(grant))
    }

    pub fn deny(capability_name: impl Into<String>) -> Self {
        Self::Deny(capability_name.into())
    }

    pub fn is_explicit_deny(&self) -> bool {
        matches!(self, Self::Deny(_))
    }

    pub fn allow_value(&self) -> Option<&EffectiveCapabilityGrant> {
        match self {
            Self::Allow(grant) => Some(grant),
            Self::Deny(_) => None,
        }
    }
}

pub fn intersect_authority(
    requirement: &ResolvedCapabilityRequirement,
    terms: &[AuthorityTerm],
) -> Result<Option<EffectiveCapabilityGrant>, Vec<Diagnostic>> {
    if terms.is_empty() || terms.iter().any(AuthorityTerm::is_explicit_deny) {
        return Ok(None);
    }

    let grants = terms
        .iter()
        .filter_map(AuthorityTerm::allow_value)
        .collect::<Vec<_>>();
    if grants.is_empty()
        || grants
            .iter()
            .any(|grant| grant.capability() != requirement.identity())
    {
        return Ok(None);
    }

    let mut resources = requirement.resources().to_vec();
    for grant in &grants {
        resources = intersect_resource_sets(
            &resources,
            &grant
                .resources()
                .iter()
                .map(|selector| selector.as_str().to_owned())
                .collect::<Vec<_>>(),
        )?;
        if resources.is_empty() {
            return Ok(None);
        }
    }

    let mut execution_modes = requirement.descriptor().execution_modes().as_set().clone();
    for grant in &grants {
        execution_modes = execution_modes
            .intersection(grant.execution_modes().as_set())
            .copied()
            .collect();
    }
    if execution_modes.is_empty() {
        return Ok(None);
    }

    let maximum_effect = grants
        .iter()
        .fold(requirement.descriptor().effect(), |effect, grant| {
            effect.min(grant.maximum_effect())
        });
    let expires_at = grants
        .iter()
        .map(|grant| grant.expires_at())
        .min()
        .expect("at least one allow term");

    let mut required_evidence = RequiredEvidence::new(
        requirement.descriptor().confirmation().clone(),
        requirement.descriptor().approval().clone(),
        requirement.descriptor().consent().clone(),
    );
    for grant in &grants {
        required_evidence = intersect_evidence(&required_evidence, grant.required_evidence())?;
    }

    let mut freshness = requirement.descriptor().freshness().clone();
    for grant in &grants {
        freshness = intersect_freshness(&freshness, grant.freshness());
    }

    let mut preconditions = requirement.descriptor().preconditions().to_vec();
    for grant in &grants {
        preconditions.extend_from_slice(grant.preconditions());
    }
    preconditions.sort();
    preconditions.dedup();

    EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: requirement.identity().clone(),
        resources: resources
            .into_iter()
            .map(NormalizedResourceSelector::new)
            .collect::<Result<_, _>>()
            .map_err(|message| vec![denied(message)])?,
        execution_modes: NonEmptySet::try_new(execution_modes)
            .map_err(|message| vec![denied(message)])?,
        maximum_effect,
        expires_at,
        required_evidence,
        freshness,
        preconditions,
    })
    .map(Some)
    .map_err(|message| vec![denied(message)])
}

pub trait EffectiveGrantSubset {
    fn is_subset_of(&self, other: &Self) -> bool;
}

impl EffectiveGrantSubset for EffectiveCapabilityGrant {
    fn is_subset_of(&self, other: &Self) -> bool {
        self.capability() == other.capability()
            && self.resources().iter().all(|resource| {
                other.resources().iter().any(|allowed| {
                    selector_is_subset(resource.as_str(), allowed.as_str()).unwrap_or(false)
                })
            })
            && self
                .execution_modes()
                .as_set()
                .is_subset(other.execution_modes().as_set())
            && self.maximum_effect() <= other.maximum_effect()
            && self.expires_at() <= other.expires_at()
            && evidence_is_at_least(self.required_evidence(), other.required_evidence())
            && freshness_is_at_least(self.freshness(), other.freshness())
            && other
                .preconditions()
                .iter()
                .all(|required| self.preconditions().contains(required))
    }
}

fn intersect_resource_sets(
    left: &[String],
    right: &[String],
) -> Result<Vec<String>, Vec<Diagnostic>> {
    let mut intersections = BTreeSet::new();
    for left_selector in left {
        validate_selector(left_selector).map_err(|message| vec![denied(message)])?;
        for right_selector in right {
            validate_selector(right_selector).map_err(|message| vec![denied(message)])?;
            if let Some(intersection) = selector_intersection(left_selector, right_selector) {
                intersections.insert(intersection);
            }
        }
    }
    Ok(intersections.into_iter().collect())
}

fn selector_intersection(left: &str, right: &str) -> Option<String> {
    let left = ParsedSelector::parse(left)?;
    let right = ParsedSelector::parse(right)?;
    if left.separators != right.separators || left.tokens.len() != right.tokens.len() {
        return None;
    }

    let mut tokens = Vec::with_capacity(left.tokens.len());
    for (left, right) in left.tokens.iter().zip(&right.tokens) {
        match (left.as_str(), right.as_str()) {
            (left, right) if left == right => tokens.push(left.to_owned()),
            ("*", right) => tokens.push(right.to_owned()),
            (left, "*") => tokens.push(left.to_owned()),
            _ => return None,
        }
    }
    Some(
        ParsedSelector {
            tokens,
            separators: left.separators,
        }
        .render(),
    )
}

fn selector_is_subset(left: &str, right: &str) -> Result<bool, String> {
    validate_selector(left)?;
    validate_selector(right)?;
    Ok(selector_intersection(left, right).as_deref() == Some(left))
}

fn validate_selector(value: &str) -> Result<(), String> {
    if value.contains("${context.") {
        return Err("resource selector contains an unresolved context placeholder".to_owned());
    }
    let parsed = ParsedSelector::parse(value)
        .ok_or_else(|| "resource selector must contain non-empty segments".to_owned())?;
    if parsed
        .tokens
        .iter()
        .any(|token| token.contains('*') && token != "*")
    {
        return Err("resource selector wildcard must occupy a complete segment".to_owned());
    }
    Ok(())
}

#[derive(Clone)]
struct ParsedSelector {
    tokens: Vec<String>,
    separators: Vec<char>,
}

impl ParsedSelector {
    fn parse(value: &str) -> Option<Self> {
        let mut tokens = Vec::new();
        let mut separators = Vec::new();
        let mut current = String::new();
        for character in value.chars() {
            if matches!(character, '/' | ':') {
                if current.is_empty() {
                    return None;
                }
                tokens.push(std::mem::take(&mut current));
                separators.push(character);
            } else {
                current.push(character);
            }
        }
        if current.is_empty() {
            return None;
        }
        tokens.push(current);
        Some(Self { tokens, separators })
    }

    fn render(self) -> String {
        let mut rendered = self.tokens[0].clone();
        for (separator, token) in self
            .separators
            .into_iter()
            .zip(self.tokens.into_iter().skip(1))
        {
            rendered.push(separator);
            rendered.push_str(&token);
        }
        rendered
    }
}

fn intersect_evidence(
    left: &RequiredEvidence,
    right: &RequiredEvidence,
) -> Result<RequiredEvidence, Vec<Diagnostic>> {
    Ok(RequiredEvidence::new(
        confirmation_intersection(left.confirmation(), right.confirmation())?,
        approval_intersection(left.approval(), right.approval())?,
        consent_intersection(left.consent(), right.consent())?,
    ))
}

fn confirmation_intersection(
    left: &ConfirmationRequirement,
    right: &ConfirmationRequirement,
) -> Result<ConfirmationRequirement, Vec<Diagnostic>> {
    match (left, right) {
        (ConfirmationRequirement::None, value) | (value, ConfirmationRequirement::None) => {
            Ok(value.clone())
        }
        (
            ConfirmationRequirement::Required { evidence: left },
            ConfirmationRequirement::Required { evidence: right },
        ) if left == right => Ok(ConfirmationRequirement::Required {
            evidence: left.clone(),
        }),
        _ => Err(vec![denied("confirmation evidence requirements conflict")]),
    }
}

fn approval_intersection(
    left: &ApprovalRequirement,
    right: &ApprovalRequirement,
) -> Result<ApprovalRequirement, Vec<Diagnostic>> {
    match (left, right) {
        (ApprovalRequirement::None, value) | (value, ApprovalRequirement::None) => {
            Ok(value.clone())
        }
        (
            ApprovalRequirement::Required { evidence: left },
            ApprovalRequirement::Required { evidence: right },
        ) if left == right => Ok(ApprovalRequirement::Required {
            evidence: left.clone(),
        }),
        _ => Err(vec![denied("approval evidence requirements conflict")]),
    }
}

fn consent_intersection(
    left: &ConsentRequirement,
    right: &ConsentRequirement,
) -> Result<ConsentRequirement, Vec<Diagnostic>> {
    match (left, right) {
        (ConsentRequirement::None, value) | (value, ConsentRequirement::None) => Ok(value.clone()),
        (
            ConsentRequirement::Required { evidence: left },
            ConsentRequirement::Required { evidence: right },
        ) if left == right => Ok(ConsentRequirement::Required {
            evidence: left.clone(),
        }),
        _ => Err(vec![denied("consent evidence requirements conflict")]),
    }
}

fn intersect_freshness(
    left: &FreshnessRequirement,
    right: &FreshnessRequirement,
) -> FreshnessRequirement {
    FreshnessRequirement {
        max_admission_age_seconds: minimum_bound(
            left.max_admission_age_seconds,
            right.max_admission_age_seconds,
        ),
        policy_revision_required: left.policy_revision_required || right.policy_revision_required,
        max_input_age_seconds: minimum_bound(
            left.max_input_age_seconds,
            right.max_input_age_seconds,
        ),
    }
}

fn minimum_bound(left: Option<NonZeroU64>, right: Option<NonZeroU64>) -> Option<NonZeroU64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn evidence_is_at_least(effective: &RequiredEvidence, baseline: &RequiredEvidence) -> bool {
    confirmation_is_at_least(effective.confirmation(), baseline.confirmation())
        && approval_is_at_least(effective.approval(), baseline.approval())
        && consent_is_at_least(effective.consent(), baseline.consent())
}

fn confirmation_is_at_least(
    effective: &ConfirmationRequirement,
    baseline: &ConfirmationRequirement,
) -> bool {
    matches!(baseline, ConfirmationRequirement::None) || effective == baseline
}

fn approval_is_at_least(effective: &ApprovalRequirement, baseline: &ApprovalRequirement) -> bool {
    matches!(baseline, ApprovalRequirement::None) || effective == baseline
}

fn consent_is_at_least(effective: &ConsentRequirement, baseline: &ConsentRequirement) -> bool {
    matches!(baseline, ConsentRequirement::None) || effective == baseline
}

fn freshness_is_at_least(
    effective: &FreshnessRequirement,
    baseline: &FreshnessRequirement,
) -> bool {
    bound_is_at_least(
        effective.max_admission_age_seconds,
        baseline.max_admission_age_seconds,
    ) && bound_is_at_least(
        effective.max_input_age_seconds,
        baseline.max_input_age_seconds,
    ) && (!baseline.policy_revision_required || effective.policy_revision_required)
}

fn bound_is_at_least(effective: Option<NonZeroU64>, baseline: Option<NonZeroU64>) -> bool {
    match (effective, baseline) {
        (_, None) => true,
        (Some(effective), Some(baseline)) => effective <= baseline,
        (None, Some(_)) => false,
    }
}

fn denied(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::AdmissionDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Admit,
        message.into(),
    )
}
