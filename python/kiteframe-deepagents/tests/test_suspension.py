from __future__ import annotations

import ast
import copy
import hashlib
import json
import pickle
import re
from collections.abc import Iterable, Sequence
from pathlib import Path
from typing import Any, NotRequired, TypedDict

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
    load_resolved_agent,
)
from langchain_core.runnables import RunnableConfig
from langgraph.checkpoint.base import (
    ChannelVersions,
    Checkpoint,
    CheckpointMetadata,
)
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph import END, START, StateGraph
from langgraph.types import Command

from kiteframe_deepagents.context import (
    KiteframeSessionContext,
    KiteframeTraceContext,
)
from kiteframe_deepagents.suspension import (
    EvidenceReferenceResolver,
    LangGraphSuspensionBridge,
    ProtectedEvidenceReference,
    protect_resume_checkpointer,
    resolve_protected_evidence_reference,
    resume_command,
)
from kiteframe_deepagents.tools import (
    DurableInvocationCheckpointStore,
    IdempotencyScope,
    PersistedIdempotencyKey,
    PersistedInvocationCorrelation,
    RestartableIdempotencyCheckpointStore,
    build_capability_tools,
)

WORKSPACE = Path(__file__).resolve().parents[3]
VALID_TRACEPARENT = (
    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
)
RESOURCE = "tenant:t1/case:case-1"
RAW_EVIDENCE = "I approve this comment"
EVIDENCE_REF = "evidence-ref-1"
EVIDENCE_HANDLE = "approval-handle-1"


class FakeEvidenceReferenceResolver(EvidenceReferenceResolver):
    def __init__(self, references: dict[str, str]) -> None:
        self.references = references

    async def resolve_evidence_reference(self, handle: str) -> str:
        try:
            return self.references[handle]
        except KeyError:
            raise ValueError("evidence handle is unresolved") from None


async def trusted_resume_command() -> Any:
    return resume_command(await trusted_reference())


async def trusted_reference() -> ProtectedEvidenceReference:
    return await resolve_protected_evidence_reference(
        EVIDENCE_HANDLE,
        FakeEvidenceReferenceResolver({EVIDENCE_HANDLE: EVIDENCE_REF}),
    )


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


def comment_requirement(
    *,
    read_only_suspendable: bool = False,
) -> ResolvedCapabilityRequirement:
    resolved = json.loads(
        (WORKSPACE / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )
    locked = resolved["capabilityRequirements"][0]["lockedCapability"]
    descriptor = {
        "approval": {"kind": "none"},
        "confirmation": {"kind": "none"},
        "consent": {"kind": "none"},
        "effect": (
            "read_only" if read_only_suspendable else "reversible_write"
        ),
        "executionModes": ["immediate", "deferred", "suspendable"],
        "freshness": {
            "maxAdmissionAgeSeconds": None,
            "maxInputAgeSeconds": None,
            "policyRevisionRequired": False,
        },
        "idempotency": (
            {"kind": "none"}
            if read_only_suspendable
            else {
                "kind": "required",
                "retention_seconds": 86400,
                "scope": "actor_capability_resource_operation",
            }
        ),
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
        "stableErrors": [],
        "summary": "Add a comment to a case",
    }
    descriptor["descriptorDigest"] = hashlib.sha256(
        canonical_bytes(descriptor)
    ).hexdigest()
    locked["descriptor"] = descriptor
    locked["descriptorDigest"] = descriptor["descriptorDigest"]
    locked["identity"] = copy.deepcopy(descriptor["identity"])
    locked["inputSchemaDigest"] = hashlib.sha256(
        canonical_bytes(descriptor["inputSchema"])
    ).hexdigest()
    resolved["capabilityRequirements"][0]["resources"] = [RESOURCE]
    resolved["resolvedDigest"] = _resolved_digest(resolved)
    return load_resolved_agent(
        canonical_bytes(resolved)
    ).capability_requirements[0]


def grant_and_session(
    *,
    read_only_suspendable: bool = False,
) -> tuple[EffectiveCapabilityGrant, KiteframeSessionContext]:
    authority_entries = [{"revision": "7", "source": "policy"}]
    authority_revisions = {
        "authorityRevisionDigest": canonical_digest(
            b"kiteframe:authority-revision-set:v1\0",
            authority_entries,
        ),
        "entries": authority_entries,
    }
    values: dict[str, object] = {
        "actor": "actor:alice",
        "admissionId": "admission:comment-1",
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
                "maximumEffect": (
                    "read_only"
                    if read_only_suspendable
                    else "reversible_write"
                ),
                "preconditions": [],
                "requiredEvidence": {
                    "approval": {"kind": "none"},
                    "confirmation": {"kind": "none"},
                    "consent": {"kind": "none"},
                },
                "resources": [RESOURCE],
            }
        ],
        "issuedAt": 100,
        "optionalDenials": [],
        "policyRevision": "policy:7",
        "session": "session:1",
        "task": "task:triage",
    }
    values["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        values,
    )
    grant_set = load_capability_grant_set(canonical_bytes(values))
    session = KiteframeSessionContext(
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
            baggage=(("kiteframe.session_id", "11" * 16),),
        ),
    )
    return grant_set.grants[0], session


