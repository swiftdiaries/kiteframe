"""Protected LangGraph interrupt and native invocation-resume mapping."""

from __future__ import annotations

import copy
import re
import time
from collections.abc import (
    AsyncIterator,
    Collection,
    Iterator,
    Sequence,
)
from dataclasses import asdict, dataclass, field
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
from langgraph.runtime import get_runtime
from langgraph.types import Command, Interrupt, interrupt

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
_NULL_TASK_ID = "00000000-0000-0000-0000-000000000000"
_LEGACY_SERIALIZED_REFERENCE_KEY = (
    "__kiteframe_resolver_issued_evidence_reference_v1__"
)
_SERIALIZED_REFERENCE_PREFIX = (
    b"\x00kiteframe-resolver-issued-evidence-reference-v3\x00"
)
_CREDENTIAL_VERSION = "kiteframe.evidence-resume.v1"


def _exact_non_empty(value: object, name: str) -> str:
    if type(value) is not str or not value:
        raise TypeError(f"{name} must be a non-empty exact string")
    return value


def _evidence_kind(value: object) -> EvidenceKind:
    if value not in {"confirmation", "approval", "consent"}:
        raise ValueError("suspension evidence kind is unsupported")
    return value  # type: ignore[return-value]


def _suspension_type(
    value: object,
) -> Literal["kiteframe.capability.suspension"]:
    if type(value) is not str or value != SUSPENSION_TYPE:
        raise ValueError("suspension type is unsupported")
    return SUSPENSION_TYPE


def _protected_evidence_ref(value: object) -> str:
    reference = _exact_non_empty(value, "evidence_ref")
    if _PROTECTED_REFERENCE_PATTERN.fullmatch(reference) is None:
        raise ValueError("evidence_ref must be a protected reference")
    return reference


@runtime_checkable
class EvidenceReferenceResolver(Protocol):
    """Deployment issuer from an external handle to an opaque credential."""

    async def resolve_evidence_reference(
        self,
        handle: str,
        suspension: SuspensionEnvelope,
    ) -> bytes: ...


@dataclass(frozen=True, slots=True)
class EvidenceResumeCredentialClaims:
    """Verified deployment claims carried by one opaque resume credential."""

    credential_version: str
    key_id: str
    nonce: str
    expires_at: int
    evidence_ref: str
    suspension: SuspensionEnvelope


@runtime_checkable
class EvidenceResumeCredentialVerifier(Protocol):
    """Restart-stable deployment verifier for opaque resume credentials."""

    def verify_evidence_resume_credential(
        self,
        credential: bytes,
    ) -> EvidenceResumeCredentialClaims: ...


