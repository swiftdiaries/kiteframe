from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path

import pytest

from kiteframe.binary import HarnessBinary, validate_harness_version
from kiteframe.models import VersionMismatchError

FAKE_HARNESS = Path(__file__).with_name("fake_harness.py")


@pytest.mark.asyncio
async def test_binary_version_is_checked_before_the_harness_starts(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    log = tmp_path / "requests.ndjson"
    monkeypatch.setenv("FAKE_VERSION", "9.9.9")
    monkeypatch.setenv("FAKE_LOG", str(log))
    binary = HarnessBinary((sys.executable, str(FAKE_HARNESS)))

    with pytest.raises(VersionMismatchError, match="client 0.1.0.*harness 9.9.9"):
        await validate_harness_version(binary, "0.1.0")

    assert not log.exists(), "version mismatch must fail before ACP initialization"


@pytest.mark.asyncio
async def test_binary_version_failure_includes_bounded_startup_diagnostics(tmp_path: Path) -> None:
    script = tmp_path / "broken_version.py"
    script.write_text(
        "import sys\nprint('cannot inspect binary', file=sys.stderr)\nraise SystemExit(4)\n",
        encoding="utf-8",
    )

    with pytest.raises(RuntimeError, match="cannot inspect binary"):
        await validate_harness_version(HarnessBinary((sys.executable, str(script))), "0.1.0")


@pytest.mark.asyncio
async def test_cancelling_version_probe_reaps_the_child(tmp_path: Path) -> None:
    pid_path = tmp_path / "version.pid"
    script = tmp_path / "hung_version.py"
    script.write_text(
        "import os, pathlib, time\n"
        f"pathlib.Path({str(pid_path)!r}).write_text(str(os.getpid()))\n"
        "time.sleep(60)\n",
        encoding="utf-8",
    )
    probe = asyncio.create_task(
        validate_harness_version(
            HarnessBinary((sys.executable, str(script))), "0.1.0", timeout=60
        )
    )
    for _ in range(100):
        if pid_path.exists():
            break
        await asyncio.sleep(0.01)
    probe.cancel()

    with pytest.raises(asyncio.CancelledError):
        await probe

    pid = int(pid_path.read_text(encoding="utf-8"))
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)