def proposal_digest(
    request: InvocationRequest,
    *,
    effect: str = "reversible_write",
) -> str:
    return canonical_digest(
        b"kiteframe:effect-proposal:v1\0",
        {
            "admissionId": request.admission_id,
            "argumentsDigest": canonical_digest(
                b"kiteframe:effect-arguments:v1\0",
                request.arguments,
            ),
            "capability": {
                "name": request.capability_name,
                "version": request.capability_version,
            },
            "effect": effect,
            "grantDigest": request.grant_digest,
            "idempotencyKey": request.idempotency_key,
            "invocationId": request.invocation_id,
            "preconditionsDigest": canonical_digest(
                b"kiteframe:effect-preconditions:v1\0",
                request.preconditions,
            ),
            "selectedResource": request.selected_resource,
        },
    )


def suspended(
    request: InvocationRequest,
    *,
    effect: str = "reversible_write",
) -> InvocationOutcome:
    return load_invocation_outcome(
        canonical_bytes(
            {
                "invocation_id": request.invocation_id,
                "status": "suspended",
                "suspension": {
                    "checkpointRef": "checkpoint-ref-1",
                    "evidenceKind": "approval",
                    "evidenceRequestRef": EVIDENCE_REF,
                    "proposalDigest": proposal_digest(
                        request,
                        effect=effect,
                    ),
                },
            }
        )
    )


def succeeded(request: InvocationRequest) -> InvocationOutcome:
    return load_invocation_outcome(
        canonical_bytes(
            {
                "invocation_id": request.invocation_id,
                "result": {"ok": True},
                "status": "succeeded",
            }
        )
    )


class FakeInvoker:
    def __init__(self, *, effect: str = "reversible_write") -> None:
        self.calls: list[str] = []
        self.effect = effect
        self.requests: list[InvocationRequest] = []

    async def invoke(self, request: InvocationRequest) -> InvocationOutcome:
        self.requests.append(request)
        if request.evidence_refs == {}:
            return suspended(request, effect=self.effect)
        self.calls.extend(
            [
                "validate_evidence",
                "check_grant_expiry",
                "check_authority_revisions",
                "check_preconditions",
                "invoke_point_of_use",
            ]
        )
        return succeeded(request)

    async def status(
        self,
        request: StatusRequest,
        invocation: InvocationRequest,
        requirement: ResolvedCapabilityRequirement,
    ) -> InvocationStatus:
        del request, invocation, requirement
        raise AssertionError("resume unexpectedly used status reconciliation")


