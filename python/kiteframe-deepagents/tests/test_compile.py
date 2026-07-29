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
from kiteframe import (
    ComponentKind,
    ComponentRegistry,
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
    load_capability_grant_set,
    resolve_package,
)
from langchain.agents.middleware import AgentMiddleware
from langchain_core.language_models.fake_chat_models import (
    FakeMessagesListChatModel,
)
from langchain_core.messages import AIMessage
from langgraph.graph.state import CompiledStateGraph

import kiteframe_deepagents.adapter as adapter_module
import kiteframe_deepagents.compatibility as compatibility
from kiteframe_deepagents.adapter import DeepAgentsAdapter
from kiteframe_deepagents.compatibility import (
    AMBIENT_TOOL_NAMES,
    KiteframeHarnessProfileToken,
    bootstrap_deepagents_deployment,
)
from kiteframe_deepagents.context import (
    KiteframeSessionContext,
    KiteframeTraceContext,
)
from kiteframe_deepagents.middleware import KiteframeGuardMiddleware
from kiteframe_deepagents.tools import CapabilityTool

WORKSPACE = Path(__file__).resolve().parents[3]
MODEL_KEY = "anthropic:claude-3-5-haiku-latest"
TRACEPARENT = (
    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
)
ISSUED_AT = 1_900_000_000
EXPIRES_AT = 4_000_000_000


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def canonical_digest(domain: bytes, value: object) -> str:
    return hashlib.sha256(domain + canonical_bytes(value)).hexdigest()


def session_context(*, with_case_grant: bool) -> KiteframeSessionContext:
    authority_entries = [{"revision": "7", "source": "policy"}]
    authority_revisions = {
        "authorityRevisionDigest": canonical_digest(
            b"kiteframe:authority-revision-set:v1\0",
            authority_entries,
        ),
        "entries": authority_entries,
    }
    grants: list[dict[str, object]] = []
    if with_case_grant:
        grants.append(
            {
                "capability": {"name": "cases.read", "version": "1.2.0"},
                "executionModes": ["immediate"],
                "expiresAt": EXPIRES_AT,
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
        )
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
        "expiresAt": EXPIRES_AT,
        "grants": grants,
        "issuedAt": ISSUED_AT,
        "optionalDenials": [],
        "policyRevision": "policy:7",
        "session": "session:compile-1",
        "task": "task:compile",
    }
    values["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        values,
    )
    grant_set = load_capability_grant_set(canonical_bytes(values))
    return KiteframeSessionContext(
        actor=grant_set.actor,
        session=grant_set.session,
        task=grant_set.task,
        admission_id=grant_set.admission_id,
        grant_digest=grant_set.grant_digest,
        grants=grant_set.grants,
        authority_revisions=grant_set.authority_revisions,
        trace_context=KiteframeTraceContext(traceparent=TRACEPARENT),
    )


class TenantMiddleware(AgentMiddleware):
    pass


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


class ChangingSessionContext(KiteframeSessionContext):
    """Adversarial subclass whose authority changes between property reads."""

    def __getattribute__(self, name: str) -> Any:
        if name == "grant_digest":
            reads = object.__getattribute__(self, "__dict__").get(
                "_grant_reads",
                0,
            )
            object.__getattribute__(self, "__dict__")["_grant_reads"] = (
                reads + 1
            )
            if reads % 2:
                return "ff" * 32
        return super().__getattribute__(name)


def changing_session_context(
    source: KiteframeSessionContext,
) -> ChangingSessionContext:
    return ChangingSessionContext(
        actor=source.actor,
        session=source.session,
        task=source.task,
        admission_id=source.admission_id,
        grant_digest=source.grant_digest,
        grants=source.grants,
        authority_revisions=source.authority_revisions,
        trace_context=source.trace_context,
        suspension=source.suspension,
    )


@pytest.fixture
def adapter() -> DeepAgentsAdapter:
    return DeepAgentsAdapter()


