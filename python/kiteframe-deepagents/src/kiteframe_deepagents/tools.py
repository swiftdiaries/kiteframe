"""Locked capability descriptors projected as async Deep Agents tools."""

from __future__ import annotations

import secrets
import time
import uuid
from dataclasses import dataclass
from typing import Any, NoReturn, Protocol, runtime_checkable

from kiteframe import (
    EffectiveCapabilityGrant,
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
    ResolvedCapabilityRequirement,
    StatusRequest,
    build_invocation_request_for_requirement,
    build_status_request,
    load_invocation_outcome,
    load_invocation_outcome_for_request,
    load_invocation_status_for_request,
)
from kiteframe.provider import CapabilityInvoker
from langchain_core.tools import ArgsSchema, BaseTool, ToolException
from pydantic import ConfigDict, Field

from .context import KiteframeSessionContext

INVALID_PROVIDER_RESULT = "KF-CAP-002: invalid capability provider result"
PROVIDER_UNAVAILABLE = "KF-CAP-004: capability provider unavailable"
RESOURCE_DENIED = "KF-AUTH-003: capability resource is not granted"
INVOCATION_DENIED = "KF-AUTH-003: capability invocation denied"
OUTCOME_RECONCILIATION_REQUIRED = (
    "KF-CAP-003: capability outcome requires reconciliation"
)
SUSPENSION_DID_NOT_INTERRUPT = (
    "KF-CAP-005: suspension bridge did not interrupt"
)