class FakeDurableCheckpointer(
    InMemorySaver,
    DurableInvocationCheckpointStore,
    RestartableIdempotencyCheckpointStore,
):
    kiteframe_durable = True

    def __init__(self, durable_path: Path | None = None) -> None:
        super().__init__()
        self.durable_path = durable_path
        self.correlations: dict[IdempotencyScope, PersistedIdempotencyKey] = {}
        self.invocations: dict[
            IdempotencyScope,
            PersistedInvocationCorrelation,
        ] = {}
        if durable_path is not None and durable_path.exists():
            state = pickle.loads(durable_path.read_bytes())
            for thread_id, namespaces in state["storage"].items():
                for checkpoint_ns, checkpoints in namespaces.items():
                    self.storage[thread_id][checkpoint_ns].update(checkpoints)
            self.writes.update(state["writes"])
            self.blobs.update(state["blobs"])
            self.correlations.update(state["correlations"])
            self.invocations.update(state["invocations"])

    def _flush(self) -> None:
        if self.durable_path is None:
            return
        storage = {
            thread_id: {
                checkpoint_ns: dict(checkpoints)
                for checkpoint_ns, checkpoints in namespaces.items()
            }
            for thread_id, namespaces in self.storage.items()
        }
        state = {
            "blobs": dict(self.blobs),
            "correlations": dict(self.correlations),
            "invocations": dict(self.invocations),
            "storage": storage,
            "writes": dict(self.writes),
        }
        self.durable_path.write_bytes(pickle.dumps(state))

    async def aput(
        self,
        config: RunnableConfig,
        checkpoint: Checkpoint,
        metadata: CheckpointMetadata,
        new_versions: ChannelVersions,
    ) -> RunnableConfig:
        stored = await super().aput(
            config,
            checkpoint,
            metadata,
            new_versions,
        )
        self._flush()
        return stored

    async def aput_writes(
        self,
        config: RunnableConfig,
        writes: Sequence[tuple[str, Any]],
        task_id: str,
        task_path: str = "",
    ) -> None:
        await super().aput_writes(config, writes, task_id, task_path)
        self._flush()

    async def persist_idempotency_key(
        self,
        record: PersistedIdempotencyKey,
    ) -> None:
        existing = self.correlations.setdefault(record.scope, record)
        if existing != record:
            raise AssertionError("idempotency correlation changed")
        self._flush()

    async def load_idempotency_key(
        self,
        scope: IdempotencyScope,
    ) -> PersistedIdempotencyKey | None:
        return self.correlations.get(scope)

    async def persist_invocation_correlation(
        self,
        record: PersistedInvocationCorrelation,
    ) -> None:
        existing = self.invocations.setdefault(record.scope, record)
        if existing != record:
            raise AssertionError("invocation correlation changed")
        self._flush()

    async def load_invocation_correlation(
        self,
        scope: IdempotencyScope,
    ) -> PersistedInvocationCorrelation | None:
        return self.invocations.get(scope)

    async def latest(self, config: RunnableConfig) -> object:
        checkpoint = await self.aget_tuple(config)
        assert checkpoint is not None
        return {
            "checkpoint": checkpoint.checkpoint,
            "pending_writes": checkpoint.pending_writes,
        }


def checkpointer_snapshot(checkpointer: InMemorySaver) -> bytes:
    storage = {
        thread_id: {
            checkpoint_ns: dict(checkpoints)
            for checkpoint_ns, checkpoints in namespaces.items()
        }
        for thread_id, namespaces in checkpointer.storage.items()
    }
    return pickle.dumps(
        (
            storage,
            dict(checkpointer.writes),
            dict(checkpointer.blobs),
        )
    )


class GraphState(TypedDict, total=False):
    arguments: NotRequired[dict[str, object]]
    result: NotRequired[object]


def compile_graph(
    *,
    checkpointer: FakeDurableCheckpointer,
    invoker: FakeInvoker,
    read_only_suspendable: bool = False,
) -> Any:
    requirement = comment_requirement(
        read_only_suspendable=read_only_suspendable
    )
    grant, session = grant_and_session(
        read_only_suspendable=read_only_suspendable
    )
    tool = build_capability_tools(
        (requirement,),
        (grant,),
        grant_digest=session.grant_digest,
        invoker=invoker,
        session=session,
        checkpoint_store=checkpointer,
        suspension_bridge=LangGraphSuspensionBridge(),
    )[0]

    async def invoke_tool(state: GraphState) -> GraphState:
        arguments = state.get("arguments")
        if arguments is None:
            raise TypeError("graph arguments are unresolved")
        result = await tool.ainvoke(arguments)
        return {"result": result}

    builder = StateGraph(GraphState)
    builder.add_node("invoke_tool", invoke_tool)
    builder.add_edge(START, "invoke_tool")
    builder.add_edge("invoke_tool", END)
    return builder.compile(
        checkpointer=protect_resume_checkpointer(checkpointer)
    )


