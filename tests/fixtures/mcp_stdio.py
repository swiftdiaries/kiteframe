import json
import os
import sys


def reply(identifier, result):
    print(json.dumps({"jsonrpc": "2.0", "id": identifier, "result": result}), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    identifier = message.get("id")
    if identifier is None:
        continue
    method = message["method"]
    if method == "initialize":
        reply(identifier, {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kiteframe-e2e-mcp", "version": "1"},
        })
    elif method == "tools/list":
        reply(identifier, {"tools": [{
            "name": "search",
            "description": "deterministic e2e search",
            "inputSchema": {"type": "object", "properties": {}, "required": [], "additionalProperties": False},
            "outputSchema": {"type": "object", "properties": {}, "required": [], "additionalProperties": False},
        }]})
    elif method == "tools/call":
        with open(os.environ["KITEFRAME_E2E_MCP_LOG"], "a", encoding="utf-8") as log:
            log.write(method + "\n")
        reply(identifier, {"content": [{"type": "text", "text": "ok"}], "structuredContent": {}})
    else:
        reply(identifier, {})
