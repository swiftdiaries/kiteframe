"""Pinned public-surface compatibility checks for Deep Agents."""

from __future__ import annotations

import inspect
from dataclasses import dataclass

import deepagents
from deepagents import (
    GeneralPurposeSubagentProfile,
    HarnessProfile,
    create_deep_agent,
)

DEEPAGENTS_VERSION = "0.6.12"
AMBIENT_TOOL_NAMES = frozenset(
    {"ls", "read_file", "write_file", "edit_file", "glob", "grep", "execute"}
)
EXPECTED_CREATE_DEEP_AGENT_PARAMETERS = (
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


@dataclass(frozen=True, slots=True)
class DeepAgentsCompatibility:
    """Attestation that the supported public Deep Agents API is present."""

    deepagents_version: str
    create_deep_agent_parameters: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class KiteframeHarnessProfileToken:
    """A registry token that attests to deployment-installed denial defaults."""

    model_key: str
    deepagents_version: str
    excluded_tools: frozenset[str]
    general_purpose_subagent_disabled: bool


def deny_only_profile() -> HarnessProfile:
    """Return the deployment profile that removes all ambient facilities."""

    return HarnessProfile(
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent=GeneralPurposeSubagentProfile(enabled=False),
    )


def verify_compatibility() -> DeepAgentsCompatibility:
    """Fail closed when the pinned package or public constructor drifts."""

    version = deepagents.__version__
    parameters = tuple(inspect.signature(create_deep_agent).parameters)
    if version != DEEPAGENTS_VERSION:
        msg = f"unsupported deepagents version {version!r}"
        raise RuntimeError(msg)
    if parameters != EXPECTED_CREATE_DEEP_AGENT_PARAMETERS:
        msg = "unsupported deepagents.create_deep_agent public signature"
        raise RuntimeError(msg)
    return DeepAgentsCompatibility(
        deepagents_version=version,
        create_deep_agent_parameters=parameters,
    )
