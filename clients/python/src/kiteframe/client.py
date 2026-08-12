from __future__ import annotations

import json
import subprocess
from collections.abc import Sequence
from typing import Any
from uuid import UUID

from .binary import harness_path
from .models import HarnessError, HarnessResult


class KiteframeClient:
    """A synchronous NDJSON client for a locally bundled Kiteframe harness."""

    def __init__(self, session_id: UUID, *, executable: str | Sequence[str] | None = None) -> None:
        self._session_id = session_id
        command = [executable or harness_path()] if isinstance(executable, str) or executable is None else list(executable)
        self._process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        try:
            self.call("initialize", {})
        except Exception:
            self.close()
            raise

    def close(self) -> None:
        if self._process.poll() is None:
            self._process.terminate()
            self._process.wait(timeout=5)
        for stream in (self._process.stdin, self._process.stdout, self._process.stderr):
            if stream is not None:
                stream.close()

    def __enter__(self) -> "KiteframeClient":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def call(self, method: str, payload: dict[str, Any]) -> HarnessResult:
        if self._process.stdin is None or self._process.stdout is None:
            raise HarnessError("harness pipes are unavailable")
        self._process.stdin.write(json.dumps({"session_id": str(self._session_id), "method": method, "payload": payload}) + "\n")
        self._process.stdin.flush()
        line = self._process.stdout.readline()
        if not line:
            stderr = self._process.stderr.read() if self._process.stderr else ""
            raise HarnessError(f"harness exited without a response: {stderr.strip()}")
        response = json.loads(line)
        if not response.get("ok"):
            raise HarnessError(str(response.get("error", "unknown harness error")))
        result = response["result"]
        return HarnessResult(kind=result["kind"], value=result)
