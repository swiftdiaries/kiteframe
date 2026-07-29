from __future__ import annotations

import copy
import hashlib
import json
import uuid
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, NoReturn

import pytest
from kiteframe import (
    EffectiveCapabilityGrant,
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
    ResolvedCapabilityRequirement,
    StatusRequest,
    load_capability_grant_set,
    load_invocation_outcome,
    load_invocation_status,
    load_resolved_agent,
)
from pydantic import ValidationError

import kiteframe_deepagents.tools as tools_module
from kiteframe_deepagents.context import (
    KiteframeSessionContext,
    KiteframeTraceContext,
)
from kiteframe_deepagents.tools import (
    CapabilitySuspensionBridge,
    CapabilityTool,
    IdempotencyCheckpointStore,
    PersistedIdempotencyKey,
    build_capability_tools,
)

WORKSPACE = Path(__file__).resolve().parents[3]
VALID_TRACEPARENT = (
    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
)
RESOURCE = "tenant:t1/case:case-1"
OutcomeFactory = Callable[[InvocationRequest], InvocationOutcome]
StatusFactory = Callable[
    [StatusRequest, InvocationRequest],
    InvocationStatus,
]


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def canonical_digest(domain: bytes, value: object) -> str:
    return hashlib.sha256(domain + canonical_bytes(value)).hexdigest()


def _hash_domain(domain: bytes, chunks: Iterable[bytes]) -> bytes:
    hasher = hashlib.sha256()
    hasher.update(b"kiteframe:v1\0")
    hasher.update(len(domain).to_bytes(8, "big"))
    hasher.update(domain)
    for chunk in chunks:
        hasher.update(len(chunk).to_bytes(8, "big"))
        hasher.update(chunk)
    return hasher.digest()


def _canonical_component(domain: bytes, value: object) -> bytes:
    return _hash_domain(domain, [canonical_bytes(value)])


def _resolved_digest(resolved: dict[str, Any]) -> str:
    components = [
        _canonical_component(
            b"resolved/identity",
            [resolved["schemaVersion"], resolved["packageIdentity"]],
        ),
        _hash_domain(
            b"resolved/portable",
            [bytes.fromhex(resolved["portableDigest"])],
        ),
        _hash_domain(
            b"resolved/lock",
            [bytes.fromhex(resolved["lockDigest"])],
        ),
        _canonical_component(
            b"resolved/catalog",
            [resolved["catalogIdentity"], resolved["catalogDigest"]],
        ),
        _hash_domain(
            b"resolved/binding",
            [bytes.fromhex(resolved["bindingDigest"])],
        ),
        _canonical_component(b"resolved/prompts", resolved["prompts"]),
        _canonical_component(b"resolved/skills", resolved["skills"]),
        _canonical_component(
            b"resolved/features",
            [resolved["requiredFeatures"], resolved["optionalFeatures"]],
        ),
        _canonical_component(b"resolved/models", resolved["models"]),
        _canonical_component(
            b"resolved/capabilities",
            resolved["capabilityRequirements"],
        ),
        _canonical_component(b"resolved/children", resolved["subagents"]),
        _canonical_component(
            b"resolved/content-capture",
            resolved["contentCapture"],
        ),
        _canonical_component(
            b"resolved/report",
            resolved["compilationReport"],
        ),
    ]
    return _hash_domain(b"resolved-agent", components).hex()


def comment_requirement() -> ResolvedCapabilityRequirement:
    resolved = json.loads(
        (WORKSPACE / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )
    locked = resolved["capabilityRequirements"][0]["lockedCapability"]
    descriptor = {
        "approval": {"kind": "none"},
        "confirmation": {"kind": "none"},
        "consent": {"kind": "none"},
        "effect": "reversible_write",
        "executionModes": ["immediate", "deferred", "suspendable"],
        "freshness": {
            "maxAdmissionAgeSeconds": None,
            "maxInputAgeSeconds": None,
            "policyRevisionRequired": False,
        },
        "idempotency": {
            "kind": "required",
            "retention_seconds": 86400,
            "scope": "actor_capability_resource_operation",
        },
        "identity": {"name": "cases.comment", "version": "1.0.0"},
        "inputSchema": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "additionalProperties": False,
            "properties": {
                "body": {"type": "string"},
                "case_id": {"type": "string"},
            },
            "required": ["body", "case_id"],
            "type": "object",
        },
        "outputSchema": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "additionalProperties": False,
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"],
            "type": "object",
        },
        "preconditions": [],
        "resourceSelectorSchema": {"type": "string"},
        "stableErrors": [
            {
                "category": "conflict",
                "code": "COMMENT_REJECTED",
                "message": "comment rejected",
                "retry": "never",
            }
        ],
        "summary": "Add a comment to a case",
    }
    descriptor["descriptorDigest"] = hashlib.sha256(
        canonical_bytes(descriptor)
    ).hexdigest()
    locked["descriptor"] = descriptor
    locked["descriptorDigest"] = descriptor["descriptorDigest"]
    locked["identity"] = copy.deepcopy(descriptor["identity"])
    resolved["capabilityRequirements"][0]["resources"] = [RESOURCE]
    resolved["resolvedDigest"] = _resolved_digest(resolved)
    return load_resolved_agent(
        canonical_bytes(resolved)
    ).capability_requirements[0]