@pytest.fixture
def runtime_inputs(tmp_path: Path) -> ResolvedRuntimeInputs:
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
    middleware: [middleware.tenant-context]
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
def skill_inputs(tmp_path: Path) -> ResolvedRuntimeInputs:
    source = WORKSPACE / "tests/fixtures/packages/digest/skill-a"
    package = tmp_path / "skill-agent"
    shutil.copytree(source, package)
    (package / "agent.yaml").write_text(
        """\
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: support-agent, version: 1.0.0 }
spec:
  prompt: { system: prompts/system.md }
  skills: [skills/case-summary.md]
  models:
    primary: { capabilities: [text] }
"""
    )
    (package / "prompts/system.md").write_text(
        "You are the support agent.\n"
    )
    (package / "skills/rules.md").unlink()
    (package / "skills/case-summary.md").write_text(
        "---\n"
        "name: case-summary\n"
        "description: Summarize a support case.\n"
        "---\n"
        "Use only admitted case data.\n"
    )
    lock: dict[str, object] = {
        "capabilities": [],
        "catalogDigest": "01" * 32,
        "catalogIdentity": "support",
        "catalogRevision": "v1",
        "packagePortableDigest": (
            "fb298b91e7d806e392ad53902c54747a"
            "5804bbe1820ebfe73bd757d310793916"
        ),
        "resolvedFeatures": [],
        "resolverVersion": "0.1.0",
        "schemaVersion": "kiteframe.dev/lock/v1alpha1",
    }
    lock["lockDigest"] = hashlib.sha256(canonical_bytes(lock)).hexdigest()
    (package / "capability.lock").write_bytes(canonical_bytes(lock))
    binding = package / "bindings/deepagents.yaml"
    binding.parent.mkdir()
    binding.write_text(
        """\
apiVersion: kiteframe.dev/binding/v1alpha1
kind: RuntimeBinding
metadata: { runtime: deepagents }
spec:
  models: { primary: models.anthropic.haiku }
  components: { harnessProfile: profiles.deepagents }
  capabilityProvider: capability-providers.primary
  auditSink: audit-sinks.ledger
"""
    )
    return resolve_package(
        package,
        binding,
        WORKSPACE / "tests/fixtures/components/deepagents-test.json",
    )


@pytest.fixture
def child_inputs(tmp_path: Path) -> ResolvedRuntimeInputs:
    source = WORKSPACE / "tests/fixtures/packages/support-agent"
    package = tmp_path / "child-agent"
    shutil.copytree(source, package)
    (package / "agent.yaml").write_text(
        """\
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: support-agent, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text, tool-calling] }
  capabilities:
    - { name: cases.read, version: "^1.0", required: true, resources: [tenant:support] }
  delegation:
    - agent: agents/case-child/agent.yaml
      capabilities: [cases.read]
"""
    )
    child = package / "agents/case-child"
    (child / "prompts").mkdir(parents=True)
    (child / "agent.yaml").write_text(
        """\
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: case-child, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text] }
  capabilities:
    - { name: cases.read, version: "^1.0", required: true, resources: [tenant:support] }
"""
    )
    (child / "prompts/system.md").write_text(
        "Read support cases safely.\n"
    )
    lock_path = package / "capability.lock"
    lock = json.loads(lock_path.read_bytes())
    lock["packagePortableDigest"] = (
        "c9d348731150757463bb3f7926f0016a"
        "ded8141a9ea3d4a95897fb939a703025"
    )
    lock["lockDigest"] = (
        "b1d0fbd64d25b0abc323185027e2bdef"
        "e4bbcca88ca1594fa52bae01f89d0a81"
    )
    lock_path.write_bytes(canonical_bytes(lock))
    child_lock = dict(lock)
    child_lock["packagePortableDigest"] = (
        "506a229f8ddf397c0caa044b0fce344e"
        "a0e92214b69e6ff209d17b5e1f73b069"
    )
    child_lock["lockDigest"] = (
        "4545be62393b2d55e643202f61f2b6f5"
        "8c9ae62752855db3974dea4ad587ebc1"
    )
    (child / "capability.lock").write_bytes(canonical_bytes(child_lock))
    binding = package / "bindings/deepagents.yaml"
    return resolve_package(
        package,
        binding,
        WORKSPACE / "tests/fixtures/components/deepagents-test.json",
    )


