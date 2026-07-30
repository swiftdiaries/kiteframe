from __future__ import annotations

from collections import deque
from concurrent.futures import ThreadPoolExecutor
from threading import Lock
from typing import Any
from unittest.mock import Mock

import pytest
from deepagents import CompiledSubAgent
from deepagents.backends import StateBackend
from kiteframe import KiteframeDiagnosticError
from langchain.agents.middleware import AgentMiddleware
from langgraph.graph.state import CompiledStateGraph
from test_compile import frozen_registry, session_context
from test_delegation import (
    child_spec,
    compiled_graph,
    resolved_parent_and_child,
)

import kiteframe_deepagents.adapter as adapter_module
from kiteframe_deepagents.adapter import DeepAgentsAdapter
from kiteframe_deepagents.middleware import (
    DeclaredChildTaskTool,
    KiteframeGuardMiddleware,
)
from kiteframe_deepagents.tools import _uuid7

SESSION_COUNT = 100


class MutableStateBackend(StateBackend):
    """A configured mutable backend whose session state must be copied."""

    def __init__(self) -> None:
        super().__init__()
        self.session_state: list[str] = []


class NonIsolatableStateBackend(StateBackend):
    def __deepcopy__(self, memo: dict[int, object]) -> NonIsolatableStateBackend:
        del memo
        return self


class IdentityPreservingMutableMiddleware(AgentMiddleware):
    def __init__(self) -> None:
        self.session_state: list[str] = []

    def __deepcopy__(
        self,
        memo: dict[int, object],
    ) -> IdentityPreservingMutableMiddleware:
        del memo
        return self


def test_one_hundred_concurrent_declared_child_sessions_do_not_leak_state(
    tmp_path: Any,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    configured_backend = MutableStateBackend()
    registry = frozen_registry(parent, backend=configured_backend)
    graphs = deque(compiled_graph() for _ in range(SESSION_COUNT * 2))
    constructor_calls: list[dict[str, Any]] = []
    compiled_children: list[tuple[CompiledSubAgent, ...]] = []
    lock = Lock()
    real_builder = adapter_module.build_declared_child_task_tool

    def isolated_constructor(**kwargs: Any) -> CompiledStateGraph:
        with lock:
            constructor_calls.append(kwargs)
            return graphs.popleft()

    def capture_builder(**kwargs: Any) -> DeclaredChildTaskTool:
        result = real_builder(**kwargs)
        with lock:
            compiled_children.append(kwargs["compiled_children"])
        return result

    monkeypatch.setattr(
        adapter_module,
        "create_deep_agent",
        isolated_constructor,
    )
    monkeypatch.setattr(
        adapter_module,
        "build_declared_child_task_tool",
        capture_builder,
    )

    def compile_session(_index: int) -> CompiledStateGraph:
        return DeepAgentsAdapter().compile(
            parent,
            registry,
            session_context(with_case_grant=True),
            declared_children=(child_spec(parent, child),),
        )

    with ThreadPoolExecutor(max_workers=20) as executor:
        results = tuple(executor.map(compile_session, range(SESSION_COUNT)))

    child_calls = [
        kwargs for kwargs in constructor_calls if kwargs["name"] == "case-child"
    ]
    parent_calls = [
        kwargs for kwargs in constructor_calls if kwargs["name"] == "support-agent"
    ]
    all_calls = [*child_calls, *parent_calls]
    guards = [kwargs["middleware"][-1] for kwargs in all_calls]
    deployment_middleware = [kwargs["middleware"][0] for kwargs in all_calls]
    capability_tools = [
        next(tool for tool in kwargs["tools"] if tool.name == "cases.read")
        for kwargs in all_calls
    ]
    runtime_backends = [kwargs["backend"].runtime_backend for kwargs in all_calls]
    for index, backend in enumerate(runtime_backends):
        backend.session_state.append(str(index))

    assert len(results) == SESSION_COUNT
    assert len({id(graph) for graph in results}) == SESSION_COUNT
    assert len(child_calls) == SESSION_COUNT
    assert len(parent_calls) == SESSION_COUNT
    assert len(compiled_children) == SESSION_COUNT
    assert (
        len({id(children[0]["runnable"]) for children in compiled_children})
        == SESSION_COUNT
    )
    assert all(isinstance(guard, KiteframeGuardMiddleware) for guard in guards)
    assert len({id(guard) for guard in guards}) == SESSION_COUNT * 2
    assert len({id(guard.session) for guard in guards}) == SESSION_COUNT * 2
    assert (
        len({id(middleware) for middleware in deployment_middleware})
        == SESSION_COUNT * 2
    )
    assert len({id(tool) for tool in capability_tools}) == SESSION_COUNT * 2
    assert (
        len({id(tool.session.grants) for tool in capability_tools}) == SESSION_COUNT * 2
    )
    assert len({id(kwargs["backend"]) for kwargs in all_calls}) == SESSION_COUNT * 2
    assert len({id(backend) for backend in runtime_backends}) == SESSION_COUNT * 2
    assert all(len(backend.session_state) == 1 for backend in runtime_backends)
    assert configured_backend.session_state == []
    assert all(
        kwargs["model"] == "anthropic:claude-3-5-haiku-latest" for kwargs in all_calls
    )
    assert all(
        registry is not value for kwargs in all_calls for value in kwargs.values()
    )


def test_configured_backend_that_cannot_be_isolated_fails_before_construction(
    tmp_path: Any,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    create_spy = Mock(return_value=compiled_graph())
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    with pytest.raises(
        KiteframeDiagnosticError,
        match="runtime session isolation is unresolved",
    ):
        DeepAgentsAdapter().compile(
            parent,
            frozen_registry(parent, backend=NonIsolatableStateBackend()),
            session_context(with_case_grant=True),
            declared_children=(child_spec(parent, child),),
        )

    create_spy.assert_not_called()


def test_concurrent_identity_preserving_mutable_middleware_is_rejected(
    tmp_path: Any,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    configured_middleware = IdentityPreservingMutableMiddleware()
    registry = frozen_registry(parent, middleware=configured_middleware)
    create_spy = Mock(return_value=compiled_graph())
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    def compile_session(_index: int) -> str:
        with pytest.raises(
            KiteframeDiagnosticError,
            match="runtime session isolation is unresolved",
        ) as caught:
            DeepAgentsAdapter().compile(
                parent,
                registry,
                session_context(with_case_grant=True),
                declared_children=(child_spec(parent, child),),
            )
        return caught.value.code

    with ThreadPoolExecutor(max_workers=20) as executor:
        codes = tuple(executor.map(compile_session, range(SESSION_COUNT)))

    assert codes == ("KF-RUNTIME-001",) * SESSION_COUNT
    assert configured_middleware.session_state == []
    create_spy.assert_not_called()


def test_concurrent_idempotency_keys_are_session_independent() -> None:
    with ThreadPoolExecutor(max_workers=20) as executor:
        keys = tuple(executor.map(lambda _index: _uuid7(), range(SESSION_COUNT)))

    assert len(set(keys)) == SESSION_COUNT
    assert all(key.version == 7 for key in keys)
