from __future__ import annotations

import builtins
import hashlib
import json
import shutil
from pathlib import Path
from typing import Any
from unittest.mock import Mock

import pytest
from deepagents import create_deep_agent
from deepagents.backends import StateBackend
from kiteframe import (
    ComponentKind,
    ComponentRegistry,
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
    delegation_ancestry_digest,
    load_capability_grant_set,
    resolve_package,
)
from langchain.agents.middleware import AgentMiddleware
from langchain_core.language_models.fake_chat_models import FakeMessagesListChatModel
from langchain_core.messages import AIMessage
from langgraph.checkpoint.base import BaseCheckpointSaver
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph.state import CompiledStateGraph
from langgraph.store.memory import InMemoryStore

import kiteframe_deepagents.compatibility as compatibility
from kiteframe_deepagents.adapter import DeepAgentsAdapter
from kiteframe_deepagents.compatibility import (
    AMBIENT_TOOL_NAMES,
    DENY_ONLY_PROFILE,
    KiteframeHarnessProfileToken,
    bootstrap_deepagents_deployment,
)
from kiteframe_deepagents.components import (
    DurableCheckpointer,
)
from kiteframe_deepagents.context import (
    KiteframeSessionContext,
    KiteframeTraceContext,
)
from kiteframe_deepagents.suspension import (
    EvidenceResumeCredentialClaims,
)
from kiteframe_deepagents.target import (
    DEEPAGENTS_UPSTREAM_COMMIT,
    EXPECTED_CREATE_DEEP_AGENT_PARAMETERS,
    SUPPORTED_FEATURES,
    render_target_metadata,
)
from kiteframe_deepagents.tools import PersistedInvocationCorrelation

