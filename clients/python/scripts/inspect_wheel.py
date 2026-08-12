from __future__ import annotations

import argparse
import os
import subprocess
import tempfile
import zipfile
from pathlib import Path

EXPECTED_VERSION = "0.1.0"


def inspect_wheel(wheel: Path) -> None:
    executable = "kiteframe-harness.exe" if os.name == "nt" else "kiteframe-harness"
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
    required = {
        "kiteframe/__init__.py",
        "kiteframe/client.py",
        "kiteframe/models.py",
        "kiteframe/transport.py",
        "kiteframe/binary.py",
        "kiteframe/py.typed",
    }
    missing = required.difference(names)
    if missing:
        raise RuntimeError(f"wheel is missing Python package files: {sorted(missing)}")
    scripts = [name for name in names if name.endswith(f".data/scripts/{executable}")]
    if len(scripts) != 1:
        raise RuntimeError(f"wheel must contain exactly one {executable} script; found {scripts}")
    generated = [name for name in names if "__pycache__" in name or name.endswith(".pyc")]
    if generated:
        raise RuntimeError(f"wheel contains generated Python cache files: {generated}")


def smoke_install(wheel: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="kiteframe-wheel-", suffix="-smoke") as directory:
        project = Path(directory)
        subprocess.run(["uv", "init", "--bare", str(project)], check=True)
        subprocess.run(["uv", "add", "--project", str(project), str(wheel)], check=True)
        subprocess.run(
            [
                "uv",
                "run",
                "--project",
                str(project),
                "python",
                "-c",
                f"import kiteframe; assert kiteframe.__version__ == '{EXPECTED_VERSION}'",
            ],
            check=True,
        )
        completed = subprocess.run(
            ["uv", "run", "--project", str(project), "kiteframe-harness", "--version"],
            check=True,
            capture_output=True,
            text=True,
        )
        expected = f"kiteframe-harness {EXPECTED_VERSION}"
        if completed.stdout.strip() != expected:
            raise RuntimeError(
                f"embedded executable reported {completed.stdout.strip()!r}; expected {expected!r}"
            )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("wheel", type=Path)
    arguments = parser.parse_args()
    wheel = arguments.wheel.resolve()
    inspect_wheel(wheel)
    smoke_install(wheel)


if __name__ == "__main__":
    main()
