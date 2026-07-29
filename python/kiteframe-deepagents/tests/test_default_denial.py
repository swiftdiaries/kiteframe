from __future__ import annotations

import hashlib
import json
from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, NoReturn, cast

import pytest
from kiteframe import (
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
    KiteframeDiagnosticError,
    ResolvedSubagent,
    StatusRequest,
    load_capability_grant_set,
    load_invocation_outcome,
    load_resolved_agent,
)
from langchain.agents import create_agent
from langchain.agents.middleware import (
    ModelRequest,
    ModelResponse,
    ToolCallRequest,
)
from langchain_core.language_models.chat_models import BaseChatModel
from langchain_core.messages import (
    AIMessage,
    BaseMessage,
    HumanMessage,
    ToolMessage,
)
from langchain_core.outputs import ChatGeneration, ChatResult
from langchain_core.tools import BaseTool, StructuredTool
from pydantic import Field

from kiteframe_deepagents.context import (
    KiteframeSessionContext,
    KiteframeTraceContext,
)
from kiteframe_deepagents.middleware import (
    DeclaredChildTaskTool,
    KiteframeGuardMiddleware,
)
from kiteframe_deepagents.tools import (
    CapabilitySuspensionBridge,
    CapabilityTool,
    IdempotencyCheckpointStore,
    PersistedIdempotencyKey,
    build_capability_tools,
)

WORKSPACE = Path(__file__).resolve().parents[3]
ISSUED_AT = 100
EXPIRED_AT = 200
RESOURCE = "tenant:support"
VALID_TRACEPARENT = (
    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
)
AMBIENT_NAMES = (
    "ls",
    "read_file",
    "write_file",
    "edit_file",
    "glob",
    "grep",
    "execute",
    "http",
    "mcp",
    "task",
)
ModelHandler = Callable[[ModelRequest[Any]], Awaitable[ModelResponse[Any]]]


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def canonical_digest(domain: bytes, value: object) -> str:
    return hashlib.sha256(domain + canonical_bytes(value)).hexdigest()


def _hash_domain(domain: bytes, chunks: list[bytes]) -> bytes:
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
        _canonical_component(b"resolved/report", resolved["compilationReport"]),
    ]
    return _hash_domain(b"resolved-agent", components).hex()


