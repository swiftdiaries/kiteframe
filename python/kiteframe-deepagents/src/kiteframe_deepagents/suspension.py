"""Protected LangGraph interrupt and native invocation-resume mapping."""

from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Literal

from kiteframe import (
    InvocationOutcome,
    InvocationRequest,
    ResolvedCapabilityRequirement,
    build_invocation_request_for_requirement,
)
from langgraph.types import Command, interrupt

from .context import KiteframeSessionContext

SUSPENSION_TYPE = "kiteframe.capability.suspension"
INVALID_SUSPENSION = "KF-CAP-002: invalid capability suspension"
EvidenceKind = Literal["confirmation", "approval", "consent"]


def _exact_non_empty(value: object, name: str) -> str:
    if type(value) is not str or not value:
        raise TypeError(f"{name} must be a non-empty exact string")
    return value


def _evidence_kind(value: object) -> EvidenceKind:
    if value not in {"confirmation", "approval", "consent"}:
        raise ValueError("suspension evidence kind is unsupported")
    return value  # type: ignore[return-value]


def _protected_evidence_ref(value: object) -> str:
    reference = _exact_non_empty(value, "evidence_ref")
    if any(character.isspace() for character in reference):
        raise ValueError("evidence_ref must be an opaque reference")
    return reference


@dataclass(frozen=True, slots=True)
class SuspensionEnvelope:
    """Reference-only interrupt payload copied from one native suspension."""

    type: Literal["kiteframe.capability.suspension"]
    invocation_id: str
    admission_id: str
    checkpoint_ref: str
    evidence_kind: EvidenceKind
    evidence_request_ref: str
    proposal_digest: str
    traceparent: str

    @classmethod
    def from_native(
        cls,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> SuspensionEnvelope:
        """Copy native correlation fields without decoding or recomputing them."""

        if type(request) is not InvocationRequest:
            raise TypeError("request must be exact native InvocationRequest")
        if type(outcome) is not InvocationOutcome:
            raise TypeError("outcome must be exact native InvocationOutcome")
        suspension = outcome.suspension
        if (
            outcome.status != "suspended"
            or suspension is None
            or outcome.invocation_id != request.invocation_id
        ):
            raise ValueError("outcome is not the request's native suspension")
        return cls(
            type=SUSPENSION_TYPE,
            invocation_id=_exact_non_empty(
                outcome.invocation_id,
                "invocation_id",
            ),
            admission_id=_exact_non_empty(
                request.admission_id,
                "admission_id",
            ),
            checkpoint_ref=_exact_non_empty(
                suspension.checkpoint_ref,
                "checkpoint_ref",
            ),
            evidence_kind=_evidence_kind(suspension.evidence_kind),
            evidence_request_ref=_exact_non_empty(
                suspension.evidence_request_ref,
                "evidence_request_ref",
            ),
            proposal_digest=_exact_non_empty(
                suspension.proposal_digest,
                "proposal_digest",
            ),
            traceparent=_exact_non_empty(
                request.traceparent,
                "traceparent",
            ),
        )


@dataclass(frozen=True, slots=True)
class LangGraphSuspensionBridge:
    """Map a native suspension to the public LangGraph interrupt primitive."""

    async def suspend(
        self,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> str:
        envelope = SuspensionEnvelope.from_native(request, outcome)
        resumed = interrupt(asdict(envelope))
        return _protected_evidence_ref(resumed)


def resume_command(evidence_ref: str) -> Command:
    """Create the only public resume command accepted by the adapter."""

    return Command(resume=_protected_evidence_ref(evidence_ref))


def build_resumed_invocation_request(
    *,
    request: InvocationRequest,
    outcome: InvocationOutcome,
    requirement: ResolvedCapabilityRequirement,
    session: KiteframeSessionContext,
    evidence_ref: str,
) -> InvocationRequest:
    """Build the point-of-use request from exact native and session values."""

    if type(request) is not InvocationRequest:
        raise TypeError("request must be exact native InvocationRequest")
    if type(outcome) is not InvocationOutcome:
        raise TypeError("outcome must be exact native InvocationOutcome")
    if type(requirement) is not ResolvedCapabilityRequirement:
        raise TypeError(
            "requirement must be exact native ResolvedCapabilityRequirement"
        )
    if type(session) is not KiteframeSessionContext:
        raise TypeError("session must be exact KiteframeSessionContext")
    envelope = SuspensionEnvelope.from_native(request, outcome)
    if (
        request.admission_id != session.admission_id
        or request.grant_digest != session.grant_digest
        or request.capability_name != requirement.name
        or request.capability_version != requirement.version
        or envelope.admission_id != session.admission_id
        or envelope.traceparent != session.trace_context.traceparent
    ):
        raise ValueError("suspension correlation does not match session authority")

    reference = _protected_evidence_ref(evidence_ref)
    return build_invocation_request_for_requirement(
        invocation_id=envelope.invocation_id,
        admission_id=envelope.admission_id,
        grant_digest=session.grant_digest,
        requirement=requirement,
        selected_resource=request.selected_resource,
        arguments=request.arguments,
        preconditions=request.preconditions,
        evidence_refs={envelope.evidence_kind: reference},
        traceparent=request.traceparent,
        tracestate=request.tracestate,
        baggage=request.baggage,
        idempotency_key=request.idempotency_key,
    )


__all__ = [
    "LangGraphSuspensionBridge",
    "SuspensionEnvelope",
    "build_resumed_invocation_request",
    "resume_command",
]