@pytest.mark.asyncio
async def test_suspension_checkpoint_contains_references_not_evidence_text() -> None:
    checkpointer = FakeDurableCheckpointer()
    graph = compile_graph(checkpointer=checkpointer, invoker=FakeInvoker())
    config: RunnableConfig = {
        "configurable": {"thread_id": "task-7-suspension"}
    }

    result = await graph.ainvoke(
        {
            "arguments": {
                "body": "hello",
                "case_id": "case-1",
                "_resource": RESOURCE,
            }
        },
        config,
    )

    envelope = result["__interrupt__"][0].value
    checkpoint = await checkpointer.latest(config)
    serialized = json.dumps(checkpoint, default=str)
    assert envelope["type"] == "kiteframe.capability.suspension"
    assert envelope["checkpoint_ref"] == "checkpoint-ref-1"
    assert envelope["evidence_request_ref"] == EVIDENCE_REF
    assert envelope["proposal_digest"] in serialized
    assert "checkpoint-ref-1" in serialized
    assert EVIDENCE_REF in serialized
    assert RAW_EVIDENCE not in serialized


@pytest.mark.asyncio
async def test_process_restart_resume_reauthorizes_before_effect() -> None:
    checkpointer = FakeDurableCheckpointer()
    first_invoker = FakeInvoker()
    graph = compile_graph(checkpointer=checkpointer, invoker=first_invoker)
    config: RunnableConfig = {
        "configurable": {"thread_id": "task-7-restart"}
    }
    await graph.ainvoke(
        {
            "arguments": {
                "body": "hello",
                "case_id": "case-1",
                "_resource": RESOURCE,
            }
        },
        config,
    )
    original = first_invoker.requests[-1]

    restarted_invoker = FakeInvoker()
    restarted_graph = compile_graph(
        checkpointer=checkpointer,
        invoker=restarted_invoker,
    )
    result = await restarted_graph.ainvoke(
        await trusted_resume_command(),
        config,
    )

    resumed = restarted_invoker.requests[-1]
    assert result["result"] == {"ok": True}
    assert restarted_invoker.calls == [
        "validate_evidence",
        "check_grant_expiry",
        "check_authority_revisions",
        "check_preconditions",
        "invoke_point_of_use",
    ]
    assert resumed.invocation_id == original.invocation_id
    assert resumed.idempotency_key == original.idempotency_key
    assert resumed.admission_id == original.admission_id
    assert resumed.grant_digest == original.grant_digest
    assert resumed.traceparent == original.traceparent == VALID_TRACEPARENT
    assert resumed.evidence_refs == {"approval": EVIDENCE_REF}
    assert not hasattr(resumed, "authority_revisions")


@pytest.mark.asyncio
async def test_distinct_graph_tasks_do_not_share_invocation_correlation() -> None:
    checkpointer = FakeDurableCheckpointer()
    first_invoker = FakeInvoker()
    first_graph = compile_graph(
        checkpointer=checkpointer,
        invoker=first_invoker,
    )
    await first_graph.ainvoke(
        {
            "arguments": {
                "body": "first",
                "case_id": "case-1",
                "_resource": RESOURCE,
            }
        },
        {"configurable": {"thread_id": "task-7-first"}},
    )

    second_invoker = FakeInvoker()
    second_graph = compile_graph(
        checkpointer=checkpointer,
        invoker=second_invoker,
    )
    await second_graph.ainvoke(
        {
            "arguments": {
                "body": "second",
                "case_id": "case-1",
                "_resource": RESOURCE,
            }
        },
        {"configurable": {"thread_id": "task-7-second"}},
    )

    assert (
        first_invoker.requests[-1].invocation_id
        != second_invoker.requests[-1].invocation_id
    )
    assert (
        first_invoker.requests[-1].idempotency_key
        != second_invoker.requests[-1].idempotency_key
    )


@pytest.mark.asyncio
async def test_read_only_restart_uses_new_checkpointer_and_same_invocation(
    tmp_path: Path,
) -> None:
    durable_path = tmp_path / "durable-checkpoint.bin"
    first_checkpointer = FakeDurableCheckpointer(durable_path)
    first_invoker = FakeInvoker(effect="read_only")
    first_graph = compile_graph(
        checkpointer=first_checkpointer,
        invoker=first_invoker,
        read_only_suspendable=True,
    )
    config: RunnableConfig = {
        "configurable": {"thread_id": "task-7-read-only-restart"}
    }
    await first_graph.ainvoke(
        {
            "arguments": {
                "body": "read",
                "case_id": "case-1",
                "_resource": RESOURCE,
            }
        },
        config,
    )
    original = first_invoker.requests[-1]
    assert original.idempotency_key is None

    restarted_checkpointer = FakeDurableCheckpointer(durable_path)
    assert restarted_checkpointer is not first_checkpointer
    assert restarted_checkpointer.storage is not first_checkpointer.storage
    restarted_invoker = FakeInvoker(effect="read_only")
    restarted_graph = compile_graph(
        checkpointer=restarted_checkpointer,
        invoker=restarted_invoker,
        read_only_suspendable=True,
    )
    result = await restarted_graph.ainvoke(
        await trusted_resume_command(),
        config,
    )

    resumed = restarted_invoker.requests[-1]
    assert result["result"] == {"ok": True}
    assert resumed.invocation_id == original.invocation_id
    assert resumed.idempotency_key is None
    assert resumed.evidence_refs == {"approval": EVIDENCE_REF}


