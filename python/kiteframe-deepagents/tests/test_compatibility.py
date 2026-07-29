from __future__ import annotations

import inspect
from collections.abc import Sequence
from typing import Any

import deepagents
from deepagents import create_deep_agent, register_harness_profile
from langchain_core.language_models.fake_chat_models import FakeMessagesListChatModel
from langchain_core.messages import AIMessage, HumanMessage
from pydantic import Field

from kiteframe_deepagents.compatibility import (
    AMBIENT_TOOL_NAMES,
    KiteframeHarnessProfileToken,
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


class RecordingFakeChatModel(FakeMessagesListChatModel):
    """A public LangChain fake that records the tools exposed to the model."""

    model_name: str = MODEL_KEY
    tool_requests: list[tuple[str, ...]] = Field(default_factory=list)

    def bind_tools(
        self, tools: Sequence[Any], **kwargs: Any
    ) -> RecordingFakeChatModel:
        del kwargs
        self.tool_requests.append(tuple(tool.name for tool in tools))
        return self


def registered_model() -> RecordingFakeChatModel:
    register_harness_profile(MODEL_KEY, deny_only_profile())
    return RecordingFakeChatModel(responses=[AIMessage(content="done")])


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


def test_registered_deny_only_profile_exposes_no_ambient_or_task_tools() -> None:
    model = registered_model()
    graph = create_deep_agent(model=model, subagents=[])

    graph.invoke({"messages": [HumanMessage(content="hello")]})

    assert AMBIENT_TOOL_NAMES.isdisjoint(model.tool_requests[0])
    assert "task" not in model.tool_requests[0]


def test_registered_deny_only_profile_exposes_only_task_for_explicit_subagent() -> None:
    model = registered_model()
    graph = create_deep_agent(
        model=model,
        subagents=[
            {
                "name": "case-research",
                "description": "Research a case.",
                "system_prompt": "Research the case.",
                "model": model,
                "tools": [],
            }
        ],
    )

    graph.invoke({"messages": [HumanMessage(content="hello")]})

    assert AMBIENT_TOOL_NAMES.isdisjoint(model.tool_requests[0])
    assert model.tool_requests[0] == ("write_todos", "task")
