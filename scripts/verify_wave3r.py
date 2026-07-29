"""Run the Rust Wave 3R matrix with the checked-in Python project interpreter."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
MINIMUM_PYTHON = (3, 11)
CARGO_COMMANDS = (
    ("cargo", "fmt", "--all", "--check"),
    (
        "cargo",
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
    ),
    ("cargo", "test", "--workspace", "--all-features"),
    (
        "cargo",
        "run",
        "-p",
        "kiteframe-schema",
        "--",
        "--check",
        "schemas/v1alpha1",
    ),
    (
        "cargo",
        "run",
        "-p",
        "kiteframe-schema",
        "--",
        "--check-python-stubs",
        "python/kiteframe/src/kiteframe/_native.pyi",
    ),
)


def main() -> None:
    if sys.version_info < MINIMUM_PYTHON:
        required = ".".join(str(part) for part in MINIMUM_PYTHON)
        raise SystemExit(f"Wave 3R verification requires Python {required} or newer")

    environment = os.environ.copy()
    environment["PYO3_PYTHON"] = sys.executable
    environment["PYTHONHOME"] = sys.base_prefix
    print(
        "Wave 3R PyO3 interpreter:",
        sys.executable,
        f"({sys.version.split()[0]}, PYTHONHOME={sys.base_prefix})",
        flush=True,
    )
    for command in CARGO_COMMANDS:
        print("+", " ".join(command), flush=True)
        subprocess.run(
            command,
            check=True,
            cwd=REPOSITORY_ROOT,
            env=environment,
        )


if __name__ == "__main__":
    main()
