from __future__ import annotations

import ast
import copy
import hashlib
import json
from collections.abc import Iterable
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
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph import END, START, StateGraph

from kiteframe_deepagents.context import (
    KiteframeSessionContext,
    KiteframeTraceContext,
)
from kiteframe_deepagents.suspension import (
    LangGraphSuspensionBridge,
    resume_command,
)
from kiteframe_deepagents.tools import (
    IdempotencyScope,
    PersistedIdempotencyKey,
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


def grant_and_session() -> tuple[EffectiveCapabilityGrant, KiteframeSessionContext]:
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
                "maximumEffect": "reversible_write",
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


def proposal_digest(request: InvocationRequest) -> str:
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
            "effect": "reversible_write",
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


def suspended(request: InvocationRequest) -> InvocationOutcome:
    return load_invocation_outcome(
        canonical_bytes(
            {
                "invocation_id": request.invocation_id,
                "status": "suspended",
                "suspension": {
                    "checkpointRef": "checkpoint-ref-1",
                    "evidenceKind": "approval",
                    "evidenceRequestRef": EVIDENCE_REF,
                    "proposalDigest": proposal_digest(request),
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
    def __init__(self) -> None:
        self.calls: list[str] = []
        self.requests: list[InvocationRequest] = []

    async def invoke(self, request: InvocationRequest) -> InvocationOutcome:
        self.requests.append(request)
        if request.evidence_refs == {}:
            return suspended(request)
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
    RestartableIdempotencyCheckpointStore,
):
    kiteframe_durable = True

    def __init__(self) -> None:
        super().__init__()
        self.correlations: dict[IdempotencyScope, PersistedIdempotencyKey] = {}

    async def persist_idempotency_key(
        self,
        record: PersistedIdempotencyKey,
    ) -> None:
        existing = self.correlations.setdefault(record.scope, record)
        if existing != record:
            raise AssertionError("idempotency correlation changed")

    async def load_idempotency_key(
        self,
        scope: IdempotencyScope,
    ) -> PersistedIdempotencyKey | None:
        return self.correlations.get(scope)

    async def latest(self, config: RunnableConfig) -> object:
        checkpoint = await self.aget_tuple(config)
        assert checkpoint is not None
        return {
            "checkpoint": checkpoint.checkpoint,
            "pending_writes": checkpoint.pending_writes,
        }


class GraphState(TypedDict, total=False):
    arguments: NotRequired[dict[str, object]]
    result: NotRequired[object]


def compile_graph(
    *,
    checkpointer: FakeDurableCheckpointer,
    invoker: FakeInvoker,
) -> Any:
    requirement = comment_requirement()
    grant, session = grant_and_session()
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
    return builder.compile(checkpointer=checkpointer)


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
        resume_command(EVIDENCE_REF),
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


def test_raw_evidence_is_rejected_before_resume_command_construction() -> None:
    with pytest.raises(ValueError, match="opaque reference"):
        resume_command(RAW_EVIDENCE)


def test_adapter_source_is_closed_and_uses_only_public_deepagents_apis() -> None:
    source_root = (
        WORKSPACE
        / "python/kiteframe-deepagents/src/kiteframe_deepagents"
    )
    forbidden_entrypoint_parameters = {
        "binding",
        "descriptor",
        "lock",
        "package",
        "target",
    }
    for path in source_root.glob("*.py"):
        tree = ast.parse(path.read_text(), filename=str(path))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                assert all(
                    not alias.name.startswith("deepagents._")
                    for alias in node.names
                )
            if isinstance(node, ast.ImportFrom) and node.module is not None:
                assert not node.module.startswith("deepagents._")
            if (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and node.func.attr == "loads"
                and node.args
            ):
                argument = node.args[0]
                assert not (
                    isinstance(argument, ast.Call)
                    and isinstance(argument.func, ast.Attribute)
                    and argument.func.attr == "canonical_json"
                )
            if (
                isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                and node.name in {"compile", "validate"}
            ):
                parameter_names = {
                    argument.arg
                    for argument in (*node.args.args, *node.args.kwonlyargs)
                }
                assert parameter_names.isdisjoint(
                    forbidden_entrypoint_parameters
                )


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