def frozen_registry(
    inputs: ResolvedRuntimeInputs,
    *,
    install_profile: bool = False,
) -> FrozenComponentRegistry:
    registry = ComponentRegistry()
    primary_symbol = dict(inputs.runtime_binding.model_symbols)["primary"]
    registry.register(ComponentKind.MODEL, primary_symbol, MODEL_KEY)
    registry.register(
        ComponentKind.CAPABILITY_PROVIDER,
        inputs.runtime_binding.capability_provider,
        TestCapabilityInvoker(),
    )
    registry.register(
        ComponentKind.AUDIT_SINK,
        inputs.runtime_binding.audit_sink,
        TestAuditSink(),
    )
    for symbol in inputs.runtime_binding.middleware_symbols:
        registry.register(
            ComponentKind.MIDDLEWARE,
            symbol,
            TenantMiddleware(),
        )
    profile_symbol = inputs.runtime_binding.harness_profile
    assert profile_symbol is not None
    if install_profile:
        bootstrap_deepagents_deployment(
            registry,
            model_key=MODEL_KEY,
            profile_symbol=profile_symbol,
        )
    else:
        registry.register(
            ComponentKind.HARNESS_PROFILE,
            profile_symbol,
            KiteframeHarnessProfileToken(
                model_key=MODEL_KEY,
                deepagents_version="0.6.12",
                excluded_tools=AMBIENT_TOOL_NAMES,
                general_purpose_subagent_disabled=True,
            ),
        )
    return registry.freeze()


@pytest.fixture
def compiled_graph() -> CompiledStateGraph:
    return create_deep_agent(
        model=FakeMessagesListChatModel(
            responses=[AIMessage(content="done")]
        ),
        subagents=[],
    )


def test_compile_returns_public_compiled_state_graph(
    monkeypatch: pytest.MonkeyPatch,
    adapter: DeepAgentsAdapter,
    skill_inputs: ResolvedRuntimeInputs,
) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-only-not-a-secret")

    graph = adapter.compile(
        skill_inputs,
        frozen_registry(skill_inputs, install_profile=True),
        session_context(with_case_grant=False),
    )

    assert isinstance(graph, CompiledStateGraph)


