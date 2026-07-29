import builtins
import json
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import kiteframe
from kiteframe import ResolvedAgent, load_resolved_agent, resolve_package


@dataclass(frozen=True, slots=True)
class FrozenCapabilityRecord:
    identity: tuple[str, str]
    required: bool
    resources: tuple[str, ...]
    summary: str
    input_schema: object
    output_schema: object
    stable_errors: tuple[tuple[str, str, str, str], ...]
    execution_modes: tuple[str, ...]
    safety: tuple[object, ...]
    digests: tuple[str, str, str, str, str]


@dataclass(frozen=True, slots=True)
class FrozenRuntimeRecord:
    prompts: tuple[tuple[str, str], ...]
    skills: tuple[tuple[str, str], ...]
    model_requirements: tuple[tuple[object, ...], ...]
    binding: tuple[object, ...]
    runtime_target: str
    components: tuple[tuple[object, ...], ...]
    capabilities: tuple[FrozenCapabilityRecord, ...]
    required_features: tuple[str, ...]
    optional_features: tuple[str, ...]


def construct_fake_runtime(
    inputs: Any,
    *,
    catalog_provider: Any,
) -> FrozenRuntimeRecord:
    agent = inputs.resolved_agent
    binding = inputs.runtime_binding
    capabilities = tuple(
        FrozenCapabilityRecord(
            identity=(requirement.name, requirement.version),
            required=requirement.required,
            resources=requirement.resources,
            summary=requirement.descriptor.summary,
            input_schema=requirement.descriptor.input_schema,
            output_schema=requirement.descriptor.output_schema,
            stable_errors=tuple(
                (error.code, error.category, error.retry, error.message)
                for error in requirement.descriptor.stable_errors
            ),
            execution_modes=requirement.descriptor.execution_modes,
            safety=(
                requirement.descriptor.resource_selector_schema,
                requirement.descriptor.effect,
                requirement.descriptor.idempotency,
                requirement.descriptor.freshness,
                requirement.descriptor.preconditions,
                requirement.descriptor.confirmation,
                requirement.descriptor.approval,
                requirement.descriptor.consent,
            ),
            digests=(
                requirement.descriptor_digest,
                requirement.input_schema_digest,
                requirement.output_schema_digest,
                requirement.stable_error_set_digest,
                requirement.safety_metadata_digest,
            ),
        )
        for requirement in agent.capability_requirements
    )
    return FrozenRuntimeRecord(
        prompts=tuple((asset.path, asset.text) for asset in agent.prompts),
        skills=tuple((asset.path, asset.text) for asset in agent.skills),
        model_requirements=tuple(
            (
                requirement.role,
                requirement.symbol,
                requirement.capabilities,
                requirement.min_context_tokens,
                requirement.max_latency_class,
                requirement.residency,
                requirement.required,
            )
            for requirement in agent.model_requirements
        ),
        binding=(
            binding.runtime,
            binding.model_symbols,
            binding.middleware_symbols,
            binding.backend,
            binding.checkpointer,
            binding.capability_provider,
            binding.audit_sink,
        ),
        runtime_target=inputs.runtime_target,
        components=tuple(
            (
                component.symbol,
                component.kind,
                component.features,
                component.durable,
                component.model_tool_calling,
                component.model_structured_output,
                component.model_max_context_tokens,
                component.model_residency,
                component.model_latency_class,
                component.model_modalities,
            )
            for component in inputs.target_components
        ),
        capabilities=capabilities,
        required_features=agent.required_features,
        optional_features=agent.optional_features,
    )


def test_wave4_adapter_constructs_only_from_frozen_runtime_inputs(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    workspace = Path(__file__).resolve().parents[3]
    package_source = workspace / "tests/fixtures/packages/support-agent"
    target_source = workspace / "tests/fixtures/components/deepagents-test.json"
    package = tmp_path / "support-agent"
    target = tmp_path / "deepagents-test.json"
    shutil.copytree(package_source, package)
    shutil.copy2(target_source, target)

    inputs = resolve_package(package, package / "bindings/deepagents.yaml", target)
    shutil.rmtree(package)
    target.unlink()

    def forbidden(*_args: object, **_kwargs: object) -> None:
        raise AssertionError("Wave 4 adapter attempted a source or catalog reread")

    monkeypatch.setattr(builtins, "open", forbidden)
    monkeypatch.setattr(Path, "read_bytes", forbidden)
    monkeypatch.setattr(Path, "read_text", forbidden)
    monkeypatch.setattr(json, "loads", forbidden)
    monkeypatch.setattr(kiteframe, "load_resolved_agent", forbidden)
    yaml_module = sys.modules.get("yaml")
    if yaml_module is not None:
        for name in ("load", "safe_load"):
            if hasattr(yaml_module, name):
                monkeypatch.setattr(yaml_module, name, forbidden)

    record = construct_fake_runtime(inputs, catalog_provider=forbidden)

    assert record.prompts == (
        ("prompts/system.md", "Help support agents read cases safely.\n"),
    )
    assert record.skills == ()
    assert record.model_requirements == (
        (
            "primary",
            "models.anthropic.sonnet",
            ("text", "tool-calling"),
            None,
            None,
            None,
            True,
        ),
    )
    assert record.binding == (
        "deepagents",
        (("primary", "models.anthropic.sonnet"),),
        (),
        None,
        None,
        "capability-providers.primary",
        "audit-sinks.ledger",
    )
    assert record.runtime_target == "deepagents"
    model = next(
        component
        for component in record.components
        if component[0] == "models.anthropic.sonnet"
    )
    assert model == (
        "models.anthropic.sonnet",
        "model",
        (),
        False,
        True,
        True,
        200000,
        "global",
        "interactive",
        ("text",),
    )
    capability = record.capabilities[0]
    assert capability.identity == ("cases.read", "1.2.0")
    assert capability.required is True
    assert capability.resources == ("tenant:support",)
    assert capability.summary == "Read a case"
    assert isinstance(capability.input_schema, dict)
    assert isinstance(capability.output_schema, dict)
    assert capability.input_schema["type"] == "object"
    assert capability.output_schema["type"] == "object"
    assert capability.stable_errors == ()
    assert capability.execution_modes == ("immediate",)
    assert capability.safety[1] == "read_only"
    assert all(len(digest) == 64 for digest in capability.digests)
    assert record.required_features == ()
    assert record.optional_features == ()


def test_resolved_agent_loader_is_inspection_only() -> None:
    workspace = Path(__file__).resolve().parents[3]
    resolved = load_resolved_agent(
        (workspace / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )

    assert isinstance(resolved, ResolvedAgent)
    assert not hasattr(resolved, "runtime_binding")
    stub = (workspace / "python/kiteframe/src/kiteframe/_native.pyi").read_text()
    assert "inspection only; use `resolve_package` for compilation inputs" in stub
