"""Monotonic authority narrowing for declared child agents."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, cast

from kiteframe import (
    AdmissionRequest,
    AuthorityRevisionSet,
    CapabilityGrantSet,
    DelegationEdge,
    EffectiveCapabilityGrant,
    KiteframeDiagnosticError,
    ResolvedCapabilityRequirement,
    ResolvedRuntimeInputs,
    ResolvedSubagent,
    build_delegation_edge,
    resource_selector_is_within,
)

from .context import (
    ChildAdmissionCorrelation,
    KiteframeSessionContext,
    _snapshot_session_context,
)

AUTHORIZATION_DENIED = "KF-AUTH-001"
_EFFECT_RANK = {
    "read_only": 0,
    "reversible_write": 1,
    "irreversible_write": 2,
    "external_side_effect": 3,
}


@dataclass(frozen=True, slots=True)
class ChildAuthorityEnvelope:
    """The exact admitted child grants and their immutable authority context."""

    grants: tuple[EffectiveCapabilityGrant, ...]
    expires_at: int
    authority_revisions: AuthorityRevisionSet | None
    ancestry: tuple[DelegationEdge, ...]


@dataclass(frozen=True, slots=True)
class DeclaredSubAgentInput:
    """Closed inputs and admission snapshot for one native child declaration."""

    declaration: ResolvedSubagent
    runtime_inputs: ResolvedRuntimeInputs
    session: KiteframeSessionContext
    admission_request: AdmissionRequest
    admission: CapabilityGrantSet
    children: tuple[DeclaredSubAgentInput, ...] = ()

    def __post_init__(self) -> None:
        if not isinstance(self.declaration, ResolvedSubagent):
            raise TypeError("declaration must be native ResolvedSubagent")
        if not isinstance(self.runtime_inputs, ResolvedRuntimeInputs):
            raise TypeError("runtime_inputs must be native ResolvedRuntimeInputs")
        if type(self.session) is not KiteframeSessionContext:
            raise TypeError("session must be exact KiteframeSessionContext")
        if type(self.admission_request) is not AdmissionRequest:
            raise TypeError("admission_request must be native AdmissionRequest")
        if type(self.admission) is not CapabilityGrantSet:
            raise TypeError("admission must be native CapabilityGrantSet")
        if type(self.children) is not tuple or not all(
            type(child) is DeclaredSubAgentInput for child in self.children
        ):
            raise TypeError(
                "children must be exact immutable DeclaredSubAgentInput values"
            )
        object.__setattr__(
            self,
            "session",
            _snapshot_session_context(self.session),
        )


def _admission_denied() -> KiteframeDiagnosticError:
    message = "child admission was denied"
    error = KiteframeDiagnosticError(message)
    setattr(error, "code", AUTHORIZATION_DENIED)  # noqa: B010
    setattr(  # noqa: B010
        error,
        "diagnostics_json",
        json.dumps(
            [
                {
                    "category": "authorization",
                    "code": AUTHORIZATION_DENIED,
                    "details": {},
                    "help": None,
                    "message": message,
                    "package_path": None,
                    "retry": "never",
                    "severity": "error",
                    "source_range": None,
                    "stage": "admit",
                }
            ],
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode(),
    )
    return error


def _grants(
    value: EffectiveCapabilityGrant | tuple[EffectiveCapabilityGrant, ...],
    *,
    name: str,
) -> tuple[EffectiveCapabilityGrant, ...]:
    values = (value,) if isinstance(value, EffectiveCapabilityGrant) else value
    if type(values) is not tuple or not all(
        isinstance(grant, EffectiveCapabilityGrant) for grant in values
    ):
        raise TypeError(f"{name} must be native EffectiveCapabilityGrant values")
    identities = [(grant.name, grant.version) for grant in values]
    if len(identities) != len(set(identities)):
        raise _admission_denied()
    return tuple(values)


def _requirements(
    value: ResolvedCapabilityRequirement | tuple[ResolvedCapabilityRequirement, ...],
) -> tuple[ResolvedCapabilityRequirement, ...]:
    values = (value,) if isinstance(value, ResolvedCapabilityRequirement) else value
    if type(values) is not tuple or not all(
        isinstance(requirement, ResolvedCapabilityRequirement) for requirement in values
    ):
        raise TypeError(
            "child_requirements must be native ResolvedCapabilityRequirement values"
        )
    identities = [(requirement.name, requirement.version) for requirement in values]
    if len(identities) != len(set(identities)):
        raise _admission_denied()
    return tuple(values)


def _resources_narrow(
    effective: tuple[str, ...],
    allowed: tuple[str, ...],
) -> bool:
    return bool(effective) and all(
        any(resource_selector_is_within(resource, candidate) for candidate in allowed)
        for resource in effective
    )


def _effect_not_greater(effective: str, allowed: str) -> bool:
    effective_rank = _EFFECT_RANK.get(effective)
    allowed_rank = _EFFECT_RANK.get(allowed)
    return (
        effective_rank is not None
        and allowed_rank is not None
        and effective_rank <= allowed_rank
    )


def _maximum_not_larger(
    effective: object,
    allowed: object,
) -> bool:
    if allowed is None:
        return True
    if (
        isinstance(effective, int)
        and not isinstance(effective, bool)
        and isinstance(allowed, int)
        and not isinstance(allowed, bool)
    ):
        return effective <= allowed
    return False


def _freshness_not_weaker(
    effective: object,
    allowed: object,
) -> bool:
    if not isinstance(effective, dict) or not isinstance(allowed, dict):
        return False
    return (
        _maximum_not_larger(
            effective.get("maxAdmissionAgeSeconds"),
            allowed.get("maxAdmissionAgeSeconds"),
        )
        and _maximum_not_larger(
            effective.get("maxInputAgeSeconds"),
            allowed.get("maxInputAgeSeconds"),
        )
        and (
            not allowed.get("policyRevisionRequired", False)
            or effective.get("policyRevisionRequired") is True
        )
    )


def _evidence_item_not_weaker(
    effective: object,
    allowed: object,
) -> bool:
    if not isinstance(effective, dict) or not isinstance(allowed, dict):
        return False
    if allowed.get("kind") == "none":
        return True
    return effective == allowed


def _evidence_not_weaker(
    effective: object,
    allowed: object,
) -> bool:
    if not isinstance(effective, dict) or not isinstance(allowed, dict):
        return False
    return all(
        _evidence_item_not_weaker(
            effective.get(kind),
            allowed.get(kind),
        )
        for kind in ("confirmation", "approval", "consent")
    )


def _comparable_json(value: Any) -> object:
    if isinstance(value, dict):
        return tuple(
            (key, _comparable_json(item)) for key, item in sorted(value.items())
        )
    if isinstance(value, (list, tuple)):
        return tuple(_comparable_json(item) for item in value)
    return value


def _preconditions_not_weaker(
    effective: object,
    allowed: object,
) -> bool:
    if not isinstance(effective, (list, tuple)) or not isinstance(
        allowed,
        (list, tuple),
    ):
        return False
    effective_values = {_comparable_json(value) for value in effective}
    return all(
        not isinstance(value, dict)
        or value.get("required") is not True
        or _comparable_json(value) in effective_values
        for value in allowed
    )


def _grant_narrows_grant(
    effective: EffectiveCapabilityGrant,
    allowed: EffectiveCapabilityGrant,
) -> bool:
    return (
        (effective.name, effective.version) == (allowed.name, allowed.version)
        and _resources_narrow(effective.resources, allowed.resources)
        and set(effective.execution_modes).issubset(allowed.execution_modes)
        and _effect_not_greater(
            effective.maximum_effect,
            allowed.maximum_effect,
        )
        and effective.expires_at <= allowed.expires_at
        and _freshness_not_weaker(
            effective.freshness,
            allowed.freshness,
        )
        and _evidence_not_weaker(
            effective.required_evidence,
            allowed.required_evidence,
        )
        and _preconditions_not_weaker(
            effective.preconditions,
            allowed.preconditions,
        )
    )


def _grant_narrows_requirement(
    effective: EffectiveCapabilityGrant,
    requirement: ResolvedCapabilityRequirement,
) -> bool:
    descriptor = requirement.descriptor
    required_evidence = {
        "approval": descriptor.approval,
        "confirmation": descriptor.confirmation,
        "consent": descriptor.consent,
    }
    return (
        (effective.name, effective.version) == (requirement.name, requirement.version)
        and _resources_narrow(effective.resources, requirement.resources)
        and set(effective.execution_modes).issubset(descriptor.execution_modes)
        and _effect_not_greater(
            effective.maximum_effect,
            descriptor.effect,
        )
        and _freshness_not_weaker(
            effective.freshness,
            descriptor.freshness,
        )
        and _evidence_not_weaker(
            effective.required_evidence,
            required_evidence,
        )
        and _preconditions_not_weaker(
            effective.preconditions,
            descriptor.preconditions,
        )
    )


def intersect_child_envelope(
    *,
    parent: EffectiveCapabilityGrant | tuple[EffectiveCapabilityGrant, ...],
    delegation: ResolvedSubagent
    | EffectiveCapabilityGrant
    | tuple[EffectiveCapabilityGrant, ...],
    child_requirements: ResolvedCapabilityRequirement
    | tuple[ResolvedCapabilityRequirement, ...],
    child_admission: KiteframeSessionContext
    | EffectiveCapabilityGrant
    | tuple[EffectiveCapabilityGrant, ...],
    ancestry: tuple[DelegationEdge, ...] = (),
    parent_authority_revisions: AuthorityRevisionSet | None = None,
    parent_agent: str | None = None,
    child_agent: str | None = None,
) -> ChildAuthorityEnvelope:
    """Validate that one exact child admission narrows every authority term."""

    parent_grants = _grants(parent, name="parent")
    requirements = _requirements(child_requirements)
    authority_revisions: AuthorityRevisionSet | None = None
    if type(child_admission) is KiteframeSessionContext:
        if type(parent_authority_revisions) is not AuthorityRevisionSet:
            raise TypeError(
                "parent_authority_revisions must be native AuthorityRevisionSet"
            )
        admitted_session = _snapshot_session_context(child_admission)
        admitted = admitted_session.grants
        authority_revisions = admitted_session.authority_revisions
        parent_entries = {
            entry.source: entry.revision for entry in parent_authority_revisions.entries
        }
        child_entries = {
            entry.source: entry.revision for entry in authority_revisions.entries
        }
        if any(
            child_entries.get(source) != revision
            for source, revision in parent_entries.items()
        ):
            raise _admission_denied()
    else:
        admitted = _grants(
            cast(
                EffectiveCapabilityGrant | tuple[EffectiveCapabilityGrant, ...],
                child_admission,
            ),
            name="child_admission",
        )

    if isinstance(delegation, ResolvedSubagent):
        delegated_names = delegation.delegated_capabilities
        delegation_grants = tuple(
            grant for grant in parent_grants if grant.name in delegated_names
        )
    else:
        delegation_grants = _grants(delegation, name="delegation")
        delegated_names = tuple(grant.name for grant in delegation_grants)

    if type(ancestry) is not tuple or not all(
        type(entry) is DelegationEdge for entry in ancestry
    ):
        raise TypeError("ancestry must be exact immutable native DelegationEdge values")
    if (parent_agent is None) != (child_agent is None):
        raise TypeError("parent_agent and child_agent must be supplied together")
    seen_agents: set[str] = set()
    previous_child: str | None = None
    for entry in ancestry:
        if (
            (previous_child is not None and entry.parent_agent != previous_child)
            or entry.parent_agent == entry.child_agent
            or entry.child_agent in seen_agents
        ):
            raise _admission_denied()
        seen_agents.add(entry.parent_agent)
        seen_agents.add(entry.child_agent)
        previous_child = entry.child_agent
    if (
        parent_agent is not None
        and child_agent is not None
        and (
            parent_agent == child_agent
            or (previous_child is not None and parent_agent != previous_child)
            or child_agent in seen_agents
        )
    ):
        raise _admission_denied()

    parent_by_identity = {(grant.name, grant.version): grant for grant in parent_grants}
    delegation_by_identity = {
        (grant.name, grant.version): grant for grant in delegation_grants
    }
    requirements_by_identity = {
        (requirement.name, requirement.version): requirement
        for requirement in requirements
    }
    admitted_by_identity = {(grant.name, grant.version): grant for grant in admitted}

    for identity, requirement in requirements_by_identity.items():
        if requirement.required and identity not in admitted_by_identity:
            raise _admission_denied()
    for identity, grant_value in admitted_by_identity.items():
        parent_grant = parent_by_identity.get(identity)
        delegated_grant = delegation_by_identity.get(identity)
        requirement = requirements_by_identity.get(identity)
        if (
            grant_value.name not in delegated_names
            or parent_grant is None
            or delegated_grant is None
            or requirement is None
            or not _grant_narrows_grant(grant_value, parent_grant)
            or not _grant_narrows_grant(grant_value, delegated_grant)
            or not _grant_narrows_requirement(grant_value, requirement)
        ):
            raise _admission_denied()

    child_ancestry = tuple(ancestry)
    if parent_agent is not None and child_agent is not None:
        child_ancestry += (
            build_delegation_edge(
                parent_agent,
                child_agent,
                sorted(grant.name for grant in admitted),
            ),
        )
    return ChildAuthorityEnvelope(
        grants=tuple(admitted),
        expires_at=min(
            (grant.expires_at for grant in admitted),
            default=0,
        ),
        authority_revisions=authority_revisions,
        ancestry=child_ancestry,
    )


def bind_child_admission(
    session: KiteframeSessionContext,
    request: AdmissionRequest,
    admission: CapabilityGrantSet,
    ancestry: tuple[DelegationEdge, ...],
) -> KiteframeSessionContext:
    """Bind an already narrowed child session to native admission evidence."""

    try:
        correlation = ChildAdmissionCorrelation(
            request=request,
            admission=admission,
            ancestry=ancestry,
        )
        return KiteframeSessionContext(
            actor=session.actor,
            session=session.session,
            task=session.task,
            admission_id=session.admission_id,
            grant_digest=session.grant_digest,
            delegation_ancestry_digest=session.delegation_ancestry_digest,
            grants=session.grants,
            authority_revisions=session.authority_revisions,
            trace_context=session.trace_context,
            suspension=session.suspension,
            child_admission=correlation,
        )
    except Exception:
        raise _admission_denied() from None


__all__ = [
    "ChildAuthorityEnvelope",
    "DeclaredSubAgentInput",
    "DelegationEdge",
    "bind_child_admission",
    "intersect_child_envelope",
]