class PersistOnlyCheckpointStore:
    async def persist_idempotency_key(
        self,
        record: PersistedIdempotencyKey,
    ) -> None:
        del record


def test_suspendable_tool_requires_durable_invocation_load_and_persist() -> None:
    requirement = comment_requirement()
    grant, session = grant_and_session()

    with pytest.raises(TypeError, match="durable invocation correlation"):
        build_capability_tools(
            (requirement,),
            (grant,),
            grant_digest=session.grant_digest,
            invoker=FakeInvoker(),
            session=session,
            checkpoint_store=PersistOnlyCheckpointStore(),
            suspension_bridge=LangGraphSuspensionBridge(),
        )


@pytest.mark.parametrize(
    "untrusted_value",
    [
        "approved",
        "hunter2",
        "c2VjcmV0",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhbGljZSJ9.signature",
        RAW_EVIDENCE,
    ],
)
def test_untrusted_evidence_cannot_construct_resume_command(
    untrusted_value: str,
) -> None:
    with pytest.raises((TypeError, ValueError)):
        resume_command(untrusted_value)  # type: ignore[arg-type]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "untrusted_reference",
    [
        "approved",
        "hunter2",
        "c2VjcmV0",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhbGljZSJ9.signature",
        RAW_EVIDENCE,
    ],
)
async def test_resolver_rejects_non_reference_values(
    untrusted_reference: str,
) -> None:
    resolver = FakeEvidenceReferenceResolver(
        {EVIDENCE_HANDLE: untrusted_reference}
    )

    with pytest.raises(ValueError, match="protected reference"):
        await resolve_protected_evidence_reference(
            EVIDENCE_HANDLE,
            resolver,
        )


@pytest.mark.asyncio
async def test_resolver_issues_branded_reference_before_command() -> None:
    with pytest.raises(TypeError, match="must come from a resolver"):
        ProtectedEvidenceReference(EVIDENCE_REF)

    reference = await resolve_protected_evidence_reference(
        EVIDENCE_HANDLE,
        FakeEvidenceReferenceResolver({EVIDENCE_HANDLE: EVIDENCE_REF}),
    )
    command = resume_command(reference)

    assert command.resume is reference
    with pytest.raises(TypeError):
        json.dumps(command.resume)
    checkpointer = protect_resume_checkpointer(
        FakeDurableCheckpointer()
    ).with_allowlist(set())
    restored = checkpointer.serde.loads_typed(
        checkpointer.serde.dumps_typed(reference)
    )
    assert type(restored) is ProtectedEvidenceReference
    assert resume_command(restored).resume is restored


