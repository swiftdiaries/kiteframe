from __future__ import annotations

import hashlib
import json
import shutil
from dataclasses import FrozenInstanceError
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import Mock

import pytest
from deepagents import CompiledSubAgent, create_deep_agent
from kiteframe import (
    AdmissionRequest,
    CapabilityGrantSet,
    EffectiveCapabilityGrant,
    KiteframeDiagnosticError,
    ResolvedCapabilityRequirement,
    ResolvedRuntimeInputs,
    load_admission_request,
    load_capability_grant_set,
    load_resolved_agent,
    resolve_package,
)
from langchain_core.language_models.fake_chat_models import (
    FakeMessagesListChatModel,
)
from langchain_core.messages import AIMessage
from langgraph.graph.state import CompiledStateGraph
from test_compile import frozen_registry, session_context

import kiteframe_deepagents.adapter as adapter_module
from kiteframe_deepagents.adapter import DeepAgentsAdapter
from kiteframe_deepagents.context import (
    KiteframeSessionContext,
    KiteframeTraceContext,
)
from kiteframe_deepagents.delegation import (
    DeclaredSubAgentInput,
    DelegationAncestryEntry,
    bind_child_admission,
    intersect_child_envelope,
)
from kiteframe_deepagents.middleware import (
    DeclaredChildTaskTool,
    KiteframeGuardMiddleware,
)
from kiteframe_deepagents.tools import build_native_invocation_request

WORKSPACE = Path(__file__).resolve().parents[3]
HOUR_1 = 3_600
HOUR_2 = 7_200
RESOURCE = "tenant:support"


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def canonical_digest(domain: bytes, value: object) -> str:
    return hashlib.sha256(domain + canonical_bytes(value)).hexdigest()


def evidence(
    *,
    confirmation: dict[str, object] | None = None,
) -> dict[str, object]:
    return {
        "approval": {"kind": "none"},
        "confirmation": confirmation or {"kind": "none"},
        "consent": {"kind": "none"},
    }


def grant(
    name: str = "cases.read",
    resource: str = RESOURCE,
    *,
    version: str = "1.2.0",
    modes: tuple[str, ...] = ("immediate",),
    effect: str = "read_only",
    expires: int = HOUR_2,
    freshness: dict[str, object] | None = None,
    required_evidence: dict[str, object] | None = None,
) -> EffectiveCapabilityGrant:
    mode_rank = {"immediate": 0, "deferred": 1, "suspendable": 2}
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
        "admissionId": "admission:child",
        "admissionRequestDigest": "09" * 32,
        "agent": "agent:child",
        "authorityRevisions": authority_revisions,
        "catalogDigest": "01" * 32,
        "catalogIdentity": {
            "name": "provider.test",
            "revision": "revision-1",
        },
        "expiresAt": expires,
        "grants": [
            {
                "capability": {"name": name, "version": version},
                "executionModes": sorted(modes, key=mode_rank.__getitem__),
                "expiresAt": expires,
                "freshness": freshness
                or {
                    "maxAdmissionAgeSeconds": None,
                    "maxInputAgeSeconds": None,
                    "policyRevisionRequired": False,
                },
                "maximumEffect": effect,
                "preconditions": [],
                "requiredEvidence": required_evidence or evidence(),
                "resources": [resource],
            }
        ],
        "issuedAt": 100,
        "optionalDenials": [],
        "policyRevision": "policy:7",
        "session": "session:child",
        "task": "task:delegated",
    }
    values["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        values,
    )
    return load_capability_grant_set(canonical_bytes(values)).grants[0]