def test_constructor_receives_only_resolved_and_registered_values(
    monkeypatch: pytest.MonkeyPatch,
    compiled_graph: CompiledStateGraph,
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    registry = frozen_registry(runtime_inputs)
    create_spy = Mock(return_value=compiled_graph)
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    graph = adapter.compile(
        runtime_inputs,
        registry,
        session_context(with_case_grant=True),
    )

    assert graph is compiled_graph
    create_spy.assert_called_once()
    kwargs = create_spy.call_args.kwargs
    assert kwargs["system_prompt"] == (
        "Help support agents read cases safely.\n"
    )
    model = registry.resolve(
        ComponentKind.MODEL,
        "models.anthropic.sonnet",
    )
    assert kwargs["model"] is model
    assert kwargs["name"] == "support-agent"
    assert kwargs["skills"] is None
    assert kwargs["subagents"] is None
    assert kwargs["memory"] is None
    assert kwargs["permissions"] is None
    assert kwargs["interrupt_on"] is None
    assert kwargs["checkpointer"] is None
    assert kwargs["store"] is None
    assert len(kwargs["tools"]) == 1
    assert isinstance(kwargs["tools"][0], CapabilityTool)
    requirement = runtime_inputs.resolved_agent.capability_requirements[0]
    assert (
        kwargs["tools"][0].requirement.name,
        kwargs["tools"][0].requirement.version,
        kwargs["tools"][0].requirement.descriptor_digest,
    ) == (
        requirement.name,
        requirement.version,
        requirement.descriptor_digest,
    )
    assert isinstance(kwargs["middleware"][-1], KiteframeGuardMiddleware)
    assert isinstance(kwargs["middleware"][0], TenantMiddleware)
    assert kwargs["middleware"][-1].admitted_tools == kwargs["tools"]


def test_compile_snapshots_the_exact_immutable_session(
    monkeypatch: pytest.MonkeyPatch,
    compiled_graph: CompiledStateGraph,
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    original = session_context(with_case_grant=True)
    create_spy = Mock(return_value=compiled_graph)
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    adapter.compile(
        runtime_inputs,
        frozen_registry(runtime_inputs),
        original,
    )

    kwargs = create_spy.call_args.kwargs
    guard = kwargs["middleware"][-1]
    assert type(guard.session) is KiteframeSessionContext
    assert guard.session is not original
    assert guard.session.grant_digest == original.grant_digest
    assert guard.session.grants is not original.grants
    assert kwargs["tools"][0].session is guard.session

    object.__setattr__(original, "grant_digest", "ff" * 32)
    object.__setattr__(original.trace_context, "traceparent", "00-mutated")
    assert guard.session.grant_digest != original.grant_digest
    assert guard.session.trace_context.traceparent != "00-mutated"


def test_session_subclass_is_rejected_before_public_construction(
    monkeypatch: pytest.MonkeyPatch,
    compiled_graph: CompiledStateGraph,
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    create_spy = Mock(return_value=compiled_graph)
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)
    forged = changing_session_context(
        session_context(with_case_grant=True)
    )
    assert forged.grant_digest != forged.grant_digest

    with pytest.raises(TypeError, match="exact KiteframeSessionContext"):
        adapter.compile(
            runtime_inputs,
            frozen_registry(runtime_inputs),
            forged,
        )

    create_spy.assert_not_called()


def test_declared_children_fail_before_public_construction(
    monkeypatch: pytest.MonkeyPatch,
    compiled_graph: CompiledStateGraph,
    adapter: DeepAgentsAdapter,
    child_inputs: ResolvedRuntimeInputs,
) -> None:
    create_spy = Mock(return_value=compiled_graph)
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.compile(
            child_inputs,
            frozen_registry(child_inputs),
            session_context(with_case_grant=False),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert "declared child" in str(error.value)
    create_spy.assert_not_called()


def test_validated_skills_are_exposed_only_by_the_virtual_package_backend(
    monkeypatch: pytest.MonkeyPatch,
    compiled_graph: CompiledStateGraph,
    adapter: DeepAgentsAdapter,
    skill_inputs: ResolvedRuntimeInputs,
) -> None:
    create_spy = Mock(return_value=compiled_graph)
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    adapter.compile(
        skill_inputs,
        frozen_registry(skill_inputs),
        session_context(with_case_grant=False),
    )

    kwargs = create_spy.call_args.kwargs
    assert kwargs["skills"] == ["/__kiteframe__/skills/case-summary"]
    backend = kwargs["backend"]
    source = kwargs["skills"][0]
    listing = backend.ls(source)
    assert listing.error is None
    assert listing.entries == [
        {
            "is_dir": True,
            "modified_at": "",
            "path": f"{source}/case-summary/",
            "size": 0,
        }
    ]
    response = backend.download_files(
        [f"{source}/case-summary/SKILL.md"]
    )[0]
    assert response.error is None
    assert response.content == (
        b"---\n"
        b"name: case-summary\n"
        b"description: Summarize a support case.\n"
        b"---\n"
        b"Use only admitted case data.\n"
    )
    prompt = backend.download_files(
        ["/__kiteframe__/prompts/system.md"]
    )[0]
    assert prompt.error is None
    assert prompt.content == b"You are the support agent.\n"
    denied = backend.write(
        f"{source}/case-summary/SKILL.md",
        "forged",
    )
    assert denied.error == "permission_denied"


@pytest.mark.parametrize(
    "alias",
    [
        "//__kiteframe__/skills/case-summary/case-summary/SKILL.md",
        "/runtime/../__kiteframe__/skills/case-summary/case-summary/SKILL.md",
    ],
)
def test_virtual_package_aliases_cannot_escape_to_the_runtime_backend(
    alias: str,
    monkeypatch: pytest.MonkeyPatch,
    compiled_graph: CompiledStateGraph,
    adapter: DeepAgentsAdapter,
    skill_inputs: ResolvedRuntimeInputs,
) -> None:
    create_spy = Mock(return_value=compiled_graph)
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)
    adapter.compile(
        skill_inputs,
        frozen_registry(skill_inputs),
        session_context(with_case_grant=False),
    )
    backend = create_spy.call_args.kwargs["backend"]

    denied = backend.write(alias, "forged")

    assert denied.error == "permission_denied"


def test_all_validation_finishes_before_public_construction(
    monkeypatch: pytest.MonkeyPatch,
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    called = False

    def forbidden_constructor(**kwargs: object) -> None:
        nonlocal called
        del kwargs
        called = True

    monkeypatch.setattr(
        adapter_module,
        "create_deep_agent",
        forbidden_constructor,
    )
    registry = ComponentRegistry().freeze()

    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.compile(
            runtime_inputs,
            registry,
            session_context(with_case_grant=True),
        )

    assert error.value.code == "KF-RUNTIME-001"
    assert called is False


def test_compile_uses_closed_inputs_without_rereads_or_profile_mutation(
    monkeypatch: pytest.MonkeyPatch,
    compiled_graph: CompiledStateGraph,
    adapter: DeepAgentsAdapter,
    skill_inputs: ResolvedRuntimeInputs,
) -> None:
    registry = frozen_registry(skill_inputs)
    session = session_context(with_case_grant=False)
    create_spy = Mock(return_value=compiled_graph)

    def forbidden(*args: object, **kwargs: object) -> Any:
        del args, kwargs
        raise AssertionError("compile attempted a source, catalog, or profile write")

    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)
    monkeypatch.setattr(builtins, "open", forbidden)
    monkeypatch.setattr(Path, "read_bytes", forbidden)
    monkeypatch.setattr(Path, "read_text", forbidden)
    monkeypatch.setattr(json, "loads", forbidden)
    monkeypatch.setattr(
        compatibility,
        "register_harness_profile",
        forbidden,
    )

    adapter.compile(skill_inputs, registry, session)

    create_spy.assert_called_once()


def test_constructor_failures_are_redacted_to_component_and_class(
    monkeypatch: pytest.MonkeyPatch,
    adapter: DeepAgentsAdapter,
    runtime_inputs: ResolvedRuntimeInputs,
) -> None:
    class SecretConstructionError(Exception):
        pass

    def fail_constructor(**kwargs: object) -> None:
        del kwargs
        raise SecretConstructionError(
            "prompt=private model=provider:credential response=secret"
        )

    monkeypatch.setattr(
        adapter_module,
        "create_deep_agent",
        fail_constructor,
    )

    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.compile(
            runtime_inputs,
            frozen_registry(runtime_inputs),
            session_context(with_case_grant=True),
        )

    assert error.value.code == "KF-RUNTIME-002"
    rendered = str(error.value)
    assert "models.anthropic.sonnet" in rendered
    assert "SecretConstructionError" in rendered
    for secret in ("private", "credential", "response=secret", MODEL_KEY):
        assert secret not in rendered
        assert secret.encode() not in error.value.diagnostics_json
