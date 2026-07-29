import ast
import os
import subprocess
import sys
from pathlib import Path

import kiteframe


def workspace_root() -> Path:
    return Path(__file__).resolve().parents[3]


def native_stub_path() -> Path:
    return workspace_root() / "python/kiteframe/src/kiteframe/_native.pyi"


def test_generated_native_stub_has_no_drift() -> None:
    environment = dict(os.environ)
    environment["PYO3_PYTHON"] = sys.executable
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "-p",
            "kiteframe-schema",
            "--",
            "--check-python-stubs",
            native_stub_path(),
        ],
        cwd=workspace_root(),
        capture_output=True,
        check=False,
        env=environment,
        text=True,
    )
    assert completed.returncode == 0, completed.stderr


def test_generated_native_stub_has_exactly_one_final_newline() -> None:
    stub = native_stub_path().read_bytes()
    assert stub.endswith(b"\n")
    assert not stub.endswith(b"\n\n")


def test_public_package_exports_service_response_contracts() -> None:
    expected = {
        "CapabilityGrantSet",
        "InvocationOutcome",
        "InvocationStatus",
        "load_capability_grant_set",
        "load_invocation_outcome",
        "load_invocation_status",
    }
    assert expected <= set(kiteframe.__all__)
    assert all(hasattr(kiteframe, name) for name in expected)


def test_rust_owned_stub_classes_are_read_only_and_nonconstructible() -> None:
    module = ast.parse(native_stub_path().read_text())
    classes = {
        node.name: node
        for node in module.body
        if isinstance(node, ast.ClassDef)
    }
    assert "KiteframeDiagnosticError" in classes

    for name in [
        "AdmissionRequest",
        "AuthorityRevision",
        "AuthorityRevisionSet",
        "CapabilityCatalog",
        "CapabilityDenial",
        "CapabilityDescriptor",
        "CapabilityGrantSet",
        "CatalogFetchResult",
        "CatalogRequest",
        "CompilationReport",
        "ComponentDescriptor",
        "Diagnostic",
        "EffectProposal",
        "EffectiveCapabilityGrant",
        "InvocationRequest",
        "InvocationOutcome",
        "InvocationStatus",
        "ResolvedAgent",
        "ResolvedCapabilityRequirement",
        "ResolvedModelRequirement",
        "ResolvedRuntimeInputs",
        "ResolvedSubagent",
        "ResolvedTextAsset",
        "RuntimeBinding",
        "RuntimeBindingContentCapture",
        "StableCapabilityError",
        "StatusRequest",
        "Suspension",
    ]:
        methods = {
            node.name: node
            for node in classes[name].body
            if isinstance(node, ast.FunctionDef)
        }
        assert "__init__" not in methods
        assert "__new__" not in methods
        if name == "CatalogRequest":
            assert any(
                isinstance(decorator, ast.Name)
                and decorator.id == "staticmethod"
                for decorator in methods["default"].decorator_list
            )
        assert all(
            any(
                isinstance(decorator, ast.Name)
                and decorator.id in {"property", "staticmethod"}
                for decorator in method.decorator_list
            )
            for method_name, method in methods.items()
            if method_name not in {"canonical_json", "default"}
        )