@pytest.fixture
def requirement() -> ResolvedCapabilityRequirement:
    resolved = load_resolved_agent(
        (WORKSPACE / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )
    return resolved.capability_requirements[0]


def assert_admission_denied(call: object) -> None:
    assert isinstance(call, KiteframeDiagnosticError)
    assert call.code == "KF-AUTH-001"
    assert b"cases." not in call.diagnostics_json


def test_child_cannot_receive_capability_missing_from_parent(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as caught:
        intersect_child_envelope(
            parent=(),
            delegation=(grant(),),
            child_requirements=(requirement,),
            child_admission=(grant(),),
        )

    assert_admission_denied(caught.value)


def test_child_selector_and_expiry_are_narrower(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    admitted = grant(expires=HOUR_1)

    child = intersect_child_envelope(
        parent=(grant("cases.read", "tenant:*", expires=HOUR_2),),
        delegation=(grant(expires=HOUR_2),),
        child_requirements=(requirement,),
        child_admission=(admitted,),
    )

    assert child.grants == (admitted,)
    assert child.grants[0].resources == (RESOURCE,)
    assert child.expires_at == HOUR_1


def test_child_exact_version_must_match_every_term(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as caught:
        intersect_child_envelope(
            parent=(grant(),),
            delegation=(grant(),),
            child_requirements=(requirement,),
            child_admission=(grant(version="1.3.0"),),
        )

    assert_admission_denied(caught.value)


def test_child_effect_and_modes_can_only_narrow(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    admitted = grant(effect="read_only", modes=("immediate",))

    child = intersect_child_envelope(
        parent=(
            grant(
                effect="reversible_write",
                modes=("deferred", "immediate", "suspendable"),
            ),
        ),
        delegation=(
            grant(
                effect="reversible_write",
                modes=("deferred", "immediate"),
            ),
        ),
        child_requirements=(requirement,),
        child_admission=(admitted,),
    )

    assert child.grants == (admitted,)


@pytest.mark.parametrize(
    ("parent", "admitted"),
    [
        (
            grant(effect="read_only"),
            grant(effect="reversible_write"),
        ),
        (
            grant(modes=("immediate",)),
            grant(modes=("deferred", "immediate")),
        ),
        (
            grant(expires=HOUR_1),
            grant(expires=HOUR_2),
        ),
    ],
)
def test_child_cannot_broaden_effect_mode_or_expiry(
    requirement: ResolvedCapabilityRequirement,
    parent: EffectiveCapabilityGrant,
    admitted: EffectiveCapabilityGrant,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as caught:
        intersect_child_envelope(
            parent=(parent,),
            delegation=(grant(expires=HOUR_2),),
            child_requirements=(requirement,),
            child_admission=(admitted,),
        )

    assert_admission_denied(caught.value)


def test_child_freshness_must_not_be_weaker(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    strict = {
        "maxAdmissionAgeSeconds": 60,
        "maxInputAgeSeconds": 30,
        "policyRevisionRequired": True,
    }
    weak = {
        "maxAdmissionAgeSeconds": None,
        "maxInputAgeSeconds": 60,
        "policyRevisionRequired": False,
    }

    with pytest.raises(KiteframeDiagnosticError) as caught:
        intersect_child_envelope(
            parent=(grant(freshness=strict),),
            delegation=(grant(freshness=strict),),
            child_requirements=(requirement,),
            child_admission=(grant(freshness=weak),),
        )

    assert_admission_denied(caught.value)


def test_child_required_evidence_must_not_be_weaker(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    required = evidence(
        confirmation={
            "evidence": {"issuer": "user", "kind": "confirmation"},
            "kind": "required",
        }
    )

    with pytest.raises(KiteframeDiagnosticError) as caught:
        intersect_child_envelope(
            parent=(grant(required_evidence=required),),
            delegation=(grant(required_evidence=required),),
            child_requirements=(requirement,),
            child_admission=(grant(required_evidence=evidence()),),
        )

    assert_admission_denied(caught.value)


def test_explicit_delegation_absence_removes_the_grant(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as caught:
        intersect_child_envelope(
            parent=(grant(),),
            delegation=(),
            child_requirements=(requirement,),
            child_admission=(grant(),),
        )

    assert_admission_denied(caught.value)


def test_child_ancestry_is_extended_as_an_immutable_snapshot(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    root = DelegationAncestryEntry(
        parent_agent="agent:root",
        child_agent="agent:parent",
        delegated_capabilities=("cases.read",),
    )

    child = intersect_child_envelope(
        parent=(grant(),),
        delegation=(grant(),),
        child_requirements=(requirement,),
        child_admission=(grant(),),
        ancestry=(root,),
        parent_agent="agent:parent",
        child_agent="agent:child",
    )

    assert child.ancestry == (
        root,
        DelegationAncestryEntry(
            parent_agent="agent:parent",
            child_agent="agent:child",
            delegated_capabilities=("cases.read",),
        ),
    )
    with pytest.raises(FrozenInstanceError):
        child.ancestry[-1].child_agent = "agent:forged"  # pyright: ignore[reportAttributeAccessIssue]


def test_delegation_ancestry_rejects_cycles_and_duplicate_edges(
    requirement: ResolvedCapabilityRequirement,
) -> None:
    root = DelegationAncestryEntry(
        parent_agent="agent:root",
        child_agent="agent:parent",
        delegated_capabilities=("cases.read",),
    )

    for parent_agent, child_agent in (
        ("agent:parent", "agent:root"),
        ("agent:root", "agent:parent"),
    ):
        with pytest.raises(KiteframeDiagnosticError) as caught:
            intersect_child_envelope(
                parent=(grant(),),
                delegation=(grant(),),
                child_requirements=(requirement,),
                child_admission=(grant(),),
                ancestry=(root,),
                parent_agent=parent_agent,
                child_agent=child_agent,
            )

        assert_admission_denied(caught.value)


def resolved_parent_and_child(
    tmp_path: Path,
) -> tuple[ResolvedRuntimeInputs, ResolvedRuntimeInputs]:
    source = WORKSPACE / "tests/fixtures/packages/support-agent"
    package = tmp_path / "delegating-agent"
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
    (child / "prompts/system.md").write_text("Read support cases safely.\n")
    lock_path = package / "capability.lock"
    lock = json.loads(lock_path.read_bytes())
    lock["packagePortableDigest"] = (
        "c9d348731150757463bb3f7926f0016aded8141a9ea3d4a95897fb939a703025"
    )
    lock["lockDigest"] = (
        "b1d0fbd64d25b0abc323185027e2bdefe4bbcca88ca1594fa52bae01f89d0a81"
    )
    lock_path.write_bytes(canonical_bytes(lock))
    child_lock = dict(lock)
    child_lock["packagePortableDigest"] = (
        "506a229f8ddf397c0caa044b0fce344ea0e92214b69e6ff209d17b5e1f73b069"
    )
    child_lock["lockDigest"] = (
        "4545be62393b2d55e643202f61f2b6f58c9ae62752855db3974dea4ad587ebc1"
    )
    (child / "capability.lock").write_bytes(canonical_bytes(child_lock))
    binding = package / "bindings/deepagents.yaml"
    binding.write_text(
        """\
apiVersion: kiteframe.dev/binding/v1alpha1
kind: RuntimeBinding
metadata: { runtime: deepagents }
spec:
  models: { primary: models.anthropic.sonnet }
  components:
    backend: backends.workspace
    middleware: [middleware.tenant-context]
    harnessProfile: profiles.deepagents
  capabilityProvider: capability-providers.primary
  auditSink: audit-sinks.ledger
"""
    )
    components = WORKSPACE / "tests/fixtures/components/deepagents-test.json"
    parent_inputs = resolve_package(package, binding, components)

    child_binding = child / "bindings/deepagents.yaml"
    child_binding.parent.mkdir()
    shutil.copyfile(binding, child_binding)
    child_inputs = resolve_package(child, child_binding, components)
    return parent_inputs, child_inputs


def compiled_graph() -> CompiledStateGraph:
    return create_deep_agent(
        model=FakeMessagesListChatModel(responses=[AIMessage(content="done")]),
        subagents=[],
    )


def child_spec(
    parent: ResolvedRuntimeInputs,
    child: ResolvedRuntimeInputs,
) -> DeclaredSubAgentInput:
    admission_request, admission, session = child_admission(child)
    return DeclaredSubAgentInput(
        declaration=parent.resolved_agent.subagents[0],
        runtime_inputs=child,
        session=session,
        admission_request=admission_request,
        admission=admission,
        children=(),
    )


def child_admission(
    child: ResolvedRuntimeInputs,
    *,
    revisions: tuple[tuple[str, str], ...] = (("policy", "7"),),
    native_ancestry: tuple[str, ...] = ("agent:support-agent",),
    resolved_digest: str | None = None,
) -> tuple[AdmissionRequest, CapabilityGrantSet, KiteframeSessionContext]:
    resolved_wire = json.loads(child.resolved_agent.canonical_json())
    requirements = resolved_wire["capabilityRequirements"]
    required = [
        {
            "capability": requirement["lockedCapability"]["identity"],
            "resources": requirement["resources"],
        }
        for requirement in requirements
        if requirement["required"]
    ]
    optional = [
        {
            "capability": requirement["lockedCapability"]["identity"],
            "resources": requirement["resources"],
        }
        for requirement in requirements
        if not requirement["required"]
    ]
    request_values: dict[str, object] = {
        "actor": "actor:alice",
        "agent": f"agent:{child.resolved_agent.package_name}",
        "task": "task:compile",
        "session": "session:compile-1",
        "portableDigest": child.resolved_agent.portable_digest,
        "lockDigest": child.resolved_agent.lock_digest,
        "resolvedDigest": resolved_digest or child.resolved_agent.resolved_digest,
        "catalogIdentity": {
            "name": child.resolved_agent.catalog_name,
            "revision": child.resolved_agent.catalog_revision,
        },
        "catalogDigest": child.resolved_agent.catalog_digest,
        "requiredCapabilities": required,
        "optionalCapabilities": optional,
        "resolvedRequirements": requirements,
        "delegationAncestry": list(native_ancestry),
        "contextualFacts": {},
        "traceContext": {
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        },
    }
    request_values["requestDigest"] = canonical_digest(
        b"kiteframe:admission-request:v1\0",
        request_values,
    )
    request = load_admission_request(canonical_bytes(request_values))

    authority_entries = [
        {"revision": revision, "source": source} for source, revision in revisions
    ]
    authority_revisions = {
        "authorityRevisionDigest": canonical_digest(
            b"kiteframe:authority-revision-set:v1\0",
            authority_entries,
        ),
        "entries": authority_entries,
    }
    session_source = session_context(with_case_grant=True)
    grant_values = [
        {
            "capability": {"name": value.name, "version": value.version},
            "executionModes": list(value.execution_modes),
            "expiresAt": value.expires_at,
            "freshness": value.freshness,
            "maximumEffect": value.maximum_effect,
            "preconditions": value.preconditions,
            "requiredEvidence": value.required_evidence,
            "resources": list(value.resources),
        }
        for value in session_source.grants
    ]
    admission_values: dict[str, object] = {
        "actor": request.actor,
        "admissionId": "admission:child",
        "admissionRequestDigest": request.request_digest,
        "agent": request.agent,
        "authorityRevisions": authority_revisions,
        "catalogDigest": request.catalog_digest,
        "catalogIdentity": {
            "name": request.catalog_name,
            "revision": request.catalog_revision,
        },
        "expiresAt": 4_000_000_000,
        "grants": grant_values,
        "issuedAt": 1_900_000_000,
        "optionalDenials": [],
        "policyRevision": "policy:7",
        "session": request.session,
        "task": request.task,
    }
    admission_values["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        admission_values,
    )
    admission = load_capability_grant_set(canonical_bytes(admission_values))
    session = KiteframeSessionContext(
        actor=admission.actor,
        session=admission.session,
        task=admission.task,
        admission_id=admission.admission_id,
        grant_digest=admission.grant_digest,
        grants=admission.grants,
        authority_revisions=admission.authority_revisions,
        trace_context=KiteframeTraceContext(
            traceparent=request.traceparent,
        ),
    )
    return request, admission, session


def test_child_revision_set_must_extend_the_parent_authority_history(
    tmp_path: Path,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    root_session = session_context(with_case_grant=True)
    _, _, unrelated_session = child_admission(
        child,
        revisions=(("policy", "8"),),
    )

    with pytest.raises(KiteframeDiagnosticError) as caught:
        intersect_child_envelope(
            parent=root_session.grants,
            delegation=parent.resolved_agent.subagents[0],
            child_requirements=child.resolved_agent.capability_requirements,
            child_admission=unrelated_session,
            parent_authority_revisions=root_session.authority_revisions,
        )

    assert_admission_denied(caught.value)


def test_recursive_compile_rejects_forged_native_child_ancestry_preflight(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    request, admission, session = child_admission(
        child,
        native_ancestry=("agent:forged",),
    )
    declared = DeclaredSubAgentInput(
        declaration=parent.resolved_agent.subagents[0],
        runtime_inputs=child,
        session=session,
        admission_request=request,
        admission=admission,
    )
    create_spy = Mock(return_value=compiled_graph())
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    with pytest.raises(KiteframeDiagnosticError) as caught:
        DeepAgentsAdapter().compile(
            parent,
            frozen_registry(parent),
            session_context(with_case_grant=True),
            declared_children=(declared,),
        )

    assert_admission_denied(caught.value)
    create_spy.assert_not_called()


def test_recursive_compile_rejects_admission_for_other_resolved_child(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    request, admission, session = child_admission(
        child,
        resolved_digest="ff" * 32,
    )
    declared = DeclaredSubAgentInput(
        declaration=parent.resolved_agent.subagents[0],
        runtime_inputs=child,
        session=session,
        admission_request=request,
        admission=admission,
    )
    create_spy = Mock(return_value=compiled_graph())
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)

    with pytest.raises(KiteframeDiagnosticError) as caught:
        DeepAgentsAdapter().compile(
            parent,
            frozen_registry(parent),
            session_context(with_case_grant=True),
            declared_children=(declared,),
        )

    assert_admission_denied(caught.value)
    create_spy.assert_not_called()


def test_child_invocation_rechecks_bound_native_admission_correlation(
    tmp_path: Path,
) -> None:
    _parent, child = resolved_parent_and_child(tmp_path)
    request, admission, session = child_admission(child)
    correlated = bind_child_admission(
        session,
        request,
        admission,
        (
            DelegationAncestryEntry(
                parent_agent="agent:support-agent",
                child_agent="agent:case-child",
                delegated_capabilities=("cases.read",),
            ),
        ),
    )
    object.__setattr__(correlated, "admission_id", "admission:forged")

    with pytest.raises(
        ValueError,
        match="child admission correlation does not match",
    ):
        build_native_invocation_request(
            requirement=child.resolved_agent.capability_requirements[0],
            grant=correlated.grants[0],
            grant_digest=correlated.grant_digest,
            session=correlated,
            resource=RESOURCE,
            arguments={},
            idempotency_key=None,
        )


def test_recursive_compile_builds_real_public_child_before_parent_task_tool(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    child_graph = compiled_graph()
    parent_graph = compiled_graph()
    create_spy = Mock(side_effect=[child_graph, parent_graph])
    captured_children: list[tuple[CompiledSubAgent, ...]] = []
    real_builder = adapter_module.build_declared_child_task_tool

    def capture_builder(**kwargs: Any) -> DeclaredChildTaskTool:
        captured_children.append(kwargs["compiled_children"])
        return real_builder(**kwargs)

    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)
    monkeypatch.setattr(
        adapter_module,
        "build_declared_child_task_tool",
        capture_builder,
    )
    root_session = session_context(with_case_grant=True)

    graph = DeepAgentsAdapter().compile(
        parent,
        frozen_registry(parent),
        root_session,
        declared_children=(child_spec(parent, child),),
    )

    assert graph is parent_graph
    assert create_spy.call_count == 2
    assert captured_children == [
        (
            {
                "name": "case-child",
                "description": "Read support cases safely.",
                "runnable": child_graph,
            },
        )
    ]
    child_kwargs, parent_kwargs = [call.kwargs for call in create_spy.call_args_list]
    child_guard = child_kwargs["middleware"][-1]
    parent_guard = parent_kwargs["middleware"][-1]
    assert isinstance(child_guard, KiteframeGuardMiddleware)
    assert isinstance(parent_guard, KiteframeGuardMiddleware)
    assert child_guard.session is not root_session
    assert parent_guard.session is not root_session
    assert parent_guard.declared_child_tool is not None
    assert parent_guard.declared_child_tool.session is not root_session
    assert {tool.name for tool in parent_kwargs["tools"]} == {
        "cases.read",
        "task",
    }
    assert child_kwargs["subagents"] is None
    assert parent_kwargs["subagents"] is None


def test_recursive_compile_rejects_duplicate_declared_child_identity(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    create_spy = Mock(return_value=compiled_graph())
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)
    declared = child_spec(parent, child)

    with pytest.raises(KiteframeDiagnosticError) as caught:
        DeepAgentsAdapter().compile(
            parent,
            frozen_registry(parent),
            session_context(with_case_grant=True),
            declared_children=(declared, declared),
        )

    assert_admission_denied(caught.value)
    create_spy.assert_not_called()


def test_recursive_compile_rejects_same_public_child_name_with_other_identity(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    create_spy = Mock(return_value=compiled_graph())
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)
    first = child_spec(parent, child)
    second = child_spec(parent, child)
    object.__setattr__(
        second,
        "declaration",
        SimpleNamespace(
            package_name=first.declaration.package_name,
            package_version="0.2.0",
            resolved_digest="ff" * 32,
        ),
    )

    with pytest.raises(KiteframeDiagnosticError) as caught:
        DeepAgentsAdapter().compile(
            parent,
            frozen_registry(parent),
            session_context(with_case_grant=True),
            declared_children=(first, second),
        )

    assert_admission_denied(caught.value)
    create_spy.assert_not_called()


def test_recursive_compile_rejects_undeclared_child_package(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, child = resolved_parent_and_child(tmp_path)
    create_spy = Mock(return_value=compiled_graph())
    monkeypatch.setattr(adapter_module, "create_deep_agent", create_spy)
    declared = child_spec(parent, child)
    object.__setattr__(declared, "runtime_inputs", parent)

    with pytest.raises(KiteframeDiagnosticError) as caught:
        DeepAgentsAdapter().compile(
            parent,
            frozen_registry(parent),
            session_context(with_case_grant=True),
            declared_children=(declared,),
        )

    assert_admission_denied(caught.value)
    create_spy.assert_not_called()
