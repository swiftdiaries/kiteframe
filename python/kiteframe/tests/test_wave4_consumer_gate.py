import shutil
from pathlib import Path

from kiteframe import ResolvedAgent, load_resolved_agent, resolve_package


def test_runtime_inputs_survive_source_fixture_removal(tmp_path: Path) -> None:
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

    assert (
        inputs.resolved_agent.prompts[0].text
        == "Help support agents read cases safely.\n"
    )
    assert inputs.resolved_agent.capability_requirements[0].name == "cases.read"
    assert inputs.runtime_binding.model_symbols == (
        ("primary", "models.anthropic.sonnet"),
    )
    assert inputs.runtime_binding.capability_provider == "capability-providers.primary"
    assert inputs.runtime_target == "deepagents"
    assert any(
        component.symbol == "models.anthropic.sonnet"
        and component.model_max_context_tokens == 200000
        for component in inputs.target_components
    )


def test_nondefault_inputs_survive_source_fixture_removal(tmp_path: Path) -> None:
    workspace = Path(__file__).resolve().parents[3]
    package_source = workspace / "tests/fixtures/packages/support-agent-runtime-inputs"
    target_source = workspace / "tests/fixtures/components/deepagents-test.json"
    package = tmp_path / "support-agent"
    target = tmp_path / "deepagents-test.json"
    shutil.copytree(package_source, package)
    shutil.copy2(target_source, target)
    inputs = resolve_package(package, package / "bindings/deepagents.yaml", target)
    shutil.rmtree(package)
    target.unlink()

    capture = inputs.runtime_binding.content_capture
    assert capture is not None
    assert capture.enabled is True
    assert capture.classifications == ("confidential",)
    assert capture.redaction_policy == "redaction-policies.default"
    assert capture.retention_policy == "retention-policies.default"
    assert capture.access_policy == "access-policies.default"
    assert capture.encrypted_content_store == "content-stores.encrypted"

    model_requirement = inputs.resolved_agent.model_requirements[0]
    assert model_requirement.max_latency_class == "interactive"
    assert model_requirement.residency == "global"
    model_component = next(
        component
        for component in inputs.target_components
        if component.symbol == "models.anthropic.sonnet"
    )
    assert model_component.model_modalities == ("text",)


def test_resolved_agent_loader_is_inspection_only() -> None:
    workspace = Path(__file__).resolve().parents[3]
    resolved = load_resolved_agent(
        (workspace / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )

    assert isinstance(resolved, ResolvedAgent)
    assert not hasattr(resolved, "runtime_binding")
    stub = (workspace / "python/kiteframe/src/kiteframe/_native.pyi").read_text()
    assert "inspection only; use `resolve_package` for compilation inputs" in stub
