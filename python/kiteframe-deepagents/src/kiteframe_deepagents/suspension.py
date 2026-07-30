"""Protected LangGraph interrupt and native invocation-resume mapping."""

from __future__ import annotations

import re
from dataclasses import asdict, dataclass
from typing import Literal, Protocol, runtime_checkable

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
PROTECTED_EVIDENCE_REFERENCE_TYPE = (
    "kiteframe.protected-evidence-reference"
)
_PROTECTED_REFERENCE_PATTERN = re.compile(
    r"(?:evidence-ref-[A-Za-z0-9][A-Za-z0-9._~-]{0,127}"
    r"|evidence://[A-Za-z0-9][A-Za-z0-9._~:/-]{0,255})"
)
_REFERENCE_ISSUER = object()


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
    if _PROTECTED_REFERENCE_PATTERN.fullmatch(reference) is None:
        raise ValueError("evidence_ref must be a protected reference")
    return reference


@runtime_checkable
class EvidenceReferenceResolver(Protocol):
    """Trusted resolver from an external handle to a protected reference."""

    async def resolve_evidence_reference(self, handle: str) -> str: ...


@dataclass(frozen=True, slots=True, init=False)
class ProtectedEvidenceReference:
    """Resolver-issued reference brand required by the resume API."""

    _reference: str

    def __init__(
        self,
        reference: str,
        *,
        _issuer: object | None = None,
    ) -> None:
        if _issuer is not _REFERENCE_ISSUER:
            raise TypeError(
                "protected evidence references must come from a resolver"
            )
        object.__setattr__(
            self,
            "_reference",
            _protected_evidence_ref(reference),
        )


async def resolve_protected_evidence_reference(
    handle: str,
    resolver: EvidenceReferenceResolver,
) -> ProtectedEvidenceReference:
    """Resolve an untrusted handle before any value enters a Command."""

    if type(handle) is not str or not handle:
        raise TypeError("evidence handle must be a non-empty exact string")
    if not isinstance(resolver, EvidenceReferenceResolver):
        raise TypeError("resolver must implement EvidenceReferenceResolver")
    reference = await resolver.resolve_evidence_reference(handle)
    return ProtectedEvidenceReference(
        _protected_evidence_ref(reference),
        _issuer=_REFERENCE_ISSUER,
    )


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
        if (
            type(resumed) is not dict
            or set(resumed) != {"reference", "type"}
            or resumed.get("type") != PROTECTED_EVIDENCE_REFERENCE_TYPE
        ):
            raise TypeError(
                "resume payload must be a protected evidence reference"
            )
        return _protected_evidence_ref(resumed.get("reference"))


def resume_command(evidence_ref: ProtectedEvidenceReference) -> Command:
    """Create the only public resume command accepted by the adapter."""

    if type(evidence_ref) is not ProtectedEvidenceReference:
        raise TypeError(
            "evidence_ref must be an exact ProtectedEvidenceReference"
        )
    return Command(
        resume={
            "reference": evidence_ref._reference,
            "type": PROTECTED_EVIDENCE_REFERENCE_TYPE,
        }
    )


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
    "EvidenceReferenceResolver",
    "ProtectedEvidenceReference",
    "SuspensionEnvelope",
    "build_resumed_invocation_request",
    "resolve_protected_evidence_reference",
    "resume_command",
]
