"""Locked capability descriptors projected as async Deep Agents tools."""

from __future__ import annotations

import json
import secrets
import time
import uuid
from dataclasses import dataclass
from typing import Any, Protocol, runtime_checkable

from kiteframe import (
    EffectiveCapabilityGrant,
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
    ResolvedCapabilityRequirement,
    StatusRequest,
    load_invocation_outcome,
    load_invocation_outcome_for_request,
    load_invocation_request,
    load_invocation_status_for_request,
    load_status_request,
)
from kiteframe.provider import CapabilityInvoker
from langchain_core.tools import ArgsSchema, BaseTool, ToolException

from .context import KiteframeSessionContext

INVALID_PROVIDER_RESULT = "KF-CAP-002: invalid capability provider result"
PROVIDER_UNAVAILABLE = "KF-CAP-004: capability provider unavailable"
RESOURCE_DENIED = "KF-AUTH-003: capability resource is not granted"


def _canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


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
    ) -> object: ...


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

    request_wire = {
        "admissionId": session.admission_id,
        "arguments": arguments,
        "capability": {
            "name": requirement.name,
            "version": requirement.version,
        },
        "evidenceRefs": {},
        "grantDigest": grant_digest,
        "invocationId": f"invocation:{_uuid7()}",
        "preconditions": {},
        "selectedResource": resource,
        "traceContext": _trace_context_wire(session),
    }
    if idempotency_key is not None:
        request_wire["idempotencyKey"] = idempotency_key
    return load_invocation_request(_canonical_bytes(request_wire))


def _grant_projection(grant: EffectiveCapabilityGrant) -> tuple[object, ...]:
    return (
        grant.name,
        grant.version,
        grant.resources,
        grant.execution_modes,
        grant.maximum_effect,
        grant.expires_at,
        _canonical_bytes(grant.required_evidence),
        _canonical_bytes(grant.freshness),
        _canonical_bytes(grant.preconditions),
    )


class CapabilityTool(BaseTool):
    """Provider-backed tool derived only from one embedded locked descriptor."""

    name: str = ""
    description: str
    args_schema: ArgsSchema | None = None
    requirement: ResolvedCapabilityRequirement
    grant: EffectiveCapabilityGrant
    grant_digest: str
    invoker: CapabilityInvoker
    session: KiteframeSessionContext
    checkpoint_store: IdempotencyCheckpointStore
    suspension_bridge: CapabilitySuspensionBridge

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
        if outcome.diagnostic is not None:
            return f"{outcome.diagnostic.code}: {outcome.diagnostic.message}"
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
        status_request = load_status_request(
            _canonical_bytes(
                {
                    "invocationId": invocation.invocation_id,
                    "traceContext": _trace_context_wire(self.session),
                }
            )
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
            return await self.suspension_bridge.suspend(invocation, outcome)
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
            return await self.suspension_bridge.suspend(request, outcome)
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
                args_schema=input_schema,
                requirement=requirement,
                grant=grant,
                grant_digest=grant_digest,
                invoker=invoker,
                session=session,
                checkpoint_store=checkpoint_store,
                suspension_bridge=suspension_bridge,
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
