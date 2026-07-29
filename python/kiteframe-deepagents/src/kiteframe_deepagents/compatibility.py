"""Pinned public-surface compatibility checks for Deep Agents."""

from __future__ import annotations

import inspect
from dataclasses import dataclass
from threading import Lock

import deepagents
from deepagents import (
    GeneralPurposeSubagentProfile,
    HarnessProfile,
    create_deep_agent,
    register_harness_profile,
)
from kiteframe.registry import (
    ComponentKind,
    ComponentRegistry,
    ComponentUnresolvedError,
)

DEEPAGENTS_VERSION = "0.6.12"
AMBIENT_TOOL_NAMES = frozenset(
    {"ls", "read_file", "write_file", "edit_file", "glob", "grep", "execute"}
)
DENY_ONLY_PROFILE = HarnessProfile(
    excluded_tools=AMBIENT_TOOL_NAMES,
    general_purpose_subagent=GeneralPurposeSubagentProfile(enabled=False),
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


_profile_install_lock = Lock()
_installed_profile_tokens: dict[str, KiteframeHarnessProfileToken] = {}


def deny_only_profile() -> HarnessProfile:
    """Return the deployment profile that removes all ambient facilities."""

    return DENY_ONLY_PROFILE


def bootstrap_deepagents_deployment(
    registry: ComponentRegistry,
    *,
    model_key: str,
    profile_symbol: str,
) -> KiteframeHarnessProfileToken:
    """Install one deny-only profile and bind its token before registry freeze."""

    provider, separator, model_name = model_key.partition(":")
    if (
        separator != ":"
        or not provider
        or not model_name
        or ":" in model_name
    ):
        raise ValueError("model_key must use the exact provider:model form")
    verify_compatibility()
    expected = KiteframeHarnessProfileToken(
        model_key=model_key,
        deepagents_version=DEEPAGENTS_VERSION,
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent_disabled=True,
    )
    with _profile_install_lock:
        token = _installed_profile_tokens.get(model_key)
        if token is None:
            register_harness_profile(model_key, DENY_ONLY_PROFILE)
            token = expected
            _installed_profile_tokens[model_key] = token
        elif token != expected:
            msg = f"incompatible deny-only profile for model {model_key!r}"
            raise RuntimeError(msg)

    try:
        registered = registry.resolve(ComponentKind.HARNESS_PROFILE, profile_symbol)
    except ComponentUnresolvedError:
        registry.register(ComponentKind.HARNESS_PROFILE, profile_symbol, token)
    else:
        if registered is not token:
            msg = f"harness profile symbol {profile_symbol!r} is already registered"
            raise RuntimeError(msg)
    return token


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
