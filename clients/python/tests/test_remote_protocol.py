from __future__ import annotations

import asyncio
import json
from collections.abc import Mapping
from typing import Any

import pytest
from websockets.asyncio.server import ServerConnection, serve

import kiteframe.transport as transport_module
from kiteframe import Kiteframe, PermissionChoice, PermissionRequest, SessionPolicy

SESSION_ID = "019ff400-0000-7000-8000-000000000010"


def empty_policy() -> SessionPolicy:
    return SessionPolicy(version=1, rules=())


def initialize_result(request: Mapping[str, Any]) -> dict[str, object]:
    params = request["params"]
    assert isinstance(params, Mapping)
    return {
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "agentInfo": {"name": "kiteframe", "version": "0.1.0"},
            "_meta": params["_meta"],
        },
    }


def permission_request() -> dict[str, object]:
    return {
        "jsonrpc": "2.0",
        "id": "permission-remote",
        "method": "session/request_permission",
        "params": {
            "sessionId": SESSION_ID,
            "options": [
                {"optionId": "allow-session", "name": "Allow", "kind": "allow_always"},
                {"optionId": "deny-once", "name": "Deny", "kind": "reject_once"},
            ],
        },
    }


@pytest.mark.asyncio
async def test_websocket_transport_uses_authorization_and_handles_permission_callback() -> None:
    observed: list[dict[str, Any]] = []
    callback_seen = asyncio.Event()

    async def handler(socket: ServerConnection) -> None:
        assert socket.request is not None
        assert socket.request.path == "/base/acp"
        assert socket.request.headers["Authorization"] == "Bearer websocket-secret"
        pending_policy_id: object | None = None
        async for payload in socket:
            assert isinstance(payload, str)
            message = json.loads(payload)
            observed.append(message)
            method = message.get("method")
            if method == "initialize":
                response = initialize_result(message)
            elif method == "session/new":
                response = {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {"sessionId": SESSION_ID},
                }
            elif method == "_kiteframe/session/policy/update":
                pending_policy_id = message["id"]
                await socket.send(json.dumps(permission_request()))
                continue
            elif message.get("id") == "permission-remote":
                callback_seen.set()
                assert pending_policy_id is not None
                response = {
                    "jsonrpc": "2.0",
                    "id": pending_policy_id,
                    "result": {"revision": 1},
                }
            else:
                raise AssertionError(f"unexpected WebSocket message: {message}")
            await socket.send(json.dumps(response))

    async with serve(handler, "127.0.0.1", 0) as server:
        port = server.sockets[0].getsockname()[1]

        def choose(_request: PermissionRequest) -> PermissionChoice:
            return "allow-session"

        client = await Kiteframe.connect(
            f"http://127.0.0.1:{port}/base",
            token="websocket-secret",
            policy=empty_policy(),
            permission_handler=choose,
        )
        await callback_seen.wait()
        await client.close()

    callback = next(message for message in observed if message.get("id") == "permission-remote")
    assert callback["result"] == {
        "outcome": {"outcome": "selected", "optionId": "allow-session"}
    }


