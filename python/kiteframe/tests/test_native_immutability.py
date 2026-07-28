import json
from pathlib import Path

import pytest

from kiteframe import (
    KiteframeDiagnosticError,
    ResolvedAgent,
    ResolvedCapabilityRequirement,
    ResolvedSubagent,
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


def test_resolve_package_uses_the_validated_rust_pipeline(
    workspace: Path,
    golden_ir: bytes,
) -> None:
    package = workspace / "tests/fixtures/packages/support-agent"
    resolved = resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )

    assert resolved.canonical_json() == golden_ir


def test_resolver_diagnostics_are_exposed_as_redacted_json(
    workspace: Path,
) -> None:
    missing = workspace / "tests/fixtures/packages/does-not-exist"

    with pytest.raises(KiteframeDiagnosticError) as caught:
        resolve_package(missing, missing / "binding.yaml", missing / "target.json")

    diagnostics = json.loads(caught.value.diagnostics_json)
    assert diagnostics[0]["code"] == "KF-PKG-001"