@pytest.mark.asyncio
async def test_forged_serialized_reference_cannot_mint_brand() -> None:
    unprotected = FakeDurableCheckpointer().serde
    checkpointer = protect_resume_checkpointer(
        FakeDurableCheckpointer()
    ).with_allowlist(set())
    forged = unprotected.dumps_typed(
        {
            "__kiteframe_resolver_issued_evidence_reference_v1__": (
                "evidence-ref-forged"
            )
        }
    )

    with pytest.raises(
        TypeError,
        match="resolver-issued protected evidence reference",
    ):
        checkpointer.serde.loads_typed(forged)

    reference = await trusted_reference()
    encoded = checkpointer.serde.dumps_typed(reference)
    wire_value = unprotected.loads_typed(encoded)
    assert type(wire_value) is bytes
    tampered = wire_value[:-1] + (
        b"x" if wire_value[-1:] != b"x" else b"y"
    )
    with pytest.raises(
        TypeError,
        match="resolver-issued protected evidence reference",
    ):
        checkpointer.serde.loads_typed(
            unprotected.dumps_typed(tampered)
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "shape",
    ["list", "tuple", "nested-list", "nested-dict", "dict-key"],
)
async def test_non_framework_resume_shapes_leave_saver_unchanged(
    shape: str,
) -> None:
    checkpointer = FakeDurableCheckpointer()
    invoker = FakeInvoker()
    graph = compile_graph(checkpointer=checkpointer, invoker=invoker)
    config: RunnableConfig = {
        "configurable": {"thread_id": f"forged-shape-{shape}"}
    }
    await graph.ainvoke(
        {
            "arguments": {
                "body": "hello",
                "case_id": "case-1",
                "_resource": RESOURCE,
            }
        },
        config,
    )
    before = checkpointer_snapshot(checkpointer)
    reference = await trusted_reference()
    forged_resume: object
    if shape == "list":
        forged_resume = [reference]
    elif shape == "tuple":
        forged_resume = (reference,)
    elif shape == "nested-list":
        forged_resume = [[reference]]
    elif shape == "nested-dict":
        forged_resume = {"attacker-password": [reference]}
    else:
        forged_resume = {"attacker-password": reference}

    with pytest.raises(
        TypeError,
        match="resolver-issued protected evidence reference",
    ):
        await graph.ainvoke(Command(resume=forged_resume), config)

    after = checkpointer_snapshot(checkpointer)
    assert after == before
    assert all(request.evidence_refs == {} for request in invoker.requests)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("forged_resume", "smuggled_value"),
    [
        pytest.param(
            {
                "reference": "evidence-ref-raw",
                "type": "kiteframe.protected-evidence-reference",
            },
            "evidence-ref-raw",
            id="forged-dict",
        ),
        pytest.param(
            "evidence-ref-raw",
            "evidence-ref-raw",
            id="direct-reference",
        ),
        pytest.param(
            "evidence-ref-hunter2",
            "evidence-ref-hunter2",
            id="password",
        ),
        pytest.param(
            "evidence-ref-c2VjcmV0",
            "evidence-ref-c2VjcmV0",
            id="base64",
        ),
        pytest.param(
            (
                "evidence-ref-eyJhbGciOiJIUzI1NiJ9."
                "eyJzdWIiOiJhbGljZSJ9.signature"
            ),
            (
                "evidence-ref-eyJhbGciOiJIUzI1NiJ9."
                "eyJzdWIiOiJhbGljZSJ9.signature"
            ),
            id="jwt",
        ),
    ],
)
async def test_forged_resume_never_reaches_checkpoint_or_provider(
    forged_resume: object,
    smuggled_value: str,
) -> None:
    checkpointer = FakeDurableCheckpointer()
    invoker = FakeInvoker()
    graph = compile_graph(checkpointer=checkpointer, invoker=invoker)
    config: RunnableConfig = {
        "configurable": {"thread_id": f"forged-{smuggled_value}"}
    }
    await graph.ainvoke(
        {
            "arguments": {
                "body": "hello",
                "case_id": "case-1",
                "_resource": RESOURCE,
            }
        },
        config,
    )

    with pytest.raises(
        TypeError,
        match="resolver-issued protected evidence reference",
    ):
        await graph.ainvoke(Command(resume=forged_resume), config)

    checkpoint = await checkpointer.latest(config)
    serialized = json.dumps(checkpoint, default=str)
    assert smuggled_value not in serialized
    assert all(request.evidence_refs == {} for request in invoker.requests)


def test_adapter_source_is_closed_and_uses_only_public_deepagents_apis() -> None:
    source_root = (
        WORKSPACE
        / "python/kiteframe-deepagents/src/kiteframe_deepagents"
    )
    assert _adapter_tree_violations(source_root) == []