class StreamableHttpHarness:
    def __init__(self) -> None:
        self.server: asyncio.Server | None = None
        self.stop = asyncio.Event()
        self.streams: dict[str | None, asyncio.StreamWriter] = {}
        self.requests: list[tuple[str, dict[str, str], dict[str, Any] | None]] = []
        self.permission_response: dict[str, Any] | None = None
        self.policy_request_id: object | None = None

    async def start(self) -> int:
        self.server = await asyncio.start_server(self.handle, "127.0.0.1", 0)
        return self.server.sockets[0].getsockname()[1]

    async def close(self) -> None:
        self.stop.set()
        if self.server is not None:
            self.server.close()
            await self.server.wait_closed()

    async def handle(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        streaming = False
        stream_key: str | None = None
        try:
            head = await reader.readuntil(b"\r\n\r\n")
            lines = head.decode("ascii").split("\r\n")
            method, path, _version = lines[0].split(" ", 2)
            headers = {
                name.lower(): value.strip()
                for line in lines[1:]
                if line
                for name, value in [line.split(":", 1)]
            }
            body = await reader.readexactly(int(headers.get("content-length", "0")))
            message = json.loads(body) if body else None
            self.requests.append((method, headers, message))
            assert path == "/base/acp"
            assert headers["authorization"] == "Bearer http-secret"

            if method == "GET":
                streaming = True
                stream_key = headers.get("acp-session-id")
                self.streams[stream_key] = writer
                await self.write_head(writer, 200, {"Content-Type": "text/event-stream"})
                await self.stop.wait()
                return
            if method == "DELETE":
                assert headers["acp-connection-id"] == "connection-1"
                await self.write_response(writer, 200)
                self.stop.set()
                return
            assert message is not None
            rpc_method = message.get("method")
            if rpc_method == "initialize":
                await self.write_json(
                    writer,
                    initialize_result(message),
                    extra_headers={"Acp-Connection-Id": "connection-1"},
                )
            elif rpc_method == "session/new":
                await self.write_json(
                    writer,
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {"sessionId": SESSION_ID},
                    },
                )
            elif rpc_method == "_kiteframe/session/policy/update":
                assert "acp-session-id" not in headers
                self.policy_request_id = message["id"]
                await self.write_response(writer, 202)
                await self.send_sse(SESSION_ID, permission_request())
            elif message.get("id") == "permission-remote":
                assert "acp-session-id" not in headers
                self.permission_response = message
                await self.write_response(writer, 202)
                assert self.policy_request_id is not None
                await self.send_sse(
                    None,
                    {
                        "jsonrpc": "2.0",
                        "id": self.policy_request_id,
                        "result": {"revision": 1},
                    },
                )
            elif rpc_method == "_kiteframe/workflow/publish":
                assert "acp-session-id" not in headers
                await self.write_json(
                    writer,
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {
                            "id": "019ff400-0000-7000-8000-000000000011",
                            "name": "demo",
                            "version": 1,
                            "definitionHash": "hash",
                        },
                    },
                )
            else:
                raise AssertionError(f"unexpected HTTP message: {message}")
        finally:
            if streaming and self.streams.get(stream_key) is writer:
                self.streams.pop(stream_key, None)
            writer.close()
            await writer.wait_closed()

    async def send_sse(self, session_id: str | None, message: Mapping[str, object]) -> None:
        for _ in range(100):
            writer = self.streams.get(session_id)
            if writer is not None:
                payload = json.dumps(message, separators=(",", ":")).encode("utf-8")
                writer.write(b"data: " + payload + b"\n\n")
                await writer.drain()
                return
            await asyncio.sleep(0.01)
        raise AssertionError("session event stream did not open")

    async def write_json(
        self,
        writer: asyncio.StreamWriter,
        value: Mapping[str, object],
        *,
        extra_headers: Mapping[str, str] | None = None,
    ) -> None:
        body = json.dumps(value, separators=(",", ":")).encode("utf-8")
        headers = {"Content-Type": "application/json"}
        if extra_headers is not None:
            headers.update(extra_headers)
        await self.write_response(writer, 200, headers, body)

    async def write_response(
        self,
        writer: asyncio.StreamWriter,
        status: int,
        headers: Mapping[str, str] | None = None,
        body: bytes = b"",
    ) -> None:
        await self.write_head(writer, status, headers, content_length=len(body))
        writer.write(body)
        await writer.drain()

    async def write_head(
        self,
        writer: asyncio.StreamWriter,
        status: int,
        headers: Mapping[str, str] | None = None,
        *,
        content_length: int | None = None,
    ) -> None:
        reason = {200: "OK", 202: "Accepted"}[status]
        lines = [f"HTTP/1.1 {status} {reason}"]
        if headers is not None:
            lines.extend(f"{name}: {value}" for name, value in headers.items())
        if content_length is not None:
            lines.append(f"Content-Length: {content_length}")
        lines.append("Connection: close")
        writer.write(("\r\n".join(lines) + "\r\n\r\n").encode("ascii"))
        await writer.drain()


@pytest.mark.asyncio
async def test_streamable_http_fallback_routes_session_sse_callbacks_and_delete(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    harness = StreamableHttpHarness()
    port = await harness.start()

    async def reject_websocket(*_args: object, **_kwargs: object) -> None:
        raise OSError("upgrade unavailable")

    monkeypatch.setattr(transport_module, "ws_connect", reject_websocket)

    async def choose(_request: PermissionRequest) -> PermissionChoice:
        return "allow-session"

    try:
        client = await Kiteframe.connect(
            f"http://127.0.0.1:{port}/base",
            token="http-secret",
            policy=empty_policy(),
            permission_handler=choose,
        )
        published = await client.publish("workflow demo() -> string { start t() }")
        await client.close()
    finally:
        await harness.close()

    assert published.name == "demo"
    assert harness.permission_response is not None
    assert harness.permission_response["result"] == {
        "outcome": {"outcome": "selected", "optionId": "allow-session"}
    }
    methods = [method for method, _headers, _message in harness.requests]
    assert methods.count("GET") == 2
    assert methods[-1] == "DELETE"
