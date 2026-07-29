from __future__ import annotations

import builtins
import hashlib
import json
import shutil
from pathlib import Path
from typing import Any

import pytest
from kiteframe import (
    ComponentKind,
    ComponentRegistry,
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
    resolve_package,
)
from langchain.agents.middleware import AgentMiddleware
from langchain_core.language_models.fake_chat_models import FakeMessagesListChatModel
from langchain_core.messages import AIMessage

from kiteframe_deepagents.adapter import DeepAgentsAdapter
from kiteframe_deepagents.compatibility import (
    AMBIENT_TOOL_NAMES,
    KiteframeHarnessProfileToken,
)
from kiteframe_deepagents.components import (
    DurableCheckpointer,
)
from kiteframe_deepagents.target import (
    DEEPAGENTS_UPSTREAM_COMMIT,
    EXPECTED_CREATE_DEEP_AGENT_PARAMETERS,
    SUPPORTED_FEATURES,
    render_target_metadata,
)

WORKSPACE = Path(__file__).resolve().parents[3]
MODEL_SYMBOL = "models.anthropic.sonnet"
MODEL_KEY = "kiteframe-test:adapter"


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


class AdapterTestModel(FakeMessagesListChatModel):
    model_name: str = MODEL_KEY


class FirstMiddleware(AgentMiddleware):
    pass


class SecondMiddleware(AgentMiddleware):
    pass


class EphemeralCheckpointer:
    kiteframe_durable = False

    async def aget_tuple(self, config: object) -> None:
        del config


class TestDurableCheckpointer:
    kiteframe_durable = True

    async def aget_tuple(self, config: object) -> None:
        del config


@pytest.fixture
def adapter() -> DeepAgentsAdapter:
    return DeepAgentsAdapter()


@pytest.fixture
def runtime_inputs() -> ResolvedRuntimeInputs:
    package = WORKSPACE / "tests/fixtures/packages/support-agent"
    return resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        WORKSPACE / "tests/fixtures/components/deepagents-test.json",
    )


@pytest.fixture
def suspendable_inputs(tmp_path: Path) -> ResolvedRuntimeInputs:
    source = WORKSPACE / "tests/fixtures/packages/support-agent"
    package = tmp_path / "support-agent"
    shutil.copytree(source, package)

    lock_path = package / "capability.lock"
    lock = json.loads(lock_path.read_bytes())
    descriptor = lock["capabilities"][0]["descriptor"]
    descriptor["executionModes"] = ["immediate", "suspendable"]
    digest_input = {
        key: value for key, value in descriptor.items() if key != "descriptorDigest"
    }
    descriptor_digest = hashlib.sha256(canonical_bytes(digest_input)).hexdigest()
    descriptor["descriptorDigest"] = descriptor_digest
    lock["capabilities"][0]["descriptorDigest"] = descriptor_digest
    safety = {
        key: descriptor[key]
        for key in (
            "executionModes",
            "resourceSelectorSchema",
            "effect",
            "idempotency",
            "freshness",
            "preconditions",
            "confirmation",
            "approval",
            "consent",
        )
    }
    safety_hasher = hashlib.sha256()
    safety_hasher.update(b"kiteframe.dev/capability-descriptor/safety-metadata\0")
    safety_hasher.update(canonical_bytes(safety))
    lock["capabilities"][0]["safetyMetadataDigest"] = safety_hasher.hexdigest()
    lock_material = {key: value for key, value in lock.items() if key != "lockDigest"}
    lock["lockDigest"] = hashlib.sha256(canonical_bytes(lock_material)).hexdigest()
    lock_path.write_bytes(canonical_bytes(lock))
    (package / "bindings/deepagents.yaml").write_text(
        """\
apiVersion: kiteframe.dev/binding/v1alpha1
kind: RuntimeBinding
metadata: { runtime: deepagents }
spec:
  models: { primary: models.anthropic.sonnet }
  components:
    checkpointer: checkpointers.durable
    harnessProfile: profiles.deepagents
  capabilityProvider: capability-providers.primary
  auditSink: audit-sinks.ledger
"""
    )

    return resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        WORKSPACE / "tests/fixtures/components/deepagents-test.json",
    )


def profile_token(model_key: str = MODEL_KEY) -> KiteframeHarnessProfileToken:
    return KiteframeHarnessProfileToken(
        model_key=model_key,
        deepagents_version="0.6.12",
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent_disabled=True,
    )


def registry_for(
    inputs: ResolvedRuntimeInputs,
    *,
    include_model: bool = True,
    checkpointer: object | None = None,
    profile: KiteframeHarnessProfileToken | None = None,
) -> FrozenComponentRegistry:
    registry = ComponentRegistry()
    profile_symbol = inputs.runtime_binding.harness_profile
    assert profile_symbol is not None
    if include_model:
        registry.register(
            ComponentKind.MODEL,
            MODEL_SYMBOL,
            AdapterTestModel(responses=[AIMessage(content="done")]),
        )
    registry.register(
        ComponentKind.CAPABILITY_PROVIDER,
        inputs.runtime_binding.capability_provider,
        object(),
    )
    registry.register(
        ComponentKind.AUDIT_SINK,
        inputs.runtime_binding.audit_sink,
        object(),
    )
    if inputs.runtime_binding.checkpointer is not None:
        assert checkpointer is not None
        registry.register(
            ComponentKind.CHECKPOINTER,
            inputs.runtime_binding.checkpointer,
            checkpointer,
        )
    registry.register(
        ComponentKind.HARNESS_PROFILE,
        profile_symbol,
        profile or profile_token(),
    )
    return registry.freeze()


