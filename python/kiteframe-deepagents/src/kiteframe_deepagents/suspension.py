"""Protected LangGraph interrupt and native invocation-resume mapping."""

from __future__ import annotations

import copy
import re
from collections.abc import (
    AsyncIterator,
    Collection,
    Iterator,
    Sequence,
)
from dataclasses import asdict, dataclass
from typing import Any, Literal, Protocol, runtime_checkable

from kiteframe import (
    InvocationOutcome,
    InvocationRequest,
    ResolvedCapabilityRequirement,
    build_invocation_request_for_requirement,
)
from langchain_core.runnables import RunnableConfig
from langgraph.checkpoint.base import (
    BaseCheckpointSaver,
    ChannelVersions,
    Checkpoint,
    CheckpointMetadata,
    CheckpointTuple,
)
from langgraph.checkpoint.serde.base import SerializerProtocol
from langgraph.types import Command, interrupt

from .context import KiteframeSessionContext

SUSPENSION_TYPE = "kiteframe.capability.suspension"
INVALID_SUSPENSION = "KF-CAP-002: invalid capability suspension"
EvidenceKind = Literal["confirmation", "approval", "consent"]
_PROTECTED_REFERENCE_PATTERN = re.compile(
    r"(?:evidence-ref-[A-Za-z0-9][A-Za-z0-9._~-]{0,127}"
    r"|evidence://[A-Za-z0-9][A-Za-z0-9._~:/-]{0,255})"
)
_REFERENCE_ISSUER = object()
_RESUME_CHANNEL = "__resume__"
_SERIALIZED_REFERENCE_KEY = (
    "__kiteframe_resolver_issued_evidence_reference_v1__"
)


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


def _resolver_issued_resume(value: object) -> bool:
    if type(value) is ProtectedEvidenceReference:
        return True
    if type(value) is list:
        return bool(value) and all(_resolver_issued_resume(item) for item in value)
    if type(value) is tuple:
        return bool(value) and all(_resolver_issued_resume(item) for item in value)
    if type(value) is dict:
        return bool(value) and all(
            type(key) is str and _resolver_issued_resume(item)
            for key, item in value.items()
        )
    return False


def _encode_protected_reference(value: object) -> object:
    if type(value) is ProtectedEvidenceReference:
        return {_SERIALIZED_REFERENCE_KEY: value._reference}
    if type(value) is list:
        return [_encode_protected_reference(item) for item in value]
    if type(value) is tuple:
        return tuple(_encode_protected_reference(item) for item in value)
    if type(value) is dict:
        return {
            key: _encode_protected_reference(item)
            for key, item in value.items()
        }
    return value


def _decode_protected_reference(value: object) -> object:
    if (
        type(value) is dict
        and set(value) == {_SERIALIZED_REFERENCE_KEY}
    ):
        return ProtectedEvidenceReference(
            _protected_evidence_ref(value[_SERIALIZED_REFERENCE_KEY]),
            _issuer=_REFERENCE_ISSUER,
        )
    if type(value) is list:
        return [_decode_protected_reference(item) for item in value]
    if type(value) is tuple:
        return tuple(_decode_protected_reference(item) for item in value)
    if type(value) is dict:
        return {
            key: _decode_protected_reference(item)
            for key, item in value.items()
        }
    return value


@dataclass(frozen=True, slots=True)
class _ProtectedResumeSerializer:
    delegate: SerializerProtocol

    def dumps_typed(self, obj: Any) -> tuple[str, bytes]:
        return self.delegate.dumps_typed(
            _encode_protected_reference(obj)
        )

    def loads_typed(self, data: tuple[str, bytes]) -> Any:
        return _decode_protected_reference(
            self.delegate.loads_typed(data)
        )