def case_read_requirement() -> ResolvedCapabilityRequirement:
    resolved = json.loads(
        (WORKSPACE / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )
    resolved["capabilityRequirements"][0]["resources"] = [RESOURCE]
    resolved["resolvedDigest"] = _resolved_digest(resolved)
    return load_resolved_agent(
        canonical_bytes(resolved)
    ).capability_requirements[0]


def grant_set_values() -> dict[str, Any]:
    authority_entries = [{"revision": "7", "source": "policy"}]
    authority_revisions = {
        "authorityRevisionDigest": canonical_digest(
            b"kiteframe:authority-revision-set:v1\0",
            authority_entries,
        ),
        "entries": authority_entries,
    }
    values = {
        "actor": "actor:alice",
        "admissionId": "adm-1",
        "admissionRequestDigest": "09" * 32,
        "agent": "agent:case-worker",
        "authorityRevisions": authority_revisions,
        "catalogDigest": "01" * 32,
        "catalogIdentity": {
            "name": "provider.test",
            "revision": "revision-1",
        },
        "expiresAt": 4102444800,
        "grants": [
            {
                "capability": {"name": "cases.read", "version": "1.2.0"},
                "executionModes": ["immediate"],
                "expiresAt": 4102444700,
                "freshness": {
                    "maxAdmissionAgeSeconds": None,
                    "maxInputAgeSeconds": None,
                    "policyRevisionRequired": False,
                },
                "maximumEffect": "read_only",
                "preconditions": [],
                "requiredEvidence": {
                    "approval": {"kind": "none"},
                    "confirmation": {"kind": "none"},
                    "consent": {"kind": "none"},
                },
                "resources": [RESOURCE],
            },
            {
                "capability": {
                    "name": "cases.comment",
                    "version": "1.0.0",
                },
                "executionModes": ["immediate", "deferred", "suspendable"],
                "expiresAt": 4102444700,
                "freshness": {
                    "maxAdmissionAgeSeconds": None,
                    "maxInputAgeSeconds": None,
                    "policyRevisionRequired": False,
                },
                "maximumEffect": "reversible_write",
                "preconditions": [],
                "requiredEvidence": {
                    "approval": {"kind": "none"},
                    "confirmation": {"kind": "none"},
                    "consent": {"kind": "none"},
                },
                "resources": [RESOURCE],
            },
        ],
        "issuedAt": 100,
        "optionalDenials": [],
        "policyRevision": "policy:7",
        "session": "session:1",
        "task": "task:triage",
    }
    values["grants"].sort(
        key=lambda grant: (
            grant["capability"]["name"],
            grant["capability"]["version"],
        )
    )
    values["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        values,
    )
    return values


def succeeded(result: object) -> OutcomeFactory:
    def factory(request: InvocationRequest) -> InvocationOutcome:
        return load_invocation_outcome(
            canonical_bytes(
                {
                    "invocation_id": request.invocation_id,
                    "result": result,
                    "status": "succeeded",
                }
            )
        )

    return factory


def succeeded_status(result: object) -> StatusFactory:
    def factory(
        request: StatusRequest,
        invocation: InvocationRequest,
    ) -> InvocationStatus:
        assert request.invocation_id == invocation.invocation_id
        return load_invocation_status(
            canonical_bytes(
                {
                    "invocation_id": request.invocation_id,
                    "result": result,
                    "status": "succeeded",
                }
            )
        )

    return factory


def outcome_unknown(request: InvocationRequest) -> InvocationOutcome:
    return load_invocation_outcome(
        canonical_bytes(
            {
                "diagnostic": {
                    "category": "capability",
                    "code": "KF-CAP-003",
                    "details": {},
                    "help": None,
                    "message": "status is required",
                    "package_path": None,
                    "retry": "status_first",
                    "severity": "error",
                    "source_range": None,
                    "stage": "invoke",
                },
                "invocation_id": request.invocation_id,
                "status": "outcome_unknown",
            }
        )
    )


def deferred(request: InvocationRequest) -> InvocationOutcome:
    return load_invocation_outcome(
        canonical_bytes(
            {
                "invocation_id": request.invocation_id,
                "status": "deferred",
            }
        )
    )


def pending(
    request: StatusRequest,
    invocation: InvocationRequest,
) -> InvocationStatus:
    assert request.invocation_id == invocation.invocation_id
    return load_invocation_status(
        canonical_bytes(
            {
                "invocation_id": request.invocation_id,
                "status": "pending",
            }
        )
    )


def failed(request: InvocationRequest) -> InvocationOutcome:
    return load_invocation_outcome(
        canonical_bytes(
            {
                "error": {
                    "category": "conflict",
                    "code": "COMMENT_REJECTED",
                    "message": "comment rejected",
                    "retry": "never",
                },
                "invocation_id": request.invocation_id,
                "status": "failed",
            }
        )
    )


def denied(request: InvocationRequest) -> InvocationOutcome:
    return denied_with_message("invocation denied")(request)


def denied_with_message(message: str) -> OutcomeFactory:
    def factory(request: InvocationRequest) -> InvocationOutcome:
        return load_invocation_outcome(
            canonical_bytes(
                {
                    "diagnostic": {
                        "category": "authorization",
                        "code": "KF-AUTH-003",
                        "details": {},
                        "help": None,
                        "message": message,
                        "package_path": None,
                        "retry": "never",
                        "severity": "error",
                        "source_range": None,
                        "stage": "invoke",
                    },
                    "invocation_id": request.invocation_id,
                    "status": "denied",
                }
            )
        )

    return factory


def outcome_unknown_status(message: str) -> StatusFactory:
    def factory(
        request: StatusRequest,
        invocation: InvocationRequest,
    ) -> InvocationStatus:
        assert request.invocation_id == invocation.invocation_id
        return load_invocation_status(
            canonical_bytes(
                {
                    "diagnostic": {
                        "category": "capability",
                        "code": "KF-CAP-003",
                        "details": {},
                        "help": None,
                        "message": message,
                        "package_path": None,
                        "retry": "status_first",
                        "severity": "error",
                        "source_range": None,
                        "stage": "invoke",
                    },
                    "invocation_id": request.invocation_id,
                    "status": "outcome_unknown",
                }
            )
        )

    return factory


def effect_proposal_digest(request: InvocationRequest) -> str:
    arguments_digest = canonical_digest(
        b"kiteframe:effect-arguments:v1\0",
        request.arguments,
    )
    preconditions_digest = canonical_digest(
        b"kiteframe:effect-preconditions:v1\0",
        request.preconditions,
    )
    return canonical_digest(
        b"kiteframe:effect-proposal:v1\0",
        {
            "admissionId": request.admission_id,
            "argumentsDigest": arguments_digest,
            "capability": {
                "name": request.capability_name,
                "version": request.capability_version,
            },
            "effect": "reversible_write",
            "grantDigest": request.grant_digest,
            "idempotencyKey": request.idempotency_key,
            "invocationId": request.invocation_id,
            "preconditionsDigest": preconditions_digest,
            "selectedResource": request.selected_resource,
        },
    )


def suspended(request: InvocationRequest) -> InvocationOutcome:
    return load_invocation_outcome(
        canonical_bytes(
            {
                "invocation_id": request.invocation_id,
                "status": "suspended",
                "suspension": {
                    "checkpointRef": "checkpoint:opaque:1",
                    "evidenceKind": "approval",
                    "evidenceRequestRef": "evidence-request:opaque:1",
                    "proposalDigest": effect_proposal_digest(request),
                },
            }
        )
    )


def suspended_status(
    request: StatusRequest,
    invocation: InvocationRequest,
) -> InvocationStatus:
    assert request.invocation_id == invocation.invocation_id
    native_outcome = suspended(invocation)
    return load_invocation_status(native_outcome.canonical_json())


@dataclass
class FakeCheckpointStore(IdempotencyCheckpointStore):
    events: list[str]
    records: list[PersistedIdempotencyKey]

    async def persist_idempotency_key(
        self,
        record: PersistedIdempotencyKey,
    ) -> None:
        self.records.append(record)
        self.events.append(f"persist:{record.key}")


class SuspensionInterruptForTest(Exception):
    pass


@dataclass
class FakeSuspensionBridge(CapabilitySuspensionBridge):
    calls: list[tuple[InvocationRequest, InvocationOutcome]]

    async def suspend(
        self,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> NoReturn:
        self.calls.append((request, outcome))
        suspension = outcome.suspension
        assert suspension is not None
        raise SuspensionInterruptForTest(suspension.checkpoint_ref)


@dataclass
class ReturningSuspensionBridge(CapabilitySuspensionBridge):
    async def suspend(
        self,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> NoReturn:
        del request, outcome
        return {  # type: ignore[reportReturnType]
            "evidence_request_ref": "evidence-request:provider-secret",
            "status": "suspended",
        }


class FakeInvoker:
    def __init__(self, events: list[str]) -> None:
        self.calls: list[str] = []
        self.events = events
        self.outcomes: list[OutcomeFactory] = [succeeded({"ok": True})]
        self.requests: list[InvocationRequest] = []
        self.status_requests: list[StatusRequest] = []
        self.statuses: list[StatusFactory] = []

    async def invoke(self, request: InvocationRequest) -> InvocationOutcome:
        self.requests.append(request)
        call = f"invoke:{request.idempotency_key}"
        self.calls.append(call)
        self.events.append(call)
        return self.outcomes.pop(0)(request)

    async def status(
        self,
        request: StatusRequest,
        invocation: InvocationRequest,
        requirement: ResolvedCapabilityRequirement,
    ) -> InvocationStatus:
        assert requirement.name == invocation.capability_name
        self.status_requests.append(request)
        traced = "traced" if request.traceparent == VALID_TRACEPARENT else "untraced"
        self.calls.append(f"status:{request.invocation_id}:{traced}")
        return self.statuses.pop(0)(request, invocation)


@pytest.fixture
def read_requirement() -> ResolvedCapabilityRequirement:
    return case_read_requirement()


@pytest.fixture
def write_requirement() -> ResolvedCapabilityRequirement:
    return comment_requirement()


@pytest.fixture
def native_grants() -> tuple[EffectiveCapabilityGrant, ...]:
    return load_capability_grant_set(
        canonical_bytes(grant_set_values())
    ).grants


@pytest.fixture
def session() -> KiteframeSessionContext:
    grant_set = load_capability_grant_set(canonical_bytes(grant_set_values()))
    return KiteframeSessionContext(
        actor=grant_set.actor,
        session=grant_set.session,
        task=grant_set.task,
        admission_id=grant_set.admission_id,
        grant_digest=grant_set.grant_digest,
        grants=grant_set.grants,
        authority_revisions=grant_set.authority_revisions,
        trace_context=KiteframeTraceContext(
            traceparent=VALID_TRACEPARENT,
            tracestate="vendor=value",
            baggage=(
                (
                    "kiteframe.session_id",
                    "11111111111111111111111111111111",
                ),
            ),
        ),
    )


class SessionContextSubclass(KiteframeSessionContext):
    pass


def subclassed_session(
    source: KiteframeSessionContext,
) -> SessionContextSubclass:
    return SessionContextSubclass(
        actor=source.actor,
        session=source.session,
        task=source.task,
        admission_id=source.admission_id,
        grant_digest=source.grant_digest,
        grants=source.grants,
        authority_revisions=source.authority_revisions,
        trace_context=source.trace_context,
        suspension=source.suspension,
    )


@pytest.fixture
def events() -> list[str]:
    return []


@pytest.fixture
def fake_invoker(events: list[str]) -> FakeInvoker:
    return FakeInvoker(events)


@pytest.fixture
def checkpoint_store(events: list[str]) -> FakeCheckpointStore:
    return FakeCheckpointStore(events=events, records=[])


@pytest.fixture
def suspension_bridge() -> FakeSuspensionBridge:
    return FakeSuspensionBridge(calls=[])


@pytest.fixture
def tools(
    read_requirement: ResolvedCapabilityRequirement,
    write_requirement: ResolvedCapabilityRequirement,
    native_grants: tuple[EffectiveCapabilityGrant, ...],
    session: KiteframeSessionContext,
    fake_invoker: FakeInvoker,
    checkpoint_store: FakeCheckpointStore,
    suspension_bridge: FakeSuspensionBridge,
) -> tuple[CapabilityTool, ...]:
    return build_capability_tools(
        (read_requirement, write_requirement),
        native_grants,
        grant_digest=session.grant_digest,
        invoker=fake_invoker,
        session=session,
        checkpoint_store=checkpoint_store,
        suspension_bridge=suspension_bridge,
    )


def test_capability_tool_builder_rejects_session_subclasses(
    read_requirement: ResolvedCapabilityRequirement,
    write_requirement: ResolvedCapabilityRequirement,
    native_grants: tuple[EffectiveCapabilityGrant, ...],
    session: KiteframeSessionContext,
    fake_invoker: FakeInvoker,
    checkpoint_store: FakeCheckpointStore,
    suspension_bridge: FakeSuspensionBridge,
) -> None:
    with pytest.raises(TypeError, match="exact KiteframeSessionContext"):
        build_capability_tools(
            (read_requirement, write_requirement),
            native_grants,
            grant_digest=session.grant_digest,
            invoker=fake_invoker,
            session=subclassed_session(session),
            checkpoint_store=checkpoint_store,
            suspension_bridge=suspension_bridge,
        )


@pytest.fixture
def read_tool(tools: tuple[CapabilityTool, ...]) -> CapabilityTool:
    return next(tool for tool in tools if tool.name == "cases.read")


@pytest.fixture
def comment_tool(tools: tuple[CapabilityTool, ...]) -> CapabilityTool:
    return next(tool for tool in tools if tool.name == "cases.comment")


def test_tool_name_description_and_schema_come_from_lock(
    read_tool: CapabilityTool,
    read_requirement: ResolvedCapabilityRequirement,
) -> None:
    descriptor = read_requirement.descriptor
    assert read_tool.name == "cases.read"
    assert read_tool.description == descriptor.summary
    assert read_tool.args_schema == descriptor.input_schema
    assert read_tool.descriptor_digest == read_requirement.descriptor_digest


def test_build_tools_matches_exact_effective_grants_and_canonical_digest(
    tools: tuple[CapabilityTool, ...],
    session: KiteframeSessionContext,
) -> None:
    assert tuple(tool.name for tool in tools) == ("cases.read", "cases.comment")
    assert all(tool.grant_digest == session.grant_digest for tool in tools)
    assert {
        (tool.grant.name, tool.grant.version, tool.grant.resources)
        for tool in tools
    } == {
        (grant.name, grant.version, grant.resources)
        for grant in session.grants
    }
    assert all(
        tool.session.authority_revisions is session.authority_revisions
        for tool in tools
    )


@pytest.mark.asyncio
async def test_tool_invokes_provider_with_session_and_trace_context(
    read_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    result = await read_tool.ainvoke(
        {"case_id": "case-1", "_resource": RESOURCE}
    )

    request = fake_invoker.requests[-1]
    assert result == {"ok": True}
    assert request.admission_id == "adm-1"
    assert request.grant_digest == read_tool.grant_digest
    assert request.traceparent == VALID_TRACEPARENT
    assert request.tracestate == "vendor=value"
    assert request.baggage == {
        "kiteframe.session_id": "11111111111111111111111111111111"
    }
    assert request.arguments == {"case_id": "case-1"}
    assert not hasattr(request, "authority_revisions")


@pytest.mark.asyncio
async def test_resource_defaults_only_when_one_exact_granted_selector_exists(
    read_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    await read_tool.ainvoke({"case_id": "case-1"})

    assert fake_invoker.requests[-1].selected_resource == RESOURCE


@pytest.mark.asyncio
async def test_ungranted_resource_fails_closed_before_provider_invocation(
    read_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    result = await read_tool.ainvoke(
        {
            "case_id": "case-1",
            "_resource": "tenant:t2/case:case-1",
        }
    )

    assert result == "KF-AUTH-003: capability resource is not granted"
    assert fake_invoker.requests == []


@pytest.mark.asyncio
async def test_effectful_key_is_uuidv7_scoped_and_persisted_before_invoke(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
    checkpoint_store: FakeCheckpointStore,
    events: list[str],
) -> None:
    await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    request = fake_invoker.requests[-1]
    key = request.idempotency_key
    assert key is not None
    assert uuid.UUID(key).version == 7
    assert events == [f"persist:{key}", f"invoke:{key}"]
    assert checkpoint_store.records == [
        PersistedIdempotencyKey(
            actor="actor:alice",
            capability_name="cases.comment",
            capability_version="1.0.0",
            key=key,
            resource=RESOURCE,
            semantic_operation="cases.comment",
            session="session:1",
            task="task:triage",
        )
    ]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "invalid_arguments",
    [
        {"case_id": "case-1"},
        {"case_id": "case-1", "body": "hello", "extra": "forged"},
        {"case_id": 7, "body": "hello"},
    ],
)
async def test_locked_input_schema_rejects_invalid_arguments_before_effect(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
    checkpoint_store: FakeCheckpointStore,
    invalid_arguments: dict[str, object],
) -> None:
    result = await comment_tool.ainvoke(
        {**invalid_arguments, "_resource": RESOURCE}
    )

    assert result == "KF-CAP-002: invalid capability invocation"
    assert checkpoint_store.records == []
    assert fake_invoker.requests == []


@pytest.mark.asyncio
async def test_native_factory_uses_rfc8785_numbers_and_unicode(
    read_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    result = await read_tool.ainvoke(
        {
            "amount": 1.0,
            "label": "€ café",
            "_resource": RESOURCE,
        }
    )

    assert result == {"ok": True}
    canonical = fake_invoker.requests[-1].canonical_json()
    assert b'"amount":1,' in canonical
    assert '"label":"€ café"'.encode() in canonical
    assert b"\\u20ac" not in canonical.lower()


@pytest.mark.asyncio
async def test_read_only_capability_forbids_idempotency_key(
    read_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
    checkpoint_store: FakeCheckpointStore,
) -> None:
    await read_tool.ainvoke({"case_id": "case-1", "_resource": RESOURCE})

    assert fake_invoker.requests[-1].idempotency_key is None
    assert checkpoint_store.records == []


@pytest.mark.asyncio
async def test_unknown_outcome_queries_status_before_any_same_key_retry(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    fake_invoker.outcomes = [outcome_unknown]
    fake_invoker.statuses = [succeeded_status({"ok": True})]

    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    key = fake_invoker.requests[-1].idempotency_key
    invocation_id = fake_invoker.requests[-1].invocation_id
    assert result == {"ok": True}
    assert fake_invoker.calls == [
        f"invoke:{key}",
        f"status:{invocation_id}:traced",
    ]
    assert fake_invoker.status_requests[-1].traceparent == VALID_TRACEPARENT


@pytest.mark.asyncio
async def test_deferred_outcome_queries_status_and_returns_invocation_reference(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    fake_invoker.outcomes = [deferred]
    fake_invoker.statuses = [pending]

    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    assert result == {
        "invocation_id": fake_invoker.requests[-1].invocation_id,
        "status": "deferred",
    }
    assert fake_invoker.calls[-1].startswith("status:")


@pytest.mark.asyncio
async def test_deferred_status_suspension_goes_to_bridge(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
    suspension_bridge: FakeSuspensionBridge,
) -> None:
    fake_invoker.outcomes = [deferred]
    fake_invoker.statuses = [suspended_status]

    with pytest.raises(SuspensionInterruptForTest, match="checkpoint:opaque:1"):
        await comment_tool.ainvoke(
            {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
        )

    request, outcome = suspension_bridge.calls[-1]
    assert request is fake_invoker.requests[-1]
    assert outcome.suspension is not None


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("factory", "safe_error"),
    [
        (failed, "COMMENT_REJECTED: comment rejected"),
        (denied, "KF-AUTH-003: capability invocation denied"),
    ],
)
async def test_native_failures_map_only_to_stable_safe_tool_errors(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
    factory: OutcomeFactory,
    safe_error: str,
) -> None:
    fake_invoker.outcomes = [factory]

    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    assert result == safe_error


@pytest.mark.asyncio
async def test_provider_denial_diagnostic_message_is_never_tool_output(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    fake_invoker.outcomes = [
        denied_with_message("provider secret: internal policy row 42")
    ]

    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    assert result == "KF-AUTH-003: capability invocation denied"
    assert "secret" not in result


@pytest.mark.asyncio
async def test_status_first_diagnostic_message_is_never_tool_output(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    fake_invoker.outcomes = [outcome_unknown]
    fake_invoker.statuses = [
        outcome_unknown_status("provider secret: shard unavailable")
    ]

    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    assert result == "KF-CAP-003: capability outcome requires reconciliation"
    assert "secret" not in result


@pytest.mark.asyncio
async def test_invalid_provider_result_is_rejected_by_native_locked_validation(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    fake_invoker.outcomes = [succeeded({"unexpected": "shape"})]

    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    assert result == "KF-CAP-002: invalid capability provider result"


@pytest.mark.asyncio
async def test_provider_exception_text_never_becomes_tool_output(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    async def raising_invoke(
        request: InvocationRequest,
    ) -> InvocationOutcome:
        del request
        raise RuntimeError("provider secret: do not disclose")

    fake_invoker.invoke = raising_invoke  # type: ignore[method-assign]

    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    assert result == "KF-CAP-004: capability provider unavailable"
    assert "secret" not in result


@pytest.mark.asyncio
async def test_suspended_outcome_goes_to_bridge_with_native_values(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
    suspension_bridge: FakeSuspensionBridge,
) -> None:
    fake_invoker.outcomes = [suspended]

    with pytest.raises(SuspensionInterruptForTest, match="checkpoint:opaque:1"):
        await comment_tool.ainvoke(
            {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
        )

    request, outcome = suspension_bridge.calls[-1]
    assert request is fake_invoker.requests[-1]
    assert outcome.suspension is not None
    assert outcome.suspension.proposal_digest == effect_proposal_digest(request)


@pytest.mark.asyncio
async def test_suspension_bridge_return_fails_closed_without_protected_output(
    read_requirement: ResolvedCapabilityRequirement,
    write_requirement: ResolvedCapabilityRequirement,
    native_grants: tuple[EffectiveCapabilityGrant, ...],
    session: KiteframeSessionContext,
    fake_invoker: FakeInvoker,
    checkpoint_store: FakeCheckpointStore,
) -> None:
    fake_invoker.outcomes = [suspended]
    tool = next(
        tool
        for tool in build_capability_tools(
            (read_requirement, write_requirement),
            native_grants,
            grant_digest=session.grant_digest,
            invoker=fake_invoker,
            session=session,
            checkpoint_store=checkpoint_store,
            suspension_bridge=ReturningSuspensionBridge(),
        )
        if tool.name == "cases.comment"
    )

    result = await tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": RESOURCE}
    )

    assert result == "KF-CAP-005: suspension bridge did not interrupt"
    assert "evidence-request" not in result


def test_tool_authority_and_schema_are_deeply_immutable(
    comment_tool: CapabilityTool,
) -> None:
    with pytest.raises((TypeError, ValidationError)):
        comment_tool.grant_digest = "ff" * 32  # pyright: ignore[reportAttributeAccessIssue]
    with pytest.raises((TypeError, ValidationError)):
        comment_tool.requirement = object()  # type: ignore[assignment]

    schema = comment_tool.args_schema
    assert isinstance(schema, dict)
    with pytest.raises(TypeError):
        schema["type"] = "array"
    with pytest.raises(TypeError):
        schema |= {"type": "array"}
    properties = schema["properties"]
    assert isinstance(properties, dict)
    with pytest.raises(TypeError):
        properties["case_id"]["type"] = "integer"


def test_sync_invocation_is_rejected(
    read_tool: CapabilityTool,
) -> None:
    with pytest.raises(
        RuntimeError,
        match="Kiteframe capability tools require async invocation",
    ):
        read_tool.invoke({"case_id": "case-1", "_resource": RESOURCE})


def test_build_rejects_grant_digest_not_bound_to_session(
    read_requirement: ResolvedCapabilityRequirement,
    native_grants: tuple[EffectiveCapabilityGrant, ...],
    session: KiteframeSessionContext,
    fake_invoker: FakeInvoker,
    checkpoint_store: FakeCheckpointStore,
    suspension_bridge: FakeSuspensionBridge,
) -> None:
    with pytest.raises(ValueError, match="grant digest"):
        build_capability_tools(
            (read_requirement,),
            native_grants,
            grant_digest="ff" * 32,
            invoker=fake_invoker,
            session=session,
            checkpoint_store=checkpoint_store,
            suspension_bridge=suspension_bridge,
        )


def test_uuid_factory_produces_uuidv7() -> None:
    generated = tools_module._uuid7()

    assert generated.version == 7
    assert generated.variant == uuid.RFC_4122
