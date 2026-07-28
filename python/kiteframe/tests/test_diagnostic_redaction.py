import json
from collections.abc import Callable

import pytest

from kiteframe import KiteframeDiagnosticError, load_resolved_agent
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