def _adapter_source_violations(source: str, filename: str) -> list[str]:
    tree = ast.parse(source, filename=filename)
    violations: list[str] = []
    forbidden_input_words = {
        "binding",
        "descriptor",
        "lock",
        "lockfile",
        "manifest",
        "package",
        "pkg",
        "schema",
        "target",
    }
    json_modules = {"json"}
    json_decoders: set[tuple[str, ...]] = set()
    tainted_values: set[tuple[str, ...]] = set()
    semantic_aliases: dict[tuple[str, ...], set[str]] = {}

    def attribute_path(expression: ast.AST) -> tuple[str, ...] | None:
        if isinstance(expression, ast.Name):
            return (expression.id,)
        if isinstance(expression, ast.Attribute):
            prefix = attribute_path(expression.value)
            if prefix is not None:
                return (*prefix, expression.attr)
        if (
            isinstance(expression, ast.Subscript)
            and (prefix := attribute_path(expression.value)) is not None
            and isinstance(expression.slice, ast.Constant)
            and type(expression.slice.value) in {str, int}
        ):
            return (*prefix, f"[{expression.slice.value!r}]")
        return None

    def semantic_words(identifier: str) -> set[str]:
        normalized = re.sub(
            r"(?<=[a-z0-9])(?=[A-Z])",
            "_",
            identifier,
        )
        return set(re.findall(r"[a-z0-9]+", normalized.lower()))

    def assignment_pairs(
        target: ast.AST,
        value: ast.AST,
    ) -> list[tuple[tuple[str, ...], ast.AST]]:
        if isinstance(target, ast.Starred):
            return assignment_pairs(target.value, value)
        if (
            isinstance(target, (ast.Tuple, ast.List))
            and isinstance(value, (ast.Tuple, ast.List))
            and len(target.elts) == len(value.elts)
        ):
            return [
                pair
                for child_target, child_value in zip(
                    target.elts,
                    value.elts,
                    strict=True,
                )
                for pair in assignment_pairs(child_target, child_value)
            ]
        if isinstance(target, (ast.Tuple, ast.List)):
            return [
                pair
                for child_target in target.elts
                for pair in assignment_pairs(child_target, value)
            ]
        path = attribute_path(target)
        return [(path, value)] if path is not None else []

    def contains_canonical_json(expression: ast.AST) -> bool:
        return any(
            isinstance(child, ast.Call)
            and isinstance(child.func, ast.Attribute)
            and child.func.attr == "canonical_json"
            for child in ast.walk(expression)
        )

    def referenced_paths(
        expression: ast.AST,
    ) -> set[tuple[str, ...]]:
        return {
            path
            for child in ast.walk(expression)
            if (path := attribute_path(child)) is not None
        }

    def annotation_paths(
        annotation: ast.AST,
    ) -> set[tuple[str, ...]]:
        paths = referenced_paths(annotation)
        if (
            isinstance(annotation, ast.Constant)
            and type(annotation.value) is str
        ):
            try:
                forward_reference = ast.parse(
                    annotation.value,
                    mode="eval",
                ).body
            except SyntaxError:
                return paths
            paths.update(referenced_paths(forward_reference))
        return paths

    def is_json_decoder(expression: ast.AST) -> bool:
        path = attribute_path(expression)
        return (
            path is not None
            and (
                (len(path) == 2 and path[0] in json_modules and path[1] == "loads")
                or path in json_decoders
            )
        )

    assignments: list[tuple[tuple[str, ...], ast.AST]] = []
    entrypoints: list[ast.FunctionDef | ast.AsyncFunctionDef] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name.startswith("deepagents._"):
                    violations.append("private Deep Agents import")
                if alias.name == "json":
                    json_modules.add(alias.asname or alias.name)
                local_name = alias.asname or alias.name.split(".", maxsplit=1)[0]
                semantic_aliases[(local_name,)] = semantic_words(alias.name)
        if isinstance(node, ast.ImportFrom) and node.module is not None:
            if node.module.startswith("deepagents._") or (
                node.module == "deepagents"
                and any(alias.name.startswith("_") for alias in node.names)
            ):
                violations.append("private Deep Agents import")
            if node.module == "json":
                json_decoders.update(
                    (alias.asname or alias.name,)
                    for alias in node.names
                    if alias.name == "loads"
                )
            for alias in node.names:
                semantic_aliases[(alias.asname or alias.name,)] = (
                    semantic_words(alias.name)
                )
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            if node.value is None:
                continue
            targets = (
                node.targets
                if isinstance(node, ast.Assign)
                else [node.target]
            )
            for target in targets:
                assignments.extend(assignment_pairs(target, node.value))
        if (
            isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name in {"compile", "validate"}
        ):
            entrypoints.append(node)

    changed = True
    while changed:
        changed = False
        for target, value in assignments:
            if (
                is_json_decoder(value)
                and target not in json_decoders
            ):
                json_decoders.add(target)
                changed = True
            if (
                contains_canonical_json(value)
                or not referenced_paths(value).isdisjoint(tainted_values)
            ) and target not in tainted_values:
                tainted_values.add(target)
                changed = True
            inherited_words = {
                word
                for path in referenced_paths(value)
                for word in semantic_aliases.get(path, set())
            }
            if inherited_words:
                current_words = semantic_aliases.setdefault(target, set())
                new_words = inherited_words - current_words
                current_words.update(new_words)
                changed = changed or bool(new_words)

    for entrypoint in entrypoints:
        arguments = (
            *entrypoint.args.args,
            *entrypoint.args.kwonlyargs,
        )
        for argument in arguments:
            words = semantic_words(argument.arg)
            if argument.annotation is not None:
                words.update(
                    semantic_words(ast.unparse(argument.annotation))
                )
                words.update(
                    word
                    for path in annotation_paths(argument.annotation)
                    for word in semantic_aliases.get(path, set())
                )
            if not words.isdisjoint(forbidden_input_words):
                violations.append("open compilation input")

    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Call)
            and is_json_decoder(node.func)
            and node.args
            and (
                contains_canonical_json(node.args[0])
                or not referenced_paths(node.args[0]).isdisjoint(
                    tainted_values
                )
            )
        ):
            violations.append("canonical JSON reconstruction")
    return violations