def child_declarations() -> tuple[ResolvedSubagent, ...]:
    resolved = json.loads(
        (WORKSPACE / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )
    resolved["subagents"] = [
        {
            "delegation": {
                "agent": "agents/case-child/agent.yaml",
                "capabilities": ["cases.read"],
            },
            "packageIdentity": {
                "name": "case-child",
                "version": "0.1.0",
            },
            "resolvedDigest": "ab" * 32,
        }
    ]
    resolved["resolvedDigest"] = _resolved_digest(resolved)
    return load_resolved_agent(canonical_bytes(resolved)).subagents


def grant_set_values(*, revision: str = "7") -> dict[str, Any]:
    authority_entries = [{"revision": revision, "source": "policy"}]
    authority_revisions = {
        "authorityRevisionDigest": canonical_digest(
            b"kiteframe:authority-revision-set:v1\0",
            authority_entries,
        ),
        "entries": authority_entries,
    }
    values: dict[str, Any] = {
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
        "expiresAt": EXPIRED_AT + 1,
        "grants": [
            {
                "capability": {"name": "cases.read", "version": "1.2.0"},
                "executionModes": ["immediate"],
                "expiresAt": EXPIRED_AT,
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
            }
        ],
        "issuedAt": ISSUED_AT,
        "optionalDenials": [],
        "policyRevision": f"policy:{revision}",
        "session": "session:1",
        "task": "task:triage",
    }
    values["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        values,
    )
    return values


def session_context(*, revision: str = "7") -> KiteframeSessionContext:
    grant_set = load_capability_grant_set(
        canonical_bytes(grant_set_values(revision=revision))
    )
    return KiteframeSessionContext(
        actor=grant_set.actor,
        session=grant_set.session,
        task=grant_set.task,
        admission_id=grant_set.admission_id,
        grant_digest=grant_set.grant_digest,
        grants=grant_set.grants,
        authority_revisions=grant_set.authority_revisions,
        trace_context=KiteframeTraceContext(traceparent=VALID_TRACEPARENT),
    )


class FakeInvoker:
    async def invoke(self, request: InvocationRequest) -> InvocationOutcome:
        raise AssertionError(f"unexpected provider invocation: {request}")

    async def status(
        self,
        request: StatusRequest,
        invocation: InvocationRequest,
        requirement: object,
    ) -> InvocationStatus:
        raise AssertionError(
            f"unexpected provider status: {request}, {invocation}, {requirement}"
        )


@dataclass
class RecordingInvoker:
    requests: list[InvocationRequest]

    async def invoke(self, request: InvocationRequest) -> InvocationOutcome:
        self.requests.append(request)
        return load_invocation_outcome(
            canonical_bytes(
                {
                    "invocation_id": request.invocation_id,
                    "result": {"authority": "refreshed"},
                    "status": "succeeded",
                }
            )
        )

    async def status(
        self,
        request: StatusRequest,
        invocation: InvocationRequest,
        requirement: object,
    ) -> InvocationStatus:
        raise AssertionError(
            f"unexpected provider status: {request}, {invocation}, {requirement}"
        )


class FakeCheckpointStore(IdempotencyCheckpointStore):
    async def persist_idempotency_key(
        self,
        record: PersistedIdempotencyKey,
    ) -> None:
        raise AssertionError(f"unexpected checkpoint write: {record}")


class FakeSuspensionBridge(CapabilitySuspensionBridge):
    async def suspend(
        self,
        request: InvocationRequest,
        outcome: InvocationOutcome,
    ) -> NoReturn:
        raise AssertionError(
            f"unexpected suspension: {request}, {outcome}"
        )


@dataclass(frozen=True, slots=True)
class FixedClock:
    value: int

    def now(self) -> int:
        return self.value


class FailingClock:
    def now(self) -> int:
        raise RuntimeError("clock secret")


class FailingAuthorityProvider:
    async def current(
        self,
        session: KiteframeSessionContext,
        now: int,
    ) -> KiteframeSessionContext:
        del session, now
        raise RuntimeError("provider secret")


@dataclass(frozen=True, slots=True)
class FixedAuthorityProvider:
    session: KiteframeSessionContext

    async def current(
        self,
        session: KiteframeSessionContext,
        now: int,
    ) -> KiteframeSessionContext:
        del session, now
        return self.session


class FailingToolRegistry:
    async def admitted_tools(
        self,
        session: KiteframeSessionContext,
    ) -> tuple[CapabilityTool, ...]:
        del session
        raise RuntimeError("registry secret")


@dataclass(frozen=True, slots=True)
class FixedToolRegistry:
    tools: tuple[CapabilityTool, ...]

    async def admitted_tools(
        self,
        session: KiteframeSessionContext,
    ) -> tuple[CapabilityTool, ...]:
        del session
        return self.tools


class RefreshToolCallingModel(BaseChatModel):
    bound_tools: list[
        tuple[
            dict[str, Any] | type | Callable[..., Any] | BaseTool,
            ...,
        ]
    ] = Field(default_factory=list)
    model_calls: int = 0

    @property
    def _llm_type(self) -> str:
        return "refresh-tool-calling-test"

    def bind_tools(
        self,
        tools: Sequence[
            dict[str, Any] | type | Callable[..., Any] | BaseTool
        ],
        *,
        tool_choice: str | None = None,
        **kwargs: Any,
    ) -> RefreshToolCallingModel:
        del tool_choice, kwargs
        self.bound_tools.append(tuple(tools))
        return self

    def _generate(
        self,
        messages: list[BaseMessage],
        stop: list[str] | None = None,
        run_manager: object | None = None,
        **kwargs: Any,
    ) -> ChatResult:
        del messages, stop, run_manager, kwargs
        self.model_calls += 1
        if self.model_calls == 1:
            message = AIMessage(
                content="",
                tool_calls=[
                    {
                        "args": {},
                        "id": "call:refreshed",
                        "name": "cases.read",
                        "type": "tool_call",
                    }
                ],
            )
        else:
            message = AIMessage(content="done")
        return ChatResult(generations=[ChatGeneration(message=message)])


def declared_child_tool() -> BaseTool:
    def task(child: str) -> str:
        """Invoke one declared child agent."""
        return child

    return StructuredTool.from_function(task, name="task")


def build_tool(
    session: KiteframeSessionContext,
    invoker: object,
) -> CapabilityTool:
    requirement = load_resolved_agent(
        (WORKSPACE / "tests/fixtures/resolved/support-agent.json").read_bytes()
    ).capability_requirements[0]
    return build_capability_tools(
        (requirement,),
        session.grants,
        grant_digest=session.grant_digest,
        invoker=cast(Any, invoker),
        session=session,
        checkpoint_store=FakeCheckpointStore(),
        suspension_bridge=FakeSuspensionBridge(),
    )[0]


@pytest.fixture
def capability_tool() -> CapabilityTool:
    session = session_context()
    return build_tool(session, FakeInvoker())


@pytest.fixture
def middleware(capability_tool: CapabilityTool) -> KiteframeGuardMiddleware:
    return KiteframeGuardMiddleware(
        session=session_context(),
        admitted_tools=(capability_tool,),
        clock=FixedClock(ISSUED_AT),
    )


def tool_names(tools: tuple[BaseTool, ...]) -> set[str]:
    return {tool.name for tool in tools}


def model_request(
    tools: list[BaseTool | dict[str, Any]],
) -> ModelRequest[Any]:
    return ModelRequest(
        model=cast(Any, object()),
        messages=[],
        tools=tools,
    )


def forged_tool_call(name: str, tool: BaseTool | None = None) -> ToolCallRequest:
    return ToolCallRequest(
        tool_call={"name": name, "args": {}, "id": f"call:{name}"},
        tool=tool,
        state={},
        runtime=cast(Any, None),
    )


async def should_not_run(request: ToolCallRequest) -> ToolMessage:
    raise AssertionError(f"forged request reached handler: {request}")


@pytest.mark.asyncio
async def test_expired_or_revoked_grants_disappear_from_each_model_request(
    middleware: KiteframeGuardMiddleware,
) -> None:
    assert tool_names(
        await middleware.visible_tools(now=ISSUED_AT)
    ) == {"cases.read"}

    next_session = session_context(revision="8")
    revoked = replace(
        middleware,
        authority_provider=FixedAuthorityProvider(next_session),
        tool_registry=FixedToolRegistry(middleware.admitted_tools),
    )
    assert tool_names(
        await revoked.visible_tools(now=ISSUED_AT)
    ) == set()
    assert tool_names(
        await middleware.visible_tools(now=EXPIRED_AT)
    ) == set()


@pytest.mark.asyncio
async def test_authority_refresh_replaces_the_complete_immutable_snapshot(
    middleware: KiteframeGuardMiddleware,
) -> None:
    original_session = middleware.session
    next_session = session_context(revision="8")
    next_tool = build_tool(next_session, FakeInvoker())

    refreshed = middleware.with_authority(next_session, (next_tool,))

    assert refreshed is not middleware
    assert refreshed.session is next_session
    assert refreshed.session.authority_revisions is next_session.authority_revisions
    assert refreshed.admitted_tools == (next_tool,)
    assert middleware.session is original_session
    assert (
        middleware.session.authority_revisions.authority_revision_digest
        != refreshed.session.authority_revisions.authority_revision_digest
    )


@pytest.mark.asyncio
async def test_atomic_authority_replacement_uses_complete_context_and_tools(
    middleware: KiteframeGuardMiddleware,
) -> None:
    next_session = session_context(revision="8")
    next_tool = build_tool(next_session, FakeInvoker())

    refreshed = middleware.with_authority(next_session, (next_tool,))

    assert refreshed.session is next_session
    assert refreshed.admitted_tools == (next_tool,)
    assert refreshed.authority_provider is None
    assert refreshed.tool_registry is None
    assert refreshed.declared_child_tool is None
    assert tool_names(
        await refreshed.visible_tools(now=ISSUED_AT)
    ) == {"cases.read"}
    assert middleware.session.authority_revisions is not (
        next_session.authority_revisions
    )


def test_atomic_authority_replacement_rejects_session_identity_change(
    middleware: KiteframeGuardMiddleware,
) -> None:
    next_session = replace(session_context(revision="8"), actor="actor:mallory")
    next_tool = build_tool(next_session, FakeInvoker())

    with pytest.raises(ValueError, match="identity"):
        middleware.with_authority(next_session, (next_tool,))


def test_atomic_authority_replacement_rejects_stale_tools(
    middleware: KiteframeGuardMiddleware,
) -> None:
    next_session = session_context(revision="8")

    with pytest.raises(ValueError, match="exact authority"):
        middleware.with_authority(next_session, middleware.admitted_tools)


def test_atomic_authority_replacement_rejects_incomplete_tools(
    middleware: KiteframeGuardMiddleware,
) -> None:
    next_session = session_context(revision="8")

    with pytest.raises(ValueError, match="exact authority"):
        middleware.with_authority(next_session, ())


@pytest.mark.asyncio
async def test_session_task_and_suspension_state_remove_tools(
    middleware: KiteframeGuardMiddleware,
) -> None:
    mismatched = replace(
        middleware,
        session=replace(middleware.session, task="task:other"),
    )
    suspension = load_invocation_outcome(
        canonical_bytes(
            {
                "invocation_id": "invocation:1",
                "status": "suspended",
                "suspension": {
                    "checkpointRef": "checkpoint:1",
                    "evidenceKind": "approval",
                    "evidenceRequestRef": "evidence-request:1",
                    "proposalDigest": "ab" * 32,
                },
            }
        )
    ).suspension
    assert suspension is not None
    suspended = replace(
        middleware,
        session=replace(middleware.session, suspension=suspension),
    )

    assert await mismatched.visible_tools(now=ISSUED_AT) == ()
    assert await suspended.visible_tools(now=ISSUED_AT) == ()


@pytest.mark.asyncio
async def test_only_the_exact_declared_child_tool_is_visible(
    middleware: KiteframeGuardMiddleware,
) -> None:
    child = declared_child_tool()
    binding = DeclaredChildTaskTool(
        tool=child,
        declarations=child_declarations(),
        session=middleware.session,
    )
    with_child = replace(middleware, declared_child_tool=binding)

    assert tool_names(
        await with_child.visible_tools(now=ISSUED_AT)
    ) == {"cases.read", "task"}

    forged_same_name = declared_child_tool()
    executed: list[BaseTool | None] = []

    async def capture(request: ToolCallRequest) -> ToolMessage:
        executed.append(request.tool)
        return ToolMessage(content="ok", tool_call_id="call:task")

    await with_child.awrap_tool_call(
        forged_tool_call("task", forged_same_name),
        capture,
    )
    assert executed == [child]


def test_name_correct_undeclared_child_tool_is_rejected(
    middleware: KiteframeGuardMiddleware,
) -> None:
    with pytest.raises(TypeError, match="DeclaredChildTaskTool"):
        replace(middleware, declared_child_tool=declared_child_tool())


@pytest.mark.asyncio
async def test_declared_child_is_hidden_until_authority_snapshot_is_replaced(
    middleware: KiteframeGuardMiddleware,
) -> None:
    child = declared_child_tool()
    binding = DeclaredChildTaskTool(
        tool=child,
        declarations=child_declarations(),
        session=middleware.session,
    )
    changed_authority = session_context(revision="8")
    stale = replace(
        middleware,
        declared_child_tool=binding,
        authority_provider=FixedAuthorityProvider(changed_authority),
    )

    assert await stale.visible_tools(now=ISSUED_AT) == ()

    changed_tool = build_tool(changed_authority, FakeInvoker())
    changed_binding = DeclaredChildTaskTool(
        tool=child,
        declarations=child_declarations(),
        session=changed_authority,
    )
    refreshed = replace(
        middleware.with_authority(changed_authority, (changed_tool,)),
        declared_child_tool=changed_binding,
    )
    assert tool_names(
        await refreshed.visible_tools(now=ISSUED_AT)
    ) == {"cases.read", "task"}


@pytest.mark.asyncio
async def test_each_model_request_replaces_the_original_tool_list(
    middleware: KiteframeGuardMiddleware,
) -> None:
    ambient = StructuredTool.from_function(
        lambda path: path,
        name="read_file",
        description="Ambient read.",
    )
    requests: list[ModelRequest[Any]] = []

    async def capture(request: ModelRequest[Any]) -> ModelResponse[Any]:
        requests.append(request)
        return ModelResponse(result=[])

    original = model_request([ambient])
    await middleware.awrap_model_call(original, capture)

    assert len(requests) == 1
    captured_tools = cast(list[BaseTool], requests[0].tools)
    original_tools = cast(list[BaseTool], original.tools)
    assert {tool.name for tool in captured_tools} == {"cases.read"}
    assert {tool.name for tool in original_tools} == {"read_file"}


@pytest.mark.asyncio
async def test_compiled_graph_executes_tool_rebuilt_for_refreshed_authority(
    capability_tool: CapabilityTool,
) -> None:
    refreshed_session = session_context(revision="8")
    refreshed_invoker = RecordingInvoker(requests=[])
    refreshed_tool = build_tool(refreshed_session, refreshed_invoker)
    model = RefreshToolCallingModel()
    guard = KiteframeGuardMiddleware(
        session=session_context(),
        admitted_tools=(capability_tool,),
        clock=FixedClock(ISSUED_AT),
        authority_provider=FixedAuthorityProvider(refreshed_session),
        tool_registry=FixedToolRegistry((refreshed_tool,)),
    )
    graph = create_agent(
        model=model,
        tools=[capability_tool],
        middleware=[guard],
    )

    result = await graph.ainvoke(
        {"messages": [HumanMessage(content="read the case")]}
    )

    assert result["messages"][-1].content == "done"
    assert model.bound_tools[-1] == (refreshed_tool,)
    assert len(refreshed_invoker.requests) == 1
    assert refreshed_invoker.requests[0].grant_digest == (
        refreshed_session.grant_digest
    )


@pytest.mark.parametrize("name", AMBIENT_NAMES)
@pytest.mark.asyncio
async def test_forged_ambient_call_is_denied(
    name: str,
    middleware: KiteframeGuardMiddleware,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        await middleware.awrap_tool_call(
            forged_tool_call(name),
            should_not_run,
        )
    assert error.value.code == "KF-AUTH-003"


@pytest.mark.asyncio
async def test_hidden_capability_call_is_denied_at_current_time(
    middleware: KiteframeGuardMiddleware,
    capability_tool: CapabilityTool,
) -> None:
    expired = replace(middleware, clock=FixedClock(EXPIRED_AT))

    with pytest.raises(KiteframeDiagnosticError) as error:
        await expired.awrap_tool_call(
            forged_tool_call("cases.read", capability_tool),
            should_not_run,
        )
    assert error.value.code == "KF-AUTH-003"


@pytest.mark.asyncio
async def test_malformed_forged_call_is_stably_denied(
    middleware: KiteframeGuardMiddleware,
) -> None:
    request = forged_tool_call("cases.read")
    request = request.override(tool_call=cast(Any, {"args": {}, "id": "call:1"}))

    with pytest.raises(KiteframeDiagnosticError) as error:
        await middleware.awrap_tool_call(request, should_not_run)
    assert error.value.code == "KF-AUTH-003"


@pytest.mark.parametrize("tool_call", [None, [], "cases.read", 7])
@pytest.mark.asyncio
async def test_non_mapping_forged_call_is_stably_denied(
    tool_call: object,
    middleware: KiteframeGuardMiddleware,
) -> None:
    request = forged_tool_call("cases.read")
    request = request.override(tool_call=cast(Any, tool_call))

    with pytest.raises(KiteframeDiagnosticError) as error:
        await middleware.awrap_tool_call(request, should_not_run)
    assert error.value.code == "KF-AUTH-003"


@pytest.mark.parametrize(
    ("replacement", "expected_code"),
    [
        (
            {"authority_provider": FailingAuthorityProvider()},
            "KF-AUTH-004",
        ),
        (
            {"tool_registry": FailingToolRegistry()},
            "KF-RUNTIME-001",
        ),
        (
            {"clock": FailingClock()},
            "KF-RUNTIME-002",
        ),
    ],
)
@pytest.mark.asyncio
async def test_visibility_failures_never_fall_back_to_original_tools(
    replacement: dict[str, object],
    expected_code: str,
    middleware: KiteframeGuardMiddleware,
) -> None:
    failing = replace(middleware, **replacement)
    handler_called = False

    async def handler(request: ModelRequest[Any]) -> ModelResponse[Any]:
        nonlocal handler_called
        handler_called = True
        return ModelResponse(result=[])

    ambient = StructuredTool.from_function(
        lambda command: command,
        name="execute",
        description="Ambient shell.",
    )
    with pytest.raises(KiteframeDiagnosticError) as error:
        await failing.awrap_model_call(model_request([ambient]), handler)

    assert error.value.code == expected_code
    assert "secret" not in str(error.value)
    assert handler_called is False
