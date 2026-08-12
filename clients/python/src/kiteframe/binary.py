from __future__ import annotations

import asyncio
import importlib.metadata
import os
import re
import shutil
import sysconfig
from dataclasses import dataclass
from pathlib import Path

from .models import BinaryError, VersionMismatchError

CLIENT_VERSION = "0.1.0"
_VERSION_PATTERN = re.compile(r"^kiteframe-harness ([0-9A-Za-z][0-9A-Za-z.+-]*)$")
_DIAGNOSTIC_LIMIT = 4096


@dataclass(frozen=True)
class HarnessBinary:
    command: tuple[str, ...]

    def __post_init__(self) -> None:
        if not self.command:
            raise ValueError("harness command must not be empty")


def package_version() -> str:
    try:
        return importlib.metadata.version("kiteframe")
    except importlib.metadata.PackageNotFoundError:
        return CLIENT_VERSION


def resolve_harness() -> HarnessBinary:
    executable_name = "kiteframe-harness.exe" if os.name == "nt" else "kiteframe-harness"
    scripts_path = Path(sysconfig.get_path("scripts")) / executable_name
    if scripts_path.is_file():
        return HarnessBinary((str(scripts_path),))
    discovered = shutil.which("kiteframe-harness")
    if discovered is not None and Path(discovered).name == executable_name:
        return HarnessBinary((discovered,))
    raise BinaryError(
        f"the {executable_name} executable is absent from this kiteframe installation"
    )


async def validate_harness_version(
    binary: HarnessBinary, expected_version: str, *, timeout: float = 5.0
) -> None:
    try:
        process = await asyncio.create_subprocess_exec(
            *binary.command,
            "--version",
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
    except OSError as error:
        raise BinaryError(f"failed to launch the bundled kiteframe-harness: {error}") from error
    try:
        stdout, stderr = await asyncio.wait_for(process.communicate(), timeout=timeout)
    except TimeoutError as error:
        if process.returncode is None:
            process.kill()
        await process.wait()
        raise BinaryError("kiteframe-harness --version timed out") from error
    except BaseException:
        if process.returncode is None:
            process.kill()
        await process.wait()
        raise

    diagnostic = stderr.decode("utf-8", errors="replace")[:_DIAGNOSTIC_LIMIT].strip()
    if process.returncode != 0:
        detail = f": {diagnostic}" if diagnostic else ""
        raise BinaryError(
            f"kiteframe-harness --version exited with status {process.returncode}{detail}"
        )
    reported = stdout.decode("utf-8", errors="replace").strip()
    matched = _VERSION_PATTERN.fullmatch(reported)
    if matched is None:
        raise BinaryError("kiteframe-harness returned an invalid --version response")
    harness_version = matched.group(1)
    if harness_version != expected_version:
        raise VersionMismatchError(
            f"kiteframe client {expected_version} is incompatible with harness {harness_version}"
        )