@dataclass(frozen=True, slots=True, init=False)
class ProtectedEvidenceReference:
    """Resolver-issued reference brand required by the resume API."""

    _reference: str
    _credential: bytes = field(repr=False)
    _claims: EvidenceResumeCredentialClaims = field(repr=False)

    def __init__(
        self,
        reference: str,
        *,
        _credential: bytes | None = None,
        _claims: EvidenceResumeCredentialClaims | None = None,
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
        if type(_credential) is not bytes or not _credential:
            raise TypeError("protected evidence credential is invalid")
        if type(_claims) is not EvidenceResumeCredentialClaims:
            raise TypeError("protected evidence credential claims are invalid")
        object.__setattr__(self, "_credential", _credential)
        object.__setattr__(self, "_claims", _claims)


def _verified_credential_claims(
    credential: object,
    verifier: EvidenceResumeCredentialVerifier,
) -> EvidenceResumeCredentialClaims:
    if type(credential) is not bytes or not credential:
        raise TypeError("resume credential must be non-empty opaque bytes")
    if not isinstance(verifier, EvidenceResumeCredentialVerifier):
        raise TypeError(
            "resume credential verifier must implement "
            "EvidenceResumeCredentialVerifier"
        )
    try:
        claims = verifier.verify_evidence_resume_credential(credential)
    except Exception:
        raise TypeError(
            "resume payload must be a resolver-issued "
            "protected evidence reference"
        ) from None
    if (
        type(claims) is not EvidenceResumeCredentialClaims
        or type(claims.credential_version) is not str
        or claims.credential_version != _CREDENTIAL_VERSION
        or type(claims.key_id) is not str
        or not claims.key_id
        or type(claims.nonce) is not str
        or not claims.nonce
        or type(claims.expires_at) is not int
        or claims.expires_at <= int(time.time())
        or type(claims.suspension) is not SuspensionEnvelope
    ):
        raise TypeError(
            "resume payload must be a resolver-issued "
            "protected evidence reference"
        )
    try:
        SuspensionEnvelope.from_payload(claims.suspension)
    except (TypeError, ValueError):
        raise TypeError(
            "resume payload must be a resolver-issued "
            "protected evidence reference"
        ) from None
    _protected_evidence_ref(claims.evidence_ref)
    return claims


def _is_resolver_issued_reference(
    value: object,
    verifier: EvidenceResumeCredentialVerifier,
) -> bool:
    if type(value) is not ProtectedEvidenceReference:
        return False
    try:
        claims = _verified_credential_claims(value._credential, verifier)
    except TypeError:
        return False
    return claims == value._claims and claims.evidence_ref == value._reference


def _require_resolver_issued_reference(
    value: object,
    verifier: EvidenceResumeCredentialVerifier,
    *,
    suspension: SuspensionEnvelope | None = None,
) -> ProtectedEvidenceReference:
    if (
        type(value) is not ProtectedEvidenceReference
        or not _is_resolver_issued_reference(value, verifier)
        or (
            suspension is not None
            and value._claims.suspension != suspension
        )
    ):
        raise TypeError(
            "resume payload must be a resolver-issued "
            "protected evidence reference"
        )
    return value


async def resolve_protected_evidence_reference(
    handle: str,
    suspension: object,
    resolver: EvidenceReferenceResolver,
    verifier: EvidenceResumeCredentialVerifier,
) -> ProtectedEvidenceReference:
    """Resolve an untrusted handle before any value enters a Command."""

    if type(handle) is not str or not handle:
        raise TypeError("evidence handle must be a non-empty exact string")
    if not isinstance(resolver, EvidenceReferenceResolver):
        raise TypeError("resolver must implement EvidenceReferenceResolver")
    envelope = SuspensionEnvelope.from_payload(suspension)
    credential = await resolver.resolve_evidence_reference(
        handle,
        envelope,
    )
    claims = _verified_credential_claims(credential, verifier)
    if claims.suspension != envelope:
        raise TypeError(
            "resume credential does not match the requested suspension"
        )
    return ProtectedEvidenceReference(
        claims.evidence_ref,
        _credential=credential,
        _claims=claims,
        _issuer=_REFERENCE_ISSUER,
    )


def _encode_protected_reference(
    value: object,
    verifier: EvidenceResumeCredentialVerifier,
) -> object:
    if type(value) is ProtectedEvidenceReference:
        reference = _require_resolver_issued_reference(value, verifier)
        return (
            _SERIALIZED_REFERENCE_PREFIX
            + reference._credential
        )
    if type(value) is list:
        return [
            _encode_protected_reference(item, verifier)
            for item in value
        ]
    if type(value) is tuple:
        return tuple(
            _encode_protected_reference(item, verifier)
            for item in value
        )
    if type(value) is dict:
        return {
            key: _encode_protected_reference(item, verifier)
            for key, item in value.items()
        }
    return value


def _decode_protected_reference(
    value: object,
    verifier: EvidenceResumeCredentialVerifier,
) -> object:
    if type(value) is bytes and value.startswith(
        _SERIALIZED_REFERENCE_PREFIX
    ):
        credential = value[len(_SERIALIZED_REFERENCE_PREFIX) :]
        claims = _verified_credential_claims(credential, verifier)
        return ProtectedEvidenceReference(
            claims.evidence_ref,
            _credential=credential,
            _claims=claims,
            _issuer=_REFERENCE_ISSUER,
        )
    if type(value) is list:
        return [
            _decode_protected_reference(item, verifier)
            for item in value
        ]
    if type(value) is tuple:
        return tuple(
            _decode_protected_reference(item, verifier)
            for item in value
        )
    if type(value) is dict:
        if _LEGACY_SERIALIZED_REFERENCE_KEY in value:
            raise TypeError(
                "resume payload must be a resolver-issued "
                "protected evidence reference"
            )
        return {
            key: _decode_protected_reference(item, verifier)
            for key, item in value.items()
        }
    return value


@dataclass(frozen=True, slots=True)
class _ProtectedResumeSerializer:
    delegate: SerializerProtocol
    verifier: EvidenceResumeCredentialVerifier

    def dumps_typed(self, obj: Any) -> tuple[str, bytes]:
        return self.delegate.dumps_typed(
            _encode_protected_reference(obj, self.verifier)
        )

    def loads_typed(self, data: tuple[str, bytes]) -> Any:
        return _decode_protected_reference(
            self.delegate.loads_typed(data),
            self.verifier,
        )


class _ProtectedResumeCheckpointer(BaseCheckpointSaver[Any]):
    """Reject forged LangGraph resume writes before durable persistence."""

    __slots__ = ("delegate", "verifier")

    def __init__(
        self,
        delegate: BaseCheckpointSaver[Any],
        verifier: EvidenceResumeCredentialVerifier,
    ) -> None:
        if not isinstance(verifier, EvidenceResumeCredentialVerifier):
            raise TypeError(
                "resume credential verifier must implement "
                "EvidenceResumeCredentialVerifier"
            )
        protected_delegate = copy.copy(delegate)
        protected_delegate.serde = _ProtectedResumeSerializer(
            delegate.serde,
            verifier,
        )
        self.delegate = protected_delegate
        self.serde = protected_delegate.serde
        self.verifier = verifier

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
        return type(self)(
            source.with_allowlist(extra_allowlist),
            self.verifier,
        )

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

    def _validate_writes(
        self,
        config: RunnableConfig,
        writes: Sequence[tuple[str, Any]],
        task_id: str,
        expected_suspensions: frozenset[SuspensionEnvelope],
    ) -> None:
        configurable = config.get("configurable", {})
        thread_id = configurable.get("thread_id")
        checkpoint_id = configurable.get("checkpoint_id")

        def valid_reference(value: object) -> bool:
            if (
                type(value) is not ProtectedEvidenceReference
                or not _is_resolver_issued_reference(
                    value,
                    self.verifier,
                )
            ):
                return False
            claims = value._claims
            suspension = claims.suspension
            return (
                suspension.graph_thread_id == thread_id
                and suspension.graph_checkpoint_id == checkpoint_id
                and (
                    task_id == _NULL_TASK_ID
                    or suspension.graph_task_id == task_id
                )
                and suspension in expected_suspensions
            )

        for channel, value in writes:
            if (
                channel == _RESUME_CHANNEL
                and not (
                    (
                        task_id == _NULL_TASK_ID
                        and valid_reference(value)
                    )
                    or (
                        task_id != _NULL_TASK_ID
                        and type(value) is list
                        and bool(value)
                        and all(
                            valid_reference(item)
                            for item in value
                        )
                    )
                )
            ):
                raise TypeError(
                    "resume payload must be a resolver-issued "
                    "protected evidence reference"
                )

    @staticmethod
    def _checkpoint_suspensions(
        checkpoint: CheckpointTuple | None,
    ) -> frozenset[SuspensionEnvelope]:
        if checkpoint is None or checkpoint.pending_writes is None:
            return frozenset()
        suspensions: set[SuspensionEnvelope] = set()
        for _task_id, channel, value in checkpoint.pending_writes:
            if channel != "__interrupt__":
                continue
            items = (
                value
                if type(value) in {list, tuple}
                else (value,)
            )
            for item in items:
                if (
                    type(item) is Interrupt
                    and type(item.value) is dict
                    and item.value.get("type") == SUSPENSION_TYPE
                ):
                    suspensions.add(
                        SuspensionEnvelope.from_payload(item.value)
                    )
        return frozenset(suspensions)

    def put_writes(
        self,
        config: RunnableConfig,
        writes: Sequence[tuple[str, Any]],
        task_id: str,
        task_path: str = "",
    ) -> None:
        expected_suspensions = (
            self._checkpoint_suspensions(
                self.delegate.get_tuple(config)
            )
            if any(channel == _RESUME_CHANNEL for channel, _ in writes)
            else frozenset()
        )
        self._validate_writes(
            config,
            writes,
            task_id,
            expected_suspensions,
        )
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
        expected_suspensions = (
            self._checkpoint_suspensions(
                await self.delegate.aget_tuple(config)
            )
            if any(channel == _RESUME_CHANNEL for channel, _ in writes)
            else frozenset()
        )
        self._validate_writes(
            config,
            writes,
            task_id,
            expected_suspensions,
        )
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
    verifier: EvidenceResumeCredentialVerifier,
) -> BaseCheckpointSaver[Any]:
    """Guard public LangGraph resume writes without replacing the saver."""

    if not isinstance(checkpointer, BaseCheckpointSaver):
        raise TypeError(
            "suspendable checkpointer must be a public BaseCheckpointSaver"
        )
    return _ProtectedResumeCheckpointer(checkpointer, verifier)


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
    graph_thread_id: str
    graph_task_id: str
    graph_checkpoint_ns: str
    graph_checkpoint_id: str

    @classmethod
    def from_payload(cls, value: object) -> SuspensionEnvelope:
        """Validate the exact public interrupt payload."""

        if type(value) is cls:
            value = value.to_payload()
        expected = {
            "type",
            "invocation_id",
            "admission_id",
            "checkpoint_ref",
            "evidence_kind",
            "evidence_request_ref",
            "proposal_digest",
            "traceparent",
            "graph_thread_id",
            "graph_task_id",
            "graph_checkpoint_ns",
            "graph_checkpoint_id",
        }
        if type(value) is not dict or set(value) != expected:
            raise TypeError("suspension payload has an invalid exact shape")
        return cls(
            type=_suspension_type(value["type"]),
            invocation_id=_exact_non_empty(
                value["invocation_id"],
                "invocation_id",
            ),
            admission_id=_exact_non_empty(
                value["admission_id"],
                "admission_id",
            ),
            checkpoint_ref=_exact_non_empty(
                value["checkpoint_ref"],
                "checkpoint_ref",
            ),
            evidence_kind=_evidence_kind(value["evidence_kind"]),
            evidence_request_ref=_exact_non_empty(
                value["evidence_request_ref"],
                "evidence_request_ref",
            ),
            proposal_digest=_exact_non_empty(
                value["proposal_digest"],
                "proposal_digest",
            ),
            traceparent=_exact_non_empty(
                value["traceparent"],
                "traceparent",
            ),
            graph_thread_id=_exact_non_empty(
                value["graph_thread_id"],
                "graph_thread_id",
            ),
            graph_task_id=_exact_non_empty(
                value["graph_task_id"],
                "graph_task_id",
            ),
            graph_checkpoint_ns=_exact_non_empty(
                value["graph_checkpoint_ns"],
                "graph_checkpoint_ns",
            ),
            graph_checkpoint_id=_exact_non_empty(
                value["graph_checkpoint_id"],
                "graph_checkpoint_id",
            ),
        )

    def to_payload(self) -> dict[str, object]:
        """Return the exact reference-only public interrupt payload."""

        return asdict(self)

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
        execution_info = get_runtime().execution_info
        if (
            execution_info is None
            or type(execution_info.thread_id) is not str
            or not execution_info.thread_id
        ):
            raise ValueError("LangGraph suspension scope is unresolved")
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
            graph_thread_id=execution_info.thread_id,
            graph_task_id=_exact_non_empty(
                execution_info.task_id,
                "graph_task_id",
            ),
            graph_checkpoint_ns=_exact_non_empty(
                execution_info.checkpoint_ns,
                "graph_checkpoint_ns",
            ),
            graph_checkpoint_id=_exact_non_empty(
                execution_info.checkpoint_id,
                "graph_checkpoint_id",
            ),
        )