WORKSPACE = Path(__file__).resolve().parents[3]
MODEL_SYMBOL = "models.anthropic.sonnet"
MODEL_KEY = "kiteframe-test:adapter"
MODEL_IDENTIFIER = "adapter"


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def granted_session_context() -> KiteframeSessionContext:
    authority_entries = [{"revision": "7", "source": "policy"}]
    authority_revisions = {
        "authorityRevisionDigest": hashlib.sha256(
            b"kiteframe:authority-revision-set:v1\0"
            + canonical_bytes(authority_entries)
        ).hexdigest(),
        "entries": authority_entries,
    }
    values: dict[str, object] = {
        "actor": "actor:alice",
        "admissionId": "admission:compile-1",
        "admissionRequestDigest": "09" * 32,
        "agent": "agent:support",
        "authorityRevisions": authority_revisions,
        "catalogDigest": "01" * 32,
        "catalogIdentity": {
            "name": "provider.test",
            "revision": "revision-1",
        },
        "expiresAt": 4_000_000_000,
        "grants": [
            {
                "capability": {"name": "cases.read", "version": "1.2.0"},
                "executionModes": ["immediate", "suspendable"],
                "expiresAt": 4_000_000_000,
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
                "resources": ["tenant:support"],
            }
        ],
        "issuedAt": 1_900_000_000,
        "optionalDenials": [],
        "policyRevision": "policy:7",
        "session": "session:compile-1",
        "task": "task:compile",
    }
    values["delegationAncestryDigest"] = delegation_ancestry_digest([])
    values["grantDigest"] = hashlib.sha256(
        b"kiteframe:capability-grant-set:v1\0" + canonical_bytes(values)
    ).hexdigest()
    grant_set = load_capability_grant_set(canonical_bytes(values))
    return KiteframeSessionContext(
        actor=grant_set.actor,
        session=grant_set.session,
        task=grant_set.task,
        admission_id=grant_set.admission_id,
        grant_digest=grant_set.grant_digest,
        delegation_ancestry_digest=grant_set.delegation_ancestry_digest,
        grants=grant_set.grants,
        authority_revisions=grant_set.authority_revisions,
        trace_context=KiteframeTraceContext(
            traceparent=("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        ),
    )


class FirstMiddleware(AgentMiddleware):
    pass


class SecondMiddleware(AgentMiddleware):
    pass


class EphemeralCheckpointer:
    kiteframe_durable = False

    async def aget_tuple(self, config: object) -> None:
        del config


class UnverifiedDurableCheckpointer:
    kiteframe_durable = True

    async def aget_tuple(self, config: object) -> None:
        del config


class TestDurableCheckpointer:
    kiteframe_durable = True

    async def aget_tuple(self, config: object) -> None:
        del config

    def verify_evidence_resume_credential(
        self,
        credential: bytes,
    ) -> EvidenceResumeCredentialClaims:
        del credential
        raise ValueError("test verifier rejects every credential")


class PersistOnlyDurableCheckpointer(TestDurableCheckpointer):
    async def persist_idempotency_key(self, record: object) -> None:
        del record


class SecureDurableCheckpointer(InMemorySaver):
    kiteframe_durable = True

    async def persist_idempotency_key(self, record: object) -> None:
        del record

    async def persist_invocation_correlation(
        self,
        record: PersistedInvocationCorrelation,
    ) -> None:
        del record

    async def load_invocation_correlation(
        self,
        scope: object,
    ) -> PersistedInvocationCorrelation | None:
        del scope
        return None

    def verify_evidence_resume_credential(
        self,
        credential: bytes,
    ) -> EvidenceResumeCredentialClaims:
        del credential
        raise ValueError("test verifier rejects every credential")


class TestCapabilityInvoker:
    async def invoke(self, request: object) -> object:
        return request

    async def status(
        self,
        request: object,
        invocation: object,
        requirement: object,
    ) -> object:
        del invocation, requirement
        return request


class TestAuditSink:
    async def append(self, record: object) -> object:
        return record


class IdentityAuthorityProvider:
    async def current(
        self,
        session: KiteframeSessionContext,
        now: int,
    ) -> KiteframeSessionContext:
        del now
        return session


class EmptyAdmittedToolRegistry:
    async def admitted_tools(
        self,
        session: KiteframeSessionContext,
    ) -> tuple[object, ...]:
        del session
        return ()


@pytest.fixture
def adapter() -> DeepAgentsAdapter:
    return DeepAgentsAdapter()


@pytest.fixture
def compiled_graph() -> CompiledStateGraph:
    return create_deep_agent(
        model=FakeMessagesListChatModel(responses=[AIMessage(content="done")]),
        subagents=[],
    )


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
    authorityProvider: authority.current
    admittedToolRegistry: admitted-tools.dynamic
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


@pytest.fixture
def backend_inputs(tmp_path: Path) -> ResolvedRuntimeInputs:
    source = WORKSPACE / "tests/fixtures/packages/support-agent"
    package = tmp_path / "support-agent"
    shutil.copytree(source, package)
    (package / "bindings/deepagents.yaml").write_text(
        """\
apiVersion: kiteframe.dev/binding/v1alpha1
kind: RuntimeBinding
metadata: { runtime: deepagents }
spec:
  models: { primary: models.anthropic.sonnet }
  components:
    backend: backends.workspace
    authorityProvider: authority.current
    admittedToolRegistry: admitted-tools.dynamic
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


@pytest.fixture
def checkpointed_inputs(tmp_path: Path) -> ResolvedRuntimeInputs:
    source = WORKSPACE / "tests/fixtures/packages/support-agent"
    package = tmp_path / "support-agent"
    shutil.copytree(source, package)
    (package / "bindings/deepagents.yaml").write_text(
        """\
apiVersion: kiteframe.dev/binding/v1alpha1
kind: RuntimeBinding
metadata: { runtime: deepagents }
spec:
  models: { primary: models.anthropic.sonnet }
  components:
    checkpointer: checkpointers.durable
    authorityProvider: authority.current
    admittedToolRegistry: admitted-tools.dynamic
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


@pytest.fixture
def content_capture_inputs(tmp_path: Path) -> ResolvedRuntimeInputs:
    source = WORKSPACE / "tests/fixtures/packages/support-agent-runtime-inputs"
    package = tmp_path / "support-agent-runtime-inputs"
    shutil.copytree(source, package)
    binding = package / "bindings/deepagents.yaml"
    binding.write_text(
        binding.read_text().replace(
            "  capabilityProvider:",
            "  components:\n"
            "    authorityProvider: authority.current\n"
            "    admittedToolRegistry: admitted-tools.dynamic\n"
            "    harnessProfile: profiles.deepagents\n"
            "  capabilityProvider:",
        )
    )
    return resolve_package(
        package,
        binding,
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
    model: object | None = None,
    backend: object | None = None,
    checkpointer: object | None = None,
    store: object | None = None,
    capability_provider: object | None = None,
    audit_sink: object | None = None,
    authority_provider: object | None = None,
    tool_registry: object | None = None,
    profile: KiteframeHarnessProfileToken | None = None,
) -> FrozenComponentRegistry:
    registry = ComponentRegistry()
    profile_symbol = inputs.runtime_binding.harness_profile
    assert profile_symbol is not None
    if include_model:
        registry.register(
            ComponentKind.MODEL,
            MODEL_SYMBOL,
            model or MODEL_KEY,
        )
    registry.register(
        ComponentKind.CAPABILITY_PROVIDER,
        inputs.runtime_binding.capability_provider,
        capability_provider or TestCapabilityInvoker(),
    )
    registry.register(
        ComponentKind.AUDIT_SINK,
        inputs.runtime_binding.audit_sink,
        audit_sink or TestAuditSink(),
    )
    authority_symbol = inputs.runtime_binding.authority_provider
    assert authority_symbol is not None
    registry.register(
        ComponentKind.AUTHORITY_PROVIDER,
        authority_symbol,
        authority_provider or IdentityAuthorityProvider(),
    )
    tool_registry_symbol = inputs.runtime_binding.admitted_tool_registry
    assert tool_registry_symbol is not None
    registry.register(
        ComponentKind.ADMITTED_TOOL_REGISTRY,
        tool_registry_symbol,
        tool_registry or EmptyAdmittedToolRegistry(),
    )
    if inputs.runtime_binding.backend is not None:
        registry.register(
            ComponentKind.BACKEND,
            inputs.runtime_binding.backend,
            backend or StateBackend(),
        )
    if inputs.runtime_binding.checkpointer is not None:
        assert checkpointer is not None
        registry.register(
            ComponentKind.CHECKPOINTER,
            inputs.runtime_binding.checkpointer,
            checkpointer,
        )
    capture = inputs.runtime_binding.content_capture
    if capture is not None and capture.enabled:
        registry.register(
            ComponentKind.ENCRYPTED_CONTENT_STORE,
            capture.encrypted_content_store,
            store or InMemoryStore(),
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


def test_suspendable_capability_requires_restart_stable_resume_verifier(
    adapter: DeepAgentsAdapter,
    suspendable_inputs: ResolvedRuntimeInputs,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(
            suspendable_inputs,
            registry_for(
                suspendable_inputs,
                checkpointer=UnverifiedDurableCheckpointer(),
            ),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert "durable checkpointer" in str(error.value)


def test_suspendable_compile_requires_restartable_invocation_correlation(
    adapter: DeepAgentsAdapter,
    suspendable_inputs: ResolvedRuntimeInputs,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    called = False

    def forbidden_constructor(*args: object, **kwargs: object) -> None:
        nonlocal called
        del args, kwargs
        called = True

    monkeypatch.setattr(
        "kiteframe_deepagents.adapter.create_deep_agent",
        forbidden_constructor,
    )

    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.compile(
            suspendable_inputs,
            registry_for(
                suspendable_inputs,
                checkpointer=PersistOnlyDurableCheckpointer(),
            ),
            granted_session_context(),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert "runtime assembly validation failed" in str(error.value)
    assert called is False


@pytest.mark.asyncio
async def test_suspendable_compile_guards_resume_before_durable_write(
    adapter: DeepAgentsAdapter,
    suspendable_inputs: ResolvedRuntimeInputs,
    compiled_graph: CompiledStateGraph,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    create_spy = Mock(return_value=compiled_graph)
    monkeypatch.setattr(
        "kiteframe_deepagents.adapter.create_deep_agent",
        create_spy,
    )

    adapter.compile(
        suspendable_inputs,
        registry_for(
            suspendable_inputs,
            checkpointer=SecureDurableCheckpointer(),
        ),
        granted_session_context(),
    )

    guarded = create_spy.call_args.kwargs["checkpointer"]
    assert isinstance(guarded, BaseCheckpointSaver)
    with pytest.raises(
        TypeError,
        match="resolver-issued protected evidence reference",
    ):
        await guarded.aput_writes(
            {"configurable": {"thread_id": "forged"}},
            [("__resume__", ["evidence-ref-raw"])],
            "task-id",
        )


def test_configured_checkpointer_is_retained_without_suspension(
    adapter: DeepAgentsAdapter,
    checkpointed_inputs: ResolvedRuntimeInputs,
) -> None:
    checkpointer = EphemeralCheckpointer()

    components = adapter.validate(
        checkpointed_inputs,
        registry_for(checkpointed_inputs, checkpointer=checkpointer),
    )

    assert components.checkpointer is checkpointer


def test_enabled_content_capture_retains_validated_store(
    adapter: DeepAgentsAdapter,
    content_capture_inputs: ResolvedRuntimeInputs,
) -> None:
    store = InMemoryStore()

    components = adapter.validate(
        content_capture_inputs,
        registry_for(content_capture_inputs, store=store),
    )

    assert components.store is store


@pytest.mark.parametrize(
    ("inputs_fixture", "invalid_component"),
    [
        ("backend_inputs", "backend"),
        ("checkpointed_inputs", "checkpointer"),
        ("runtime_inputs", "capability_provider"),
        ("runtime_inputs", "audit_sink"),
    ],
)
def test_runtime_component_types_fail_closed_before_constructor(
    request: pytest.FixtureRequest,
    adapter: DeepAgentsAdapter,
    monkeypatch: pytest.MonkeyPatch,
    inputs_fixture: str,
    invalid_component: str,
) -> None:
    inputs = request.getfixturevalue(inputs_fixture)
    called = False

    def forbidden_constructor(*args: object, **kwargs: object) -> None:
        nonlocal called
        del args, kwargs
        called = True

    monkeypatch.setattr(
        "kiteframe_deepagents.compatibility.create_deep_agent",
        forbidden_constructor,
    )

    if invalid_component == "backend":
        registry = registry_for(inputs, backend=object())
    elif invalid_component == "checkpointer":
        registry = registry_for(inputs, checkpointer=object())
    elif invalid_component == "capability_provider":
        registry = registry_for(inputs, capability_provider=object())
    else:
        registry = registry_for(inputs, audit_sink=object())

    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(inputs, registry)

    assert error.value.code == "KF-RUNTIME-001"
    assert called is False


@pytest.mark.parametrize(
    "failure",
    ["target", "feature", "exact-kind", "type"],
)
def test_preconstruction_fail_closed_matrix_never_calls_constructor(
    failure: str,
    tmp_path: Path,
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
        if failure == "type":
            adapter.validate(
                runtime_inputs,
                registry_for(runtime_inputs, capability_provider=object()),
            )
        else:
            source = WORKSPACE / (
                "python/kiteframe-deepagents/tests/fixtures/unsupported-feature-agent"
                if failure == "feature"
                else "tests/fixtures/packages/support-agent"
            )
            package = tmp_path / source.name
            shutil.copytree(source, package)
            binding = package / "bindings/deepagents.yaml"
            target_data = json.loads(
                (
                    WORKSPACE / "tests/fixtures/components/deepagents-test.json"
                ).read_bytes()
            )
            if failure == "target":
                binding.write_text(
                    binding.read_text().replace(
                        "runtime: deepagents",
                        "runtime: unsupported",
                    )
                )
                target_data["target"] = "unsupported"
            elif failure == "feature":
                target_data["components"][MODEL_SYMBOL]["features"] = [
                    "kiteframe.runtime.review-unsupported@1"
                ]
            else:
                target_data["components"][MODEL_SYMBOL]["kind"] = "audit_sink"
            target = tmp_path / f"{failure}-target.json"
            target.write_bytes(canonical_bytes(target_data))
            inputs = resolve_package(package, binding, target)
            adapter.validate(inputs, registry_for(inputs))

    assert error.value.code == "KF-RUNTIME-001"
    assert called is False


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
    authorityProvider: authority.current
    admittedToolRegistry: admitted-tools.dynamic
    harnessProfile: profiles.bound-deepagents
  capabilityProvider: capability-providers.primary
  auditSink: audit-sinks.ledger
"""
    )
    target = tmp_path / "target.json"
    metadata = json.loads(
        (WORKSPACE / "tests/fixtures/components/deepagents-test.json").read_bytes()
    )
    metadata["components"]["middleware.second"] = {"kind": "middleware"}
    metadata["components"]["profiles.bound-deepagents"] = {"kind": "harness_profile"}
    target.write_bytes(canonical_bytes(metadata))
    inputs = resolve_package(package, binding, target)
    registry = ComponentRegistry()
    model = MODEL_KEY
    first = FirstMiddleware()
    second = SecondMiddleware()
    backend = StateBackend()
    provider = TestCapabilityInvoker()
    audit_sink = TestAuditSink()
    authority_provider = IdentityAuthorityProvider()
    tool_registry = EmptyAdmittedToolRegistry()
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
        ComponentKind.AUTHORITY_PROVIDER,
        "authority.current",
        authority_provider,
    )
    registry.register(
        ComponentKind.ADMITTED_TOOL_REGISTRY,
        "admitted-tools.dynamic",
        tool_registry,
    )
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
    assert components.authority_provider is authority_provider
    assert components.admitted_tool_registry is tool_registry
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


def test_provider_qualified_model_key_is_retained_for_public_construction(
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    components = adapter.validate(
        runtime_inputs,
        registry_for(runtime_inputs, model=MODEL_KEY),
    )

    assert components.models == (("primary", MODEL_KEY),)
    assert components.primary_model == MODEL_KEY
    assert components.primary_model == components.harness_profile.model_key


def test_bootstrap_registration_key_matches_validated_constructor_model_string(
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    registrations: list[tuple[str, object]] = []
    monkeypatch.setattr(
        compatibility,
        "register_harness_profile",
        lambda key, profile: registrations.append((key, profile)),
    )
    registry = ComponentRegistry()
    registry.register(ComponentKind.MODEL, MODEL_SYMBOL, MODEL_KEY)
    registry.register(
        ComponentKind.CAPABILITY_PROVIDER,
        runtime_inputs.runtime_binding.capability_provider,
        TestCapabilityInvoker(),
    )
    registry.register(
        ComponentKind.AUDIT_SINK,
        runtime_inputs.runtime_binding.audit_sink,
        TestAuditSink(),
    )
    authority_symbol = runtime_inputs.runtime_binding.authority_provider
    tool_registry_symbol = runtime_inputs.runtime_binding.admitted_tool_registry
    assert authority_symbol is not None
    assert tool_registry_symbol is not None
    registry.register(
        ComponentKind.AUTHORITY_PROVIDER,
        authority_symbol,
        IdentityAuthorityProvider(),
    )
    registry.register(
        ComponentKind.ADMITTED_TOOL_REGISTRY,
        tool_registry_symbol,
        EmptyAdmittedToolRegistry(),
    )
    profile_symbol = runtime_inputs.runtime_binding.harness_profile
    assert profile_symbol is not None
    token = bootstrap_deepagents_deployment(
        registry,
        model_key=MODEL_KEY,
        profile_symbol=profile_symbol,
    )

    components = adapter.validate(runtime_inputs, registry.freeze())

    assert registrations == [(MODEL_KEY, DENY_ONLY_PROFILE)]
    assert components.primary_model == MODEL_KEY
    assert components.primary_model == token.model_key


def test_prebuilt_model_is_rejected_before_public_constructor(
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
    prebuilt_model = FakeMessagesListChatModel(responses=[AIMessage(content="done")])

    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(
            runtime_inputs,
            registry_for(runtime_inputs, model=prebuilt_model),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert "provider:model string" in str(error.value)
    assert called is False


def test_bare_model_string_is_rejected(
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(
            runtime_inputs,
            registry_for(
                runtime_inputs,
                model=MODEL_IDENTIFIER,
                profile=profile_token(MODEL_IDENTIFIER),
            ),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert "provider:model" in str(error.value)


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

    assert components.primary_model == MODEL_KEY
    assert isinstance(TestDurableCheckpointer(), DurableCheckpointer)
