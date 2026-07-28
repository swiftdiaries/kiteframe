import ast
import subprocess
from pathlib import Path


def workspace_root() -> Path:
    return Path(__file__).resolve().parents[3]


def native_stub_path() -> Path:
    return workspace_root() / "python/kiteframe/src/kiteframe/_native.pyi"


def test_generated_native_stub_has_no_drift() -> None:
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
        text=True,
    )
    assert completed.returncode == 0, completed.stderr


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
        "CapabilityCatalog",
        "CapabilityGrant",
        "CapabilityGrantSet",
        "CatalogRequest",
        "InvocationRequest",
        "InvocationOutcome",
        "InvocationStatus",
        "ResolvedAgent",
        "ResolvedCapabilityRequirement",
        "ResolvedSubagent",
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
                and decorator.id == "property"
                for decorator in method.decorator_list
            )
            for method_name, method in methods.items()
            if method_name not in {"canonical_json", "default"}
        )
