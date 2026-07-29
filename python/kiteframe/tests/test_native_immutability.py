import json
from pathlib import Path

import pytest

from kiteframe import (
    CapabilityDescriptor,
    ComponentDescriptor,
    KiteframeDiagnosticError,
    ResolvedAgent,
    ResolvedCapabilityRequirement,
    ResolvedModelRequirement,
    ResolvedRuntimeInputs,
    ResolvedSubagent,
    ResolvedTextAsset,
    RuntimeBinding,
    load_resolved_agent,
    resolve_package,
)


@pytest.fixture
def workspace() -> Path:
    return Path(__file__).resolve().parents[3]


@pytest.fixture
def golden_ir(workspace: Path) -> bytes:
    return (workspace / "tests/fixtures/resolved/support-agent.json").read_bytes()


def test_resolved_agent_has_no_public_constructor() -> None:
    with pytest.raises(TypeError):
        ResolvedAgent()  # type: ignore[call-arg]


@pytest.mark.parametrize(
    "projection",
    [ResolvedCapabilityRequirement, ResolvedSubagent],
)
def test_structured_projections_have_no_public_constructor(
    projection: type,
) -> None:
    with pytest.raises(TypeError):
        projection()


def test_resolved_agent_fields_cannot_be_reassigned(golden_ir: bytes) -> None:
    resolved = load_resolved_agent(golden_ir)
    with pytest.raises(AttributeError):
        resolved.resolved_digest = "0" * 64  # type: ignore[misc]


def test_resolved_collections_are_immutable_tuples(golden_ir: bytes) -> None:
    resolved = load_resolved_agent(golden_ir)

    assert isinstance(resolved.capability_requirements, tuple)
    assert isinstance(resolved.subagents, tuple)
    assert isinstance(
        resolved.capability_requirements[0],
        ResolvedCapabilityRequirement,
    )
    assert isinstance(resolved.capability_requirements[0].resources, tuple)


def test_structured_projection_fields_cannot_be_reassigned(
    golden_ir: bytes,
) -> None:
    requirement = load_resolved_agent(golden_ir).capability_requirements[0]

    with pytest.raises(AttributeError):
        requirement.required = False  # type: ignore[misc]


def test_noncanonical_ir_is_rejected(golden_ir: bytes) -> None:
    spaced = b" " + golden_ir
    with pytest.raises(Exception, match="canonical"):
        load_resolved_agent(spaced)


def test_resolve_package_returns_all_compilation_inputs(
    workspace: Path,
) -> None:
    package = workspace / "tests/fixtures/packages/support-agent"
    inputs = resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )

    assert isinstance(inputs, ResolvedRuntimeInputs)
    assert inputs.resolved_agent.package_name == "support-agent"
    assert inputs.resolved_agent.catalog_name == "support"
    assert inputs.resolved_agent.catalog_revision == "v1"
    assert len(inputs.resolved_agent.catalog_digest) == 64
    assert inputs.runtime_target == "deepagents"
    assert inputs.runtime_binding.runtime == "deepagents"
    assert {component.symbol for component in inputs.target_components} >= {
        "models.anthropic.sonnet",
        "capability-providers.primary",
        "audit-sinks.ledger",
    }


def test_runtime_inputs_are_not_constructible_or_mutable(workspace: Path) -> None:
    with pytest.raises(TypeError):
        ResolvedRuntimeInputs()  # type: ignore[call-arg]

    package = workspace / "tests/fixtures/packages/support-agent"
    inputs = resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )
    with pytest.raises(AttributeError):
        inputs.runtime_target = "forged"  # type: ignore[misc]


def test_new_child_projections_are_nonconstructible_and_frozen(
    workspace: Path,
) -> None:
    package = workspace / "tests/fixtures/packages/support-agent"
    inputs = resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )
    resolved = inputs.resolved_agent
    children = (
        (inputs.runtime_binding, RuntimeBinding, "runtime"),
        (inputs.target_components[0], ComponentDescriptor, "symbol"),
        (resolved.prompts[0], ResolvedTextAsset, "path"),
        (resolved.model_requirements[0], ResolvedModelRequirement, "role"),
        (
            resolved.capability_requirements[0],
            ResolvedCapabilityRequirement,
            "required",
        ),
        (
            resolved.capability_requirements[0].descriptor,
            CapabilityDescriptor,
            "summary",
        ),
    )

    for value, projection, attribute in children:
        assert isinstance(value, projection)
        assert not hasattr(value, "__dict__")
        with pytest.raises(TypeError):
            projection()
        with pytest.raises(AttributeError):
            setattr(value, attribute, "forged")


def test_new_projection_collections_are_immutable_tuples(workspace: Path) -> None:
    package = workspace / "tests/fixtures/packages/support-agent"
    inputs = resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )
    resolved = inputs.resolved_agent
    descriptor = resolved.capability_requirements[0].descriptor
    collections = (
        inputs.target_components,
        inputs.runtime_binding.model_symbols,
        inputs.runtime_binding.middleware_symbols,
        resolved.prompts,
        resolved.skills,
        resolved.model_requirements,
        resolved.capability_requirements,
        resolved.subagents,
        resolved.required_features,
        resolved.optional_features,
        resolved.capability_requirements[0].resources,
        descriptor.stable_errors,
        descriptor.execution_modes,
        descriptor.preconditions,
    )

    for collection in collections:
        assert isinstance(collection, tuple)
        with pytest.raises(AttributeError):
            collection.append("forged")  # type: ignore[attr-defined]


def test_resolver_diagnostics_are_exposed_as_redacted_json(
    workspace: Path,
) -> None:
    missing = workspace / "tests/fixtures/packages/does-not-exist"

    with pytest.raises(KiteframeDiagnosticError) as caught:
        resolve_package(missing, missing / "binding.yaml", missing / "target.json")

    diagnostics = json.loads(caught.value.diagnostics_json)
    assert diagnostics[0]["code"] == "KF-PKG-001"
