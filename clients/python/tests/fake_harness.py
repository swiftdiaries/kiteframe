from __future__ import annotations

import json
import os
import sys
from pathlib import Path

VERSION = os.environ.get("FAKE_VERSION", "0.1.0")


def emit(message: object) -> None:
    print(json.dumps(message, separators=(",", ":")), flush=True)


if sys.argv[1:] == ["--version"]:
    print(f"kiteframe-harness {VERSION}")
    raise SystemExit(0)

if diagnostic := os.environ.get("FAKE_STARTUP_FAILURE"):
    print(diagnostic, file=sys.stderr, flush=True)
    raise SystemExit(7)

log_path = Path(os.environ["FAKE_LOG"])
closed_path = Path(os.environ["FAKE_CLOSED"])
session_id = "019ff400-0000-7000-8000-000000000001"
run_id = "019ff400-0000-7000-8000-000000000002"
definition_id = "019ff400-0000-7000-8000-000000000003"

try:
    for line in sys.stdin:
        message = json.loads(line)
        with log_path.open("a", encoding="utf-8") as log:
            log.write(json.dumps(message, separators=(",", ":")) + "\n")
        if "method" not in message:
            continue

        request_id = message["id"]
        method = message["method"]
        params = message.get("params", {})
        if method == "initialize":
            version = params["protocolVersion"]
            result = {
                "protocolVersion": version,
                "_meta": params.get("_meta"),
            }
            if version == 1:
                result.update(
                    {
                        "agentCapabilities": {"loadSession": True},
                        "agentInfo": {"name": "kiteframe", "version": VERSION},
                    }
                )
            else:
                result.update(
                    {
                        "capabilities": {"session": {}},
                        "info": {"name": "kiteframe", "version": VERSION},
                    }
                )
        elif method == "session/new":
            result = {"sessionId": session_id}
        elif method == "_kiteframe/session/policy/update":
            result = {"revision": 1}
        elif method == "_kiteframe/workflow/publish":
            result = {
                "id": definition_id,
                "name": "demo",
                "version": 1,
                "definitionHash": "sha256:definition",
            }
        elif method == "_kiteframe/run/start":
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": "permission-1",
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": session_id,
                        "toolCall": {"toolCallId": "call-1", "status": "pending"},
                    "options": [
                        {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                        {"optionId": "allow-session", "name": "Allow session", "kind": "allow_always"},
                        {"optionId": "deny-once", "name": "Deny once", "kind": "reject_once"},
                        {"optionId": "deny-session", "name": "Deny session", "kind": "reject_always"},
                    ],
                    },
                }
            )
            result = {
                "id": run_id,
                "sessionId": session_id,
                "workflowName": params["workflowName"],
                "version": 1,
                "state": "waiting_for_permission",
                "revision": 2,
            }
        elif method == "_kiteframe/run/get":
            result = {
                "id": run_id,
                "sessionId": session_id,
                "workflowName": "demo",
                "version": 1,
                "state": "completed",
                "revision": 4,
            }
        elif method == "_kiteframe/run/event":
            after = params["after"]
            sequences = [1, 2] if after == 0 else [3]
            result = {
                "events": [
                    {
                        "sequence": sequence,
                        "kind": f"run.event.{sequence}",
                        "metadata": {"sequence": sequence},
                        "createdAt": "2026-08-13 00:00:00Z",
                    }
                    for sequence in sequences
                ]
            }
        else:
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": "Method not found"},
                }
            )
            continue
        emit({"jsonrpc": "2.0", "id": request_id, "result": result})
finally:
    closed_path.write_text("closed\n", encoding="utf-8")
