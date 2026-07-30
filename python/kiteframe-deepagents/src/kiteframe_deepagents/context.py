"""Immutable per-session state shared by Kiteframe runtime guards."""

from __future__ import annotations

from dataclasses import dataclass

from kiteframe import (
    AdmissionRequest,
    AuthorityRevisionSet,
    CapabilityGrantSet,
    EffectiveCapabilityGrant,
    Suspension,
    load_capability_grant_set_for_request,
)


def _grant_projection(grant: EffectiveCapabilityGrant) -> tuple[object, ...]:
    return (
        grant.name,
        grant.version,
        grant.resources,
        grant.execution_modes,
        grant.maximum_effect,
        grant.expires_at,
        grant.required_evidence,
        grant.freshness,
        grant.preconditions,
    )


@dataclass(frozen=True, slots=True)
class DelegationAncestryEntry:
    """One immutable declared authority edge."""

    parent_agent: str
    child_agent: str
    delegated_capabilities: tuple[str, ...]

    def __post_init__(self) -> None:
        if type(self.parent_agent) is not str or not self.parent_agent:
            raise TypeError("parent_agent must be a non-empty exact string")
        if type(self.child_agent) is not str or not self.child_agent:
            raise TypeError("child_agent must be a non-empty exact string")
        if (
            type(self.delegated_capabilities) is not tuple
            or not all(
                type(capability) is str and capability
                for capability in self.delegated_capabilities
            )
            or len(self.delegated_capabilities) != len(set(self.delegated_capabilities))
        ):
            raise TypeError("delegated_capabilities must be unique exact strings")


@dataclass(frozen=True, slots=True)
class ChildAdmissionCorrelation:
    """Native child admission bound to its computed local delegation ancestry."""

    request: AdmissionRequest
    admission: CapabilityGrantSet
    ancestry: tuple[DelegationAncestryEntry, ...]

    def __post_init__(self) -> None:
        if type(self.request) is not AdmissionRequest:
            raise TypeError("request must be exact native AdmissionRequest")
        if type(self.admission) is not CapabilityGrantSet:
            raise TypeError("admission must be exact native CapabilityGrantSet")
        if type(self.ancestry) is not tuple or not all(
            type(entry) is DelegationAncestryEntry for entry in self.ancestry
        ):
            raise TypeError(
                "ancestry must be exact immutable DelegationAncestryEntry values"
            )
        load_capability_grant_set_for_request(
            self.admission.canonical_json(),
            self.request,
        )
        expected_native_ancestry = tuple(
            sorted({entry.parent_agent for entry in self.ancestry})
        )
        if self.request.delegation_ancestry != expected_native_ancestry:
            raise ValueError("child admission delegation ancestry does not match")


@dataclass(frozen=True, slots=True)
class KiteframeTraceContext:
    """Immutable W3C trace values carried into native request construction."""

    traceparent: str
    tracestate: str | None = None
    baggage: tuple[tuple[str, str], ...] = ()

    def __post_init__(self) -> None:
        if type(self.traceparent) is not str or not self.traceparent:
            raise TypeError("traceparent must be a non-empty string")
        if self.tracestate is not None and type(self.tracestate) is not str:
            raise TypeError("tracestate must be a string or None")
        if type(self.baggage) is not tuple or any(
            type(entry) is not tuple
            or len(entry) != 2
            or not all(type(value) is str for value in entry)
            for entry in self.baggage
        ):
            raise TypeError("baggage must be a tuple of string pairs")


@dataclass(frozen=True, slots=True)
class KiteframeSessionContext:
    """Authorization and trace state isolated to one agent session."""

    actor: str
    session: str
    task: str
    admission_id: str
    grant_digest: str
    grants: tuple[EffectiveCapabilityGrant, ...]
    authority_revisions: AuthorityRevisionSet
    trace_context: KiteframeTraceContext
    suspension: Suspension | None = None
    child_admission: ChildAdmissionCorrelation | None = None

    def __post_init__(self) -> None:
        for value, name in (
            (self.actor, "actor"),
            (self.session, "session"),
            (self.task, "task"),
            (self.admission_id, "admission_id"),
            (self.grant_digest, "grant_digest"),
        ):
            if type(value) is not str or not value:
                raise TypeError(f"{name} must be a non-empty exact string")
        if type(self.grants) is not tuple or not all(
            type(grant) is EffectiveCapabilityGrant for grant in self.grants
        ):
            raise TypeError("grants must be a tuple of native EffectiveCapabilityGrant")
        if type(self.authority_revisions) is not AuthorityRevisionSet:
            raise TypeError("authority_revisions must be native AuthorityRevisionSet")
        if type(self.trace_context) is not KiteframeTraceContext:
            raise TypeError("trace_context must be immutable KiteframeTraceContext")
        if self.suspension is not None and type(self.suspension) is not Suspension:
            raise TypeError("suspension must be native Suspension or None")
        correlation = self.child_admission
        if correlation is not None:
            if type(correlation) is not ChildAdmissionCorrelation:
                raise TypeError(
                    "child_admission must be exact ChildAdmissionCorrelation or None"
                )
            request = correlation.request
            admission = correlation.admission
            if (
                request.actor != self.actor
                or request.task != self.task
                or request.session != self.session
                or admission.actor != self.actor
                or admission.task != self.task
                or admission.session != self.session
                or admission.admission_id != self.admission_id
                or admission.grant_digest != self.grant_digest
                or admission.authority_revisions.authority_revision_digest
                != self.authority_revisions.authority_revision_digest
                or tuple(_grant_projection(grant) for grant in admission.grants)
                != tuple(_grant_projection(grant) for grant in self.grants)
            ):
                raise ValueError("child admission does not match the session")


def _snapshot_session_context(
    session: KiteframeSessionContext,
) -> KiteframeSessionContext:
    """Detach one complete exact authority snapshot from its caller."""

    if type(session) is not KiteframeSessionContext:
        raise TypeError("session must be exact KiteframeSessionContext")
    trace = session.trace_context
    if type(trace) is not KiteframeTraceContext:
        raise TypeError("trace_context must be exact KiteframeTraceContext")
    trace_snapshot = KiteframeTraceContext(
        traceparent=trace.traceparent,
        tracestate=trace.tracestate,
        baggage=tuple((key, value) for key, value in trace.baggage),
    )
    return KiteframeSessionContext(
        actor=session.actor,
        session=session.session,
        task=session.task,
        admission_id=session.admission_id,
        grant_digest=session.grant_digest,
        grants=tuple(grant for grant in session.grants),
        authority_revisions=session.authority_revisions,
        trace_context=trace_snapshot,
        suspension=session.suspension,
        child_admission=session.child_admission,
    )


__all__ = [
    "ChildAdmissionCorrelation",
    "DelegationAncestryEntry",
    "KiteframeSessionContext",
    "KiteframeTraceContext",
]