class _ProtectedResumeCheckpointer(BaseCheckpointSaver[Any]):
    """Reject forged LangGraph resume writes before durable persistence."""

    __slots__ = ("delegate",)

    def __init__(self, delegate: BaseCheckpointSaver[Any]) -> None:
        protected_delegate = copy.copy(delegate)
        protected_delegate.serde = _ProtectedResumeSerializer(
            delegate.serde
        )
        self.delegate = protected_delegate
        self.serde = protected_delegate.serde

    def __getattr__(self, name: str) -> Any:
        return getattr(self.delegate, name)

    @property
    def config_specs(self) -> list[Any]:
        return self.delegate.config_specs

    def with_allowlist(
        self,
        extra_allowlist: Collection[tuple[str, ...]],
    ) -> _ProtectedResumeCheckpointer:
        source = copy.copy(self.delegate)
        serializer = self.serde
        if type(serializer) is not _ProtectedResumeSerializer:
            raise TypeError("protected resume serializer is unresolved")
        source.serde = serializer.delegate
        return type(self)(source.with_allowlist(extra_allowlist))

    def get_tuple(
        self,
        config: RunnableConfig,
    ) -> CheckpointTuple | None:
        return self.delegate.get_tuple(config)

    def list(
        self,
        config: RunnableConfig | None,
        *,
        filter: dict[str, Any] | None = None,
        before: RunnableConfig | None = None,
        limit: int | None = None,
    ) -> Iterator[CheckpointTuple]:
        return self.delegate.list(
            config,
            filter=filter,
            before=before,
            limit=limit,
        )

    def put(
        self,
        config: RunnableConfig,
        checkpoint: Checkpoint,
        metadata: CheckpointMetadata,
        new_versions: ChannelVersions,
    ) -> RunnableConfig:
        return self.delegate.put(
            config,
            checkpoint,
            metadata,
            new_versions,
        )

    @staticmethod
    def _validate_writes(writes: Sequence[tuple[str, Any]]) -> None:
        for channel, value in writes:
            if channel == _RESUME_CHANNEL and not _resolver_issued_resume(value):
                raise TypeError(
                    "resume payload must be a resolver-issued "
                    "protected evidence reference"
                )

    def put_writes(
        self,
        config: RunnableConfig,
        writes: Sequence[tuple[str, Any]],
        task_id: str,
        task_path: str = "",
    ) -> None:
        self._validate_writes(writes)
        self.delegate.put_writes(  # type: ignore[attr-defined]
            config,
            writes,
            task_id,
            task_path,
        )

    def delete_thread(self, thread_id: str) -> None:
        self.delegate.delete_thread(thread_id)

    def delete_for_runs(self, run_ids: Sequence[str]) -> None:
        self.delegate.delete_for_runs(run_ids)

    def copy_thread(
        self,
        source_thread_id: str,
        target_thread_id: str,
    ) -> None:
        self.delegate.copy_thread(source_thread_id, target_thread_id)

    def prune(
        self,
        thread_ids: Sequence[str],
        *,
        strategy: str = "keep_latest",
    ) -> None:
        self.delegate.prune(thread_ids, strategy=strategy)

    async def aget_tuple(
        self,
        config: RunnableConfig,
    ) -> CheckpointTuple | None:
        return await self.delegate.aget_tuple(config)

    async def alist(
        self,
        config: RunnableConfig | None,
        *,
        filter: dict[str, Any] | None = None,
        before: RunnableConfig | None = None,
        limit: int | None = None,
    ) -> AsyncIterator[CheckpointTuple]:
        async for checkpoint in self.delegate.alist(
            config,
            filter=filter,
            before=before,
            limit=limit,
        ):
            yield checkpoint

    async def aput(
        self,
        config: RunnableConfig,
        checkpoint: Checkpoint,
        metadata: CheckpointMetadata,
        new_versions: ChannelVersions,
    ) -> RunnableConfig:
        return await self.delegate.aput(
            config,
            checkpoint,
            metadata,
            new_versions,
        )

    async def aput_writes(
        self,
        config: RunnableConfig,
        writes: Sequence[tuple[str, Any]],
        task_id: str,
        task_path: str = "",
    ) -> None:
        self._validate_writes(writes)
        await self.delegate.aput_writes(  # type: ignore[attr-defined]
            config,
            writes,
            task_id,
            task_path,
        )

    async def adelete_thread(self, thread_id: str) -> None:
        await self.delegate.adelete_thread(thread_id)

    async def adelete_for_runs(self, run_ids: Sequence[str]) -> None:
        await self.delegate.adelete_for_runs(run_ids)

    async def acopy_thread(
        self,
        source_thread_id: str,
        target_thread_id: str,
    ) -> None:
        await self.delegate.acopy_thread(
            source_thread_id,
            target_thread_id,
        )

    async def aprune(
        self,
        thread_ids: Sequence[str],
        *,
        strategy: str = "keep_latest",
    ) -> None:
        await self.delegate.aprune(thread_ids, strategy=strategy)

    def get_next_version(self, current: Any, channel: None) -> Any:
        return self.delegate.get_next_version(current, channel)


def protect_resume_checkpointer(
    checkpointer: object,
) -> BaseCheckpointSaver[Any]:
    """Guard public LangGraph resume writes without replacing the saver."""

    if not isinstance(checkpointer, BaseCheckpointSaver):
        raise TypeError(
            "suspendable checkpointer must be a public BaseCheckpointSaver"
        )
    return _ProtectedResumeCheckpointer(checkpointer)


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
        if type(resumed) is not ProtectedEvidenceReference:
            raise TypeError(
                "resume payload must be a resolver-issued "
                "protected evidence reference"
            )
        return _protected_evidence_ref(resumed._reference)


def resume_command(evidence_ref: ProtectedEvidenceReference) -> Command:
    """Create the only public resume command accepted by the adapter."""

    if type(evidence_ref) is not ProtectedEvidenceReference:
        raise TypeError(
            "evidence_ref must be an exact ProtectedEvidenceReference"
        )
    return Command(resume=evidence_ref)


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
