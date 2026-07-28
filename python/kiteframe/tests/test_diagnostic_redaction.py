import json
import shutil
from collections.abc import Callable
from pathlib import Path

import pytest

from kiteframe import KiteframeDiagnosticError, load_resolved_agent, resolve_package
from kiteframe._native import (
    load_capability_catalog,
    load_capability_grant_set,
    load_invocation_outcome,
    load_invocation_status,
)


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def invalid_ir_containing(secret: str) -> bytes:
    return canonical_bytes({"schemaVersion": secret})


@pytest.mark.parametrize(
    "secret",
    [
        "sk-live-secret",
        "raw prompt text",
        "user@example.test",
        "tuple:user:admin",
    ],
)
def test_native_diagnostics_never_expose_sensitive_input(secret: str) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        load_resolved_agent(invalid_ir_containing(secret))

    public = error.value.diagnostics_json
    assert secret.encode() not in public
    assert b"KF-PKG-001" in public
    assert error.value.code == "KF-PKG-001"


@pytest.mark.parametrize(
    "loader",
    [
        load_capability_catalog,
        load_capability_grant_set,
        load_invocation_outcome,
        load_invocation_status,
    ],
)
def test_native_provider_diagnostics_never_expose_invalid_output(
    loader: Callable[[bytes], object],
) -> None:
    secret = "provider-secret-value"

    with pytest.raises(KiteframeDiagnosticError) as error:
        loader(canonical_bytes({"secret": secret}))

    assert error.value.code == "KF-CAP-002"
    assert secret.encode() not in error.value.diagnostics_json
    assert secret not in str(error.value)


def test_resolve_package_diagnostics_redact_missing_component_symbol(
    tmp_path: Path,
) -> None:
    workspace = Path(__file__).resolve().parents[3]
    secret = "sk-live-secret"
    package = tmp_path / "support-agent"
    shutil.copytree(
        workspace / "tests/fixtures/packages/support-agent",
        package,
    )
    binding = package / "bindings/deepagents.yaml"
    binding.write_text(
        binding.read_text(encoding="utf-8").replace(
            "capability-providers.primary",
            secret,
        ),
        encoding="utf-8",
    )

    with pytest.raises(KiteframeDiagnosticError) as error:
        resolve_package(
            package,
            binding,
            workspace / "tests/fixtures/components/deepagents-test.json",
        )

    public = json.loads(error.value.diagnostics_json)
    assert error.value.code == "KF-RUNTIME-001"
    assert public[0]["category"] == "runtime"
    assert secret not in str(error.value)
    assert secret.encode() not in error.value.diagnostics_json


def test_resolve_package_diagnostics_redact_wrong_kind_component_symbol(
    tmp_path: Path,
) -> None:
    workspace = Path(__file__).resolve().parents[3]
    secret = "sk-live-secret"
    package = tmp_path / "support-agent"
    shutil.copytree(
        workspace / "tests/fixtures/packages/support-agent",
        package,
    )
    binding = package / "bindings/deepagents.yaml"
    binding.write_text(
        binding.read_text(encoding="utf-8").replace(
            "capability-providers.primary",
            secret,
        ),
        encoding="utf-8",
    )
    catalog = json.loads(
        (workspace / "tests/fixtures/components/deepagents-test.json").read_text(
            encoding="utf-8"
        )
    )
    catalog["components"][secret] = {"kind": "backend"}
    catalog_path = tmp_path / "components.json"
    catalog_path.write_text(json.dumps(catalog), encoding="utf-8")

    with pytest.raises(KiteframeDiagnosticError) as error:
        resolve_package(
            package,
            binding,
            catalog_path,
        )

    public = json.loads(error.value.diagnostics_json)
    assert error.value.code == "KF-RUNTIME-001"
    assert public[0]["category"] == "runtime"
    assert secret not in str(error.value)
    assert secret.encode() not in error.value.diagnostics_json