def _uuid7() -> uuid.UUID:
    """Create an RFC 9562 UUIDv7 without requiring Python 3.14."""

    timestamp_ms = (time.time_ns() // 1_000_000) & ((1 << 48) - 1)
    random_bits = int.from_bytes(secrets.token_bytes(10), "big") & (
        (1 << 74) - 1
    )
    random_a = random_bits >> 62
    random_b = random_bits & ((1 << 62) - 1)
    value = (
        (timestamp_ms << 80)
        | (7 << 76)
        | (random_a << 64)
        | (0b10 << 62)
        | random_b
    )
    return uuid.UUID(int=value)


@dataclass(frozen=True, slots=True)
class PersistedIdempotencyKey:
    """The durable scope written before an effectful provider call."""

    actor: str
    capability_name: str
    capability_version: str
    key: str
    resource: str
    semantic_operation: str
    session: str
    task: str


@runtime_checkable
class IdempotencyCheckpointStore(Protocol):
    """Durable session-checkpoint boundary for caller-generated keys."""

    async def persist_idempotency_key(
        self,
        record: PersistedIdempotencyKey,
    ) -> None: ...


@runtime_checkable
class CapabilitySuspensionBridge(Protocol):
    """Bridge from a validated native suspension to runtime interruption."""

    async def suspend(
        self,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> NoReturn: ...


class _FrozenDict(dict[str, Any]):
    def _immutable(self, *args: object, **kwargs: object) -> NoReturn:
        del args, kwargs
        raise TypeError("locked tool schema is immutable")

    __setitem__ = _immutable
    __delitem__ = _immutable
    __ior__ = _immutable
    clear = _immutable
    pop = _immutable
    popitem = _immutable
    setdefault = _immutable  # pyright: ignore[reportAssignmentType]
    update = _immutable  # pyright: ignore[reportAssignmentType]

    def __deepcopy__(self, memo: dict[int, object]) -> _FrozenDict:
        del memo
        return self


class _FrozenList(list[Any]):
    def _immutable(self, *args: object, **kwargs: object) -> NoReturn:
        del args, kwargs
        raise TypeError("locked tool schema is immutable")

    __setitem__ = _immutable
    __delitem__ = _immutable
    __iadd__ = _immutable
    __imul__ = _immutable
    append = _immutable
    clear = _immutable
    extend = _immutable
    insert = _immutable
    pop = _immutable
    remove = _immutable
    reverse = _immutable
    sort = _immutable  # pyright: ignore[reportAssignmentType]

    def __deepcopy__(self, memo: dict[int, object]) -> _FrozenList:
        del memo
        return self


def _freeze_json(value: Any) -> Any:
    if isinstance(value, dict):
        return _FrozenDict(
            {key: _freeze_json(item) for key, item in value.items()}
        )
    if isinstance(value, list):
        return _FrozenList(_freeze_json(item) for item in value)
    return value


def _comparable_json(value: Any) -> object:
    if isinstance(value, dict):
        return tuple(
            (key, _comparable_json(item))
            for key, item in sorted(value.items())
        )
    if isinstance(value, (list, tuple)):
        return tuple(_comparable_json(item) for item in value)
    return value


def _trace_context_wire(session: KiteframeSessionContext) -> dict[str, Any]:
    trace = session.trace_context
    wire: dict[str, Any] = {"traceparent": trace.traceparent}
    if trace.tracestate is not None:
        wire["tracestate"] = trace.tracestate
    if trace.baggage:
        wire["baggage"] = dict(trace.baggage)
    return wire


def _requires_idempotency(
    requirement: ResolvedCapabilityRequirement,
) -> bool:
    idempotency = requirement.descriptor.idempotency
    if not isinstance(idempotency, dict):
        raise TypeError("locked idempotency contract must be a native mapping")
    kind = idempotency.get("kind")
    if kind == "required":
        return True
    if kind == "none":
        return False
    raise TypeError("locked idempotency contract has an unsupported kind")


def build_native_invocation_request(
    *,
    requirement: ResolvedCapabilityRequirement,
    grant: EffectiveCapabilityGrant,
    grant_digest: str,
    session: KiteframeSessionContext,
    resource: str,
    arguments: dict[str, Any],
    idempotency_key: str | None,
) -> InvocationRequest:
    """Build one immutable native request from closed Wave 3R inputs."""

    if not isinstance(requirement, ResolvedCapabilityRequirement):
        raise TypeError(
            "requirement must be native ResolvedCapabilityRequirement"
        )
    if not isinstance(grant, EffectiveCapabilityGrant):
        raise TypeError("grant must be native EffectiveCapabilityGrant")
    if not isinstance(session, KiteframeSessionContext):
        raise TypeError("session must be KiteframeSessionContext")
    if (requirement.name, requirement.version) != (
        grant.name,
        grant.version,
    ):
        raise ValueError("requirement and effective grant do not match")
    if grant_digest != session.grant_digest:
        raise ValueError("grant digest does not match the session")

    trace = _trace_context_wire(session)
    return build_invocation_request_for_requirement(
        invocation_id=f"invocation:{_uuid7()}",
        admission_id=session.admission_id,
        grant_digest=grant_digest,
        requirement=requirement,
        selected_resource=resource,
        arguments=arguments,
        preconditions={},
        evidence_refs={},
        traceparent=trace["traceparent"],
        tracestate=trace.get("tracestate"),
        baggage=trace.get("baggage", {}),
        idempotency_key=idempotency_key,
    )


def _grant_projection(grant: EffectiveCapabilityGrant) -> tuple[object, ...]:
    return (
        grant.name,
        grant.version,
        grant.resources,
        grant.execution_modes,
        grant.maximum_effect,
        grant.expires_at,
        _comparable_json(grant.required_evidence),
        _comparable_json(grant.freshness),
        _comparable_json(grant.preconditions),
    )


@dataclass(frozen=True, slots=True)
class _CapabilityAuthoritySnapshot:
    requirement: ResolvedCapabilityRequirement
    grant: EffectiveCapabilityGrant
    grant_digest: str
    session: KiteframeSessionContext


class CapabilityTool(BaseTool):
    """Provider-backed tool derived only from one embedded locked descriptor."""

    model_config = ConfigDict(arbitrary_types_allowed=True, frozen=True)

    name: str = ""
    description: str
    args_schema: ArgsSchema | None = None
    invoker: CapabilityInvoker
    checkpoint_store: IdempotencyCheckpointStore
    suspension_bridge: CapabilitySuspensionBridge
    authority_snapshot: _CapabilityAuthoritySnapshot = Field(
        exclude=True,
        repr=False,
    )

    def model_post_init(self, __context: Any) -> None:
        del __context
        schema = self.args_schema
        if isinstance(schema, dict):
            object.__setattr__(self, "args_schema", _freeze_json(schema))

    @property
    def requirement(self) -> ResolvedCapabilityRequirement:
        return self.authority_snapshot.requirement

    @property
    def grant(self) -> EffectiveCapabilityGrant:
        return self.authority_snapshot.grant

    @property
    def grant_digest(self) -> str:
        return self.authority_snapshot.grant_digest

    @property
    def session(self) -> KiteframeSessionContext:
        return self.authority_snapshot.session

    @property
    def descriptor_digest(self) -> str:
        return self.requirement.descriptor_digest

    def _select_resource(self, selected: object) -> str:
        allowed = tuple(
            resource
            for resource in self.requirement.resources
            if resource in self.grant.resources
        )
        if selected is None:
            if len(allowed) != 1:
                raise ToolException(RESOURCE_DENIED)
            return allowed[0]
        if not isinstance(selected, str) or selected not in allowed:
            raise ToolException(RESOURCE_DENIED)
        return selected

    def _new_idempotency_key(self) -> str | None:
        if not _requires_idempotency(self.requirement):
            return None
        return str(_uuid7())

    async def _persist_idempotency_key(
        self,
        key: str | None,
        resource: str,
    ) -> None:
        if key is None:
            return
        await self.checkpoint_store.persist_idempotency_key(
            PersistedIdempotencyKey(
                actor=self.session.actor,
                capability_name=self.requirement.name,
                capability_version=self.requirement.version,
                key=key,
                resource=resource,
                semantic_operation=self.requirement.name,
                session=self.session.session,
                task=self.session.task,
            )
        )

    @staticmethod
    def _stable_failure(outcome: InvocationOutcome | InvocationStatus) -> str:
        if outcome.error is not None:
            return f"{outcome.error.code}: {outcome.error.message}"
        if outcome.status == "denied":
            return INVOCATION_DENIED
        if outcome.status == "outcome_unknown":
            return OUTCOME_RECONCILIATION_REQUIRED
        return INVALID_PROVIDER_RESULT

    def _validated_outcome(
        self,
        request: InvocationRequest,
        outcome: object,
    ) -> InvocationOutcome:
        if not isinstance(outcome, InvocationOutcome):
            raise ToolException(INVALID_PROVIDER_RESULT)
        try:
            return load_invocation_outcome_for_request(
                outcome.canonical_json(),
                request,
                self.requirement,
            )
        except Exception:
            raise ToolException(INVALID_PROVIDER_RESULT) from None

    def _validated_status(
        self,
        status_request: StatusRequest,
        invocation: InvocationRequest,
        status: object,
    ) -> InvocationStatus:
        if not isinstance(status, InvocationStatus):
            raise ToolException(INVALID_PROVIDER_RESULT)
        try:
            return load_invocation_status_for_request(
                status.canonical_json(),
                status_request,
                invocation,
                self.requirement,
            )
        except Exception:
            raise ToolException(INVALID_PROVIDER_RESULT) from None

    async def _status(
        self,
        invocation: InvocationRequest,
    ) -> InvocationStatus:
        trace = _trace_context_wire(self.session)
        status_request = build_status_request(
            invocation_id=invocation.invocation_id,
            traceparent=trace["traceparent"],
            tracestate=trace.get("tracestate"),
            baggage=trace.get("baggage", {}),
        )
        try:
            status = await self.invoker.status(
                status_request,
                invocation,
                self.requirement,
            )
        except Exception:
            raise ToolException(PROVIDER_UNAVAILABLE) from None
        return self._validated_status(status_request, invocation, status)

    async def _suspend(
        self,
        invocation: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> NoReturn:
        await self.suspension_bridge.suspend(invocation, outcome)
        raise ToolException(SUSPENSION_DID_NOT_INTERRUPT)

    async def _resolve_status(
        self,
        invocation: InvocationRequest,
        status: InvocationStatus,
    ) -> Any:
        if status.status == "succeeded":
            return status.result
        if status.status in {"failed", "denied", "outcome_unknown"}:
            raise ToolException(self._stable_failure(status))
        if status.status == "pending":
            return {
                "invocation_id": status.invocation_id,
                "status": "deferred",
            }
        if status.status == "suspended":
            outcome = load_invocation_outcome(status.canonical_json())
            await self._suspend(invocation, outcome)
        raise ToolException(INVALID_PROVIDER_RESULT)

    async def _resolve_outcome(
        self,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> Any:
        if outcome.status == "succeeded":
            return outcome.result
        if outcome.status in {"failed", "denied"}:
            raise ToolException(self._stable_failure(outcome))
        if outcome.status == "suspended":
            await self._suspend(request, outcome)
        if outcome.status in {"deferred", "outcome_unknown"}:
            status = await self._status(request)
            return await self._resolve_status(request, status)
        raise ToolException(INVALID_PROVIDER_RESULT)

    async def _arun(self, **arguments: Any) -> Any:
        resource = self._select_resource(arguments.pop("_resource", None))
        idempotency_key = self._new_idempotency_key()
        try:
            request = build_native_invocation_request(
                requirement=self.requirement,
                grant=self.grant,
                grant_digest=self.grant_digest,
                session=self.session,
                resource=resource,
                arguments=arguments,
                idempotency_key=idempotency_key,
            )
        except Exception:
            raise ToolException(
                "KF-CAP-002: invalid capability invocation"
            ) from None
        await self._persist_idempotency_key(idempotency_key, resource)
        try:
            outcome = await self.invoker.invoke(request)
        except Exception:
            raise ToolException(PROVIDER_UNAVAILABLE) from None
        return await self._resolve_outcome(
            request,
            self._validated_outcome(request, outcome),
        )

    def _run(self, **arguments: Any) -> Any:
        del arguments
        raise RuntimeError("Kiteframe capability tools require async invocation")


def build_capability_tools(
    requirements: tuple[ResolvedCapabilityRequirement, ...],
    grants: tuple[EffectiveCapabilityGrant, ...],
    *,
    grant_digest: str,
    invoker: CapabilityInvoker,
    session: KiteframeSessionContext,
    checkpoint_store: IdempotencyCheckpointStore,
    suspension_bridge: CapabilitySuspensionBridge,
) -> tuple[CapabilityTool, ...]:
    """Map exact effective grants to exact embedded locked descriptors."""

    if not isinstance(requirements, tuple) or not all(
        isinstance(requirement, ResolvedCapabilityRequirement)
        for requirement in requirements
    ):
        raise TypeError(
            "requirements must be a tuple of native "
            "ResolvedCapabilityRequirement"
        )
    if not isinstance(grants, tuple) or not all(
        isinstance(grant, EffectiveCapabilityGrant) for grant in grants
    ):
        raise TypeError(
            "grants must be a tuple of native EffectiveCapabilityGrant"
        )
    if not isinstance(session, KiteframeSessionContext):
        raise TypeError("session must be KiteframeSessionContext")
    if grant_digest != session.grant_digest:
        raise ValueError("grant digest does not match the session")
    if sorted(_grant_projection(grant) for grant in grants) != sorted(
        _grant_projection(grant) for grant in session.grants
    ):
        raise ValueError("effective grants do not match the session")
    if not isinstance(invoker, CapabilityInvoker):
        raise TypeError("invoker must implement native CapabilityInvoker")
    if not isinstance(checkpoint_store, IdempotencyCheckpointStore):
        raise TypeError("checkpoint_store must persist idempotency keys")
    if not isinstance(suspension_bridge, CapabilitySuspensionBridge):
        raise TypeError("suspension_bridge must handle native suspension")

    grant_by_identity: dict[
        tuple[str, str],
        EffectiveCapabilityGrant,
    ] = {}
    for grant in grants:
        identity = (grant.name, grant.version)
        if identity in grant_by_identity:
            raise ValueError("effective grants contain duplicate capability")
        grant_by_identity[identity] = grant

    tools: list[CapabilityTool] = []
    seen_requirements: set[tuple[str, str]] = set()
    for requirement in requirements:
        identity = (requirement.name, requirement.version)
        if identity in seen_requirements:
            raise ValueError("requirements contain duplicate capability")
        seen_requirements.add(identity)
        grant = grant_by_identity.pop(identity, None)
        if grant is None:
            if requirement.required:
                raise ValueError("required capability has no effective grant")
            continue
        descriptor = requirement.descriptor
        input_schema = descriptor.input_schema
        if not isinstance(input_schema, dict):
            raise TypeError("locked input schema must be a native mapping")
        tools.append(
            CapabilityTool(
                name=requirement.name,
                description=descriptor.summary,
                args_schema=_freeze_json(input_schema),
                invoker=invoker,
                checkpoint_store=checkpoint_store,
                suspension_bridge=suspension_bridge,
                authority_snapshot=_CapabilityAuthoritySnapshot(
                    requirement=requirement,
                    grant=grant,
                    grant_digest=grant_digest,
                    session=session,
                ),
                handle_tool_error=True,
            )
        )
    if grant_by_identity:
        raise ValueError("effective grant has no resolved requirement")
    return tuple(tools)


__all__ = [
    "CapabilitySuspensionBridge",
    "CapabilityTool",
    "IdempotencyCheckpointStore",
    "PersistedIdempotencyKey",
    "build_capability_tools",
    "build_native_invocation_request",
]