@dataclass(frozen=True, slots=True)
class LangGraphSuspensionBridge:
    """Map a native suspension to the public LangGraph interrupt primitive."""

    verifier: EvidenceResumeCredentialVerifier

    def __post_init__(self) -> None:
        if not isinstance(self.verifier, EvidenceResumeCredentialVerifier):
            raise TypeError(
                "resume credential verifier must implement "
                "EvidenceResumeCredentialVerifier"
            )

    async def suspend(
        self,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> str:
        envelope = SuspensionEnvelope.from_native(request, outcome)
        resumed = interrupt(envelope.to_payload())
        resumed = _require_resolver_issued_reference(
            resumed,
            self.verifier,
            suspension=envelope,
        )
        return _protected_evidence_ref(resumed._reference)


def resume_command(
    evidence_ref: ProtectedEvidenceReference,
    verifier: EvidenceResumeCredentialVerifier,
) -> Command:
    """Create the only public resume command accepted by the adapter."""

    if type(evidence_ref) is not ProtectedEvidenceReference:
        raise TypeError(
            "evidence_ref must be an exact ProtectedEvidenceReference"
        )
    _require_resolver_issued_reference(evidence_ref, verifier)
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
    "EvidenceResumeCredentialClaims",
    "EvidenceResumeCredentialVerifier",
    "EvidenceReferenceResolver",
    "LangGraphSuspensionBridge",
    "ProtectedEvidenceReference",
    "SuspensionEnvelope",
    "build_resumed_invocation_request",
    "resolve_protected_evidence_reference",
    "resume_command",
]