def test_target_and_features_are_exact() -> None:
    adapter = DeepAgentsAdapter()

    assert adapter.target() == "deepagents"
    assert adapter.supported_features() == frozenset(
        {
            "kiteframe.runtime.deepagents.public-create@1",
            "kiteframe.capability.point-of-use-auth@1",
            "kiteframe.capability.dynamic-visibility@1",
            "kiteframe.capability.deferred@1",
            "kiteframe.capability.suspendable@1",
            "kiteframe.delegation.narrowing@1",
        }
    )
    assert adapter.supported_features() is SUPPORTED_FEATURES
    assert DEEPAGENTS_UPSTREAM_COMMIT == "196a0870fcf8a7f29d1fb37886dd323b190f9c16"
    assert EXPECTED_CREATE_DEEP_AGENT_PARAMETERS[-3:] == ("debug", "name", "cache")
    assert (
        WORKSPACE / "runtime-targets/deepagents-0.6.12.json"
    ).read_bytes() == render_target_metadata()


def test_missing_model_symbol_fails_before_constructor(
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    called = False

    def forbidden_constructor(*args: object, **kwargs: object) -> None:
        nonlocal called
        del args, kwargs
        called = True

    monkeypatch.setattr(
        "kiteframe_deepagents.compatibility.create_deep_agent",
        forbidden_constructor,
    )

    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(
            runtime_inputs,
            registry_for(runtime_inputs, include_model=False),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert called is False


def test_suspendable_capability_requires_durable_checkpointer(
    adapter: DeepAgentsAdapter,
    suspendable_inputs: ResolvedRuntimeInputs,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(
            suspendable_inputs,
            registry_for(
                suspendable_inputs,
                checkpointer=EphemeralCheckpointer(),
            ),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert "durable checkpointer" in str(error.value)


def test_validation_returns_components_in_binding_order(
    adapter: DeepAgentsAdapter,
    tmp_path: Path,
) -> None:
    package = tmp_path / "support-agent"
    shutil.copytree(
        WORKSPACE / "tests/fixtures/packages/support-agent",
        package,
    )
    binding = package / "bindings/deepagents.yaml"
    binding.write_text(
        """\
apiVersion: kiteframe.dev/binding/v1alpha1
kind: RuntimeBinding
metadata: { runtime: deepagents }
spec:
  models: { primary: models.anthropic.sonnet }
  components:
    middleware: [middleware.tenant-context, middleware.second]
    backend: backends.workspace
    harnessProfile: profiles.bound-deepagents
  capabilityProvider: capability-providers.primary
  auditSink: audit-sinks.ledger
"""
    )
    target = tmp_path / "target.json"
    metadata = json.loads(
        (
            WORKSPACE / "tests/fixtures/components/deepagents-test.json"
        ).read_bytes()
    )
    metadata["components"]["middleware.second"] = {"kind": "middleware"}
    metadata["components"]["profiles.bound-deepagents"] = {
        "kind": "harness_profile"
    }
    target.write_bytes(canonical_bytes(metadata))
    inputs = resolve_package(package, binding, target)
    registry = ComponentRegistry()
    model = AdapterTestModel(responses=[AIMessage(content="done")])
    first = FirstMiddleware()
    second = SecondMiddleware()
    backend = object()
    provider = object()
    audit_sink = object()
    registry.register(ComponentKind.MODEL, MODEL_SYMBOL, model)
    registry.register(
        ComponentKind.MIDDLEWARE,
        "middleware.tenant-context",
        first,
    )
    registry.register(ComponentKind.MIDDLEWARE, "middleware.second", second)
    registry.register(ComponentKind.BACKEND, "backends.workspace", backend)
    registry.register(
        ComponentKind.CAPABILITY_PROVIDER,
        "capability-providers.primary",
        provider,
    )
    registry.register(ComponentKind.AUDIT_SINK, "audit-sinks.ledger", audit_sink)
    registry.register(
        ComponentKind.HARNESS_PROFILE,
        "profiles.bound-deepagents",
        profile_token(),
    )

    components = adapter.validate(inputs, registry.freeze())

    assert components.models == (("primary", model),)
    assert components.primary_model is model
    assert components.middleware == (first, second)
    assert components.package_backend is backend
    assert components.capability_provider is provider
    assert components.audit_sink is audit_sink
    assert components.checkpointer is None
    assert components.store is None
    assert components.compilation_report.decisions == (
        ("features", "0 required and 0 optional enabled"),
        ("models", "1 roles resolved"),
    )


def test_profile_token_must_attest_the_resolved_model_key(
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(
            runtime_inputs,
            registry_for(
                runtime_inputs,
                profile=profile_token("kiteframe-test:wrong-model"),
            ),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert "harness profile" in str(error.value)


def test_validation_is_resolution_only(
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    registry = registry_for(runtime_inputs)

    def forbidden(*args: object, **kwargs: object) -> Any:
        del args, kwargs
        raise AssertionError("validation attempted a source or catalog read")

    monkeypatch.setattr(builtins, "open", forbidden)
    monkeypatch.setattr(Path, "read_bytes", forbidden)
    monkeypatch.setattr(Path, "read_text", forbidden)
    monkeypatch.setattr(json, "loads", forbidden)
    monkeypatch.setattr(
        "kiteframe_deepagents.compatibility.register_harness_profile",
        forbidden,
    )

    components = adapter.validate(runtime_inputs, registry)

    assert isinstance(components.primary_model, AdapterTestModel)
    assert components.primary_model.model_name == MODEL_KEY
    assert isinstance(TestDurableCheckpointer(), DurableCheckpointer)