def _adapter_tree_violations(source_root: Path) -> list[str]:
    violations: list[str] = []
    for path in source_root.rglob("*.py"):
        violations.extend(
            _adapter_source_violations(path.read_text(), str(path))
        )
    return violations


@pytest.mark.parametrize(
    "source",
    [
        "from deepagents import _private as public\n",
        (
            "import json\n"
            "payload = native.canonical_json()\n"
            "shadow = payload\n"
            "decode = json.loads\n"
            "decode(shadow)\n"
        ),
        "def compile(runtime_inputs, *, binding_snapshot): pass\n",
        (
            "def validate(runtime_inputs, *, "
            "candidate: RuntimeTarget): pass\n"
        ),
        (
            "from runtime import RuntimeTarget as RT\n"
            "def validate(runtime_inputs, *, candidate: RT): pass\n"
        ),
        (
            "from runtime import RuntimeTarget as RT\n"
            "PublicType = RT\n"
            "def validate(runtime_inputs, *, candidate: PublicType): pass\n"
        ),
        (
            "import json\n"
            "payload, ignored = native.canonical_json(), None\n"
            "decode = json.loads\n"
            "decode(payload)\n"
        ),
        (
            "import json\n"
            "holder.payload = native.canonical_json()\n"
            "shadow = holder.payload\n"
            "decode = json.loads\n"
            "decode(shadow)\n"
        ),
        (
            "import json\n"
            "slots['payload'] = native.canonical_json()\n"
            "json.loads(slots['payload'])\n"
        ),
        (
            "from runtime import RuntimeTarget as RT\n"
            "def validate(runtime_inputs, *, candidate: 'RT'): pass\n"
        ),
    ],
)
def test_adapter_source_gate_rejects_semantic_bypasses(source: str) -> None:
    assert _adapter_source_violations(source, "<adversarial>") != []


def test_adapter_source_gate_scans_nested_modules(tmp_path: Path) -> None:
    nested = tmp_path / "nested"
    nested.mkdir()
    (nested / "bypass.py").write_text(
        "from deepagents import _private\n"
    )

    assert _adapter_tree_violations(tmp_path) != []


def test_locked_tool_schema_is_an_exact_read_only_byte_projection() -> None:
    requirement = comment_requirement()
    grant, session = grant_and_session()
    checkpointer = FakeDurableCheckpointer()
    tool = build_capability_tools(
        (requirement,),
        (grant,),
        grant_digest=session.grant_digest,
        invoker=FakeInvoker(),
        session=session,
        checkpoint_store=checkpointer,
        suspension_bridge=LangGraphSuspensionBridge(),
    )[0]

    assert canonical_bytes(tool.args_schema) == canonical_bytes(
        requirement.descriptor.input_schema
    )
    assert tool.descriptor_digest == requirement.descriptor_digest
    assert hashlib.sha256(canonical_bytes(tool.args_schema)).hexdigest() == (
        requirement.input_schema_digest
    )
    assert isinstance(tool.args_schema, dict)
    with pytest.raises(TypeError):
        tool.args_schema["type"] = "array"
