from __future__ import annotations

import inspect
from collections.abc import Sequence
from typing import Any

import deepagents
import pytest
from deepagents import create_deep_agent
from kiteframe.registry import ComponentKind, ComponentRegistry
from langchain_anthropic import ChatAnthropic
from langchain_core.messages import AIMessage, HumanMessage

import kiteframe_deepagents.compatibility as compatibility
from kiteframe_deepagents.compatibility import (
    AMBIENT_TOOL_NAMES,
    DENY_ONLY_PROFILE,
    KiteframeHarnessProfileToken,
    bootstrap_deepagents_deployment,
    deny_only_profile,
    verify_compatibility,
)

EXPECTED_PARAMETERS = (
    "model",
    "tools",
    "system_prompt",
    "middleware",
    "subagents",
    "skills",
    "memory",
    "permissions",
    "backend",
    "interrupt_on",
    "response_format",
    "state_schema",
    "context_schema",
    "checkpointer",
    "store",
    "debug",
    "name",
    "cache",
)

MODEL_KEY = "kiteframe-test:deny-only"
CONSTRUCTION_MODEL_KEY = "anthropic:claude-3-5-haiku-latest"


def test_pinned_distribution_and_public_signature() -> None:
    assert deepagents.__version__ == "0.6.12"
    assert tuple(inspect.signature(create_deep_agent).parameters) == EXPECTED_PARAMETERS


def test_deny_only_profile_disables_ambient_facilities() -> None:
    profile = deny_only_profile()

    assert profile.excluded_tools == frozenset(
        {"ls", "read_file", "write_file", "edit_file", "glob", "grep", "execute"}
    )
    assert profile.general_purpose_subagent is not None
    assert profile.general_purpose_subagent.enabled is False


def test_compatibility_attests_the_pinned_public_surface() -> None:
    compatibility = verify_compatibility()

    assert compatibility.deepagents_version == "0.6.12"
    assert compatibility.create_deep_agent_parameters == EXPECTED_PARAMETERS


def test_profile_token_attests_the_deployment_profile() -> None:
    token = KiteframeHarnessProfileToken(
        model_key=MODEL_KEY,
        deepagents_version="0.6.12",
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent_disabled=True,
    )

    assert token.model_key == MODEL_KEY
    assert token.excluded_tools == AMBIENT_TOOL_NAMES


def test_bootstrap_installs_static_profile_and_registers_the_matching_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    registrations: list[tuple[str, object]] = []
    model_key = "kiteframe-test:bootstrap"
    registry = ComponentRegistry()

    monkeypatch.setattr(
        compatibility,
        "register_harness_profile",
        lambda key, profile: registrations.append((key, profile)),
    )

    token = bootstrap_deepagents_deployment(
        registry,
        model_key=model_key,
        profile_symbol="profiles.deepagents",
    )

    assert registrations == [(model_key, DENY_ONLY_PROFILE)]
    assert token == KiteframeHarnessProfileToken(
        model_key=model_key,
        deepagents_version="0.6.12",
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent_disabled=True,
    )
    assert (
        registry.freeze().resolve(
            ComponentKind.HARNESS_PROFILE,
            "profiles.deepagents",
        )
        is token
    )


def test_bootstrap_is_idempotent_without_replacing_the_static_profile(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    registrations: list[tuple[str, object]] = []
    model_key = "kiteframe-test:idempotent"
    registry = ComponentRegistry()

    monkeypatch.setattr(
        compatibility,
        "register_harness_profile",
        lambda key, profile: registrations.append((key, profile)),
    )

    first = bootstrap_deepagents_deployment(
        registry,
        model_key=model_key,
        profile_symbol="profiles.deepagents",
    )
    second = bootstrap_deepagents_deployment(
        registry,
        model_key=model_key,
        profile_symbol="profiles.deepagents",
    )

    assert first is second
    assert registrations == [(model_key, DENY_ONLY_PROFILE)]


def test_bootstrap_rejects_ambiguous_bare_profile_key(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    registrations: list[tuple[str, object]] = []
    monkeypatch.setattr(
        compatibility,
        "register_harness_profile",
        lambda key, profile: registrations.append((key, profile)),
    )

    with pytest.raises(ValueError, match="provider:model"):
        bootstrap_deepagents_deployment(
            ComponentRegistry(),
            model_key="bare-model",
            profile_symbol="profiles.deepagents",
        )

    assert registrations == []


def test_bootstrapped_model_string_constructs_deny_only_public_graph(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bound_tool_names: list[tuple[str, ...]] = []
    constructor_model_keys: list[str] = []
    original_bind_tools = ChatAnthropic.bind_tools

    def recording_bind_tools(
        model: ChatAnthropic,
        tools: Sequence[Any],
        **kwargs: Any,
    ) -> Any:
        bound_tool_names.append(tuple(tool.name for tool in tools))
        return original_bind_tools(model, tools, **kwargs)

    def public_constructor(*, model: str, **kwargs: Any) -> Any:
        constructor_model_keys.append(model)
        return create_deep_agent(model=model, **kwargs)

    def local_invoke(*args: Any, **kwargs: Any) -> AIMessage:
        del args, kwargs
        return AIMessage(content="done")

    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-only-not-a-secret")
    monkeypatch.setattr(ChatAnthropic, "bind_tools", recording_bind_tools)
    monkeypatch.setattr(ChatAnthropic, "invoke", local_invoke)
    registry = ComponentRegistry()
    registry.register(
        ComponentKind.MODEL,
        "models.anthropic.haiku",
        CONSTRUCTION_MODEL_KEY,
    )
    token = bootstrap_deepagents_deployment(
        registry,
        model_key=CONSTRUCTION_MODEL_KEY,
        profile_symbol="profiles.deepagents.construction",
    )
    frozen = registry.freeze()
    model_key = frozen.resolve(
        ComponentKind.MODEL,
        "models.anthropic.haiku",
    )
    assert isinstance(model_key, str)

    graph = public_constructor(model=model_key, subagents=[])
    result = graph.invoke({"messages": [HumanMessage(content="hello")]})

    assert graph is not None
    assert result["messages"][-1].content == "done"
    assert constructor_model_keys == [CONSTRUCTION_MODEL_KEY]
    assert constructor_model_keys[0] == model_key == token.model_key
    assert constructor_model_keys[0].encode() == token.model_key.encode()
    assert len(bound_tool_names) == 1
    assert AMBIENT_TOOL_NAMES.isdisjoint(bound_tool_names[0])
    assert "task" not in bound_tool_names[0]
