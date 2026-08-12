from __future__ import annotations

import asyncio
import inspect
import json
import ssl
from collections.abc import Awaitable, Callable, Mapping
from contextlib import suppress
from typing import Any, Protocol, cast
from urllib.parse import SplitResult, urlsplit, urlunsplit

import httpx
from websockets.asyncio.client import ClientConnection
from websockets.asyncio.client import connect as ws_connect
from websockets.exceptions import InvalidHandshake, InvalidStatus

from .binary import HarnessBinary
from .models import (
    AuthenticationError,
    PermissionChoice,
    PermissionRequest,
    RpcError,
    TransportError,
)

PermissionHandler = Callable[
    [PermissionRequest], PermissionChoice | Awaitable[PermissionChoice]
]
_MAX_MESSAGE_BYTES = 16 * 1024 * 1024
_DIAGNOSTIC_BYTES = 64 * 1024
_UPGRADE_FALLBACK_STATUSES = frozenset({404, 405, 406, 426, 501})


class JsonRpcTransport(Protocol):
    async def start(self) -> None: ...

    async def request(self, method: str, params: dict[str, object]) -> dict[str, Any]: ...

    async def open_session(self, session_id: str) -> None: ...

    async def abort(self) -> None: ...

    async def close(self) -> None: ...


class _JsonRpcTransport:
    def __init__(self, permission_handler: PermissionHandler | None = None) -> None:
        self._permission_handler = permission_handler
        self._next_id = 1
        self._pending: dict[int | str, asyncio.Future[dict[str, Any]]] = {}
        self._incoming: dict[int | str, asyncio.Task[None]] = {}
        self._send_lock = asyncio.Lock()
        self._closed = False
        self._terminal_error: BaseException | None = None

    async def request(self, method: str, params: dict[str, object]) -> dict[str, Any]:
        if self._closed:
            raise TransportError("ACP transport is closed")
        if self._terminal_error is not None:
            raise self._terminal_error
        request_id = self._next_id
        self._next_id += 1
        future = asyncio.get_running_loop().create_future()
        self._pending[request_id] = future
        message = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        try:
            async with self._send_lock:
                await self._send(message)
            return await future
        except BaseException:
            self._pending.pop(request_id, None)
            if future.done() and not future.cancelled():
                future.exception()
            elif not future.done():
                future.cancel()
            raise

    async def open_session(self, session_id: str) -> None:
        del session_id

    async def _send(self, message: Mapping[str, object]) -> None:
        raise NotImplementedError

    async def _dispatch_payload(self, payload: str | bytes) -> None:
        if isinstance(payload, bytes):
            if len(payload) > _MAX_MESSAGE_BYTES:
                raise TransportError("ACP message exceeds the 16 MiB client limit")
            payload = payload.decode("utf-8")
        elif len(payload.encode("utf-8")) > _MAX_MESSAGE_BYTES:
            raise TransportError("ACP message exceeds the 16 MiB client limit")
        try:
            parsed = json.loads(payload)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise TransportError("harness returned malformed JSON-RPC") from error
        messages = parsed if isinstance(parsed, list) else [parsed]
        for message in messages:
            if not isinstance(message, dict):
                raise TransportError("harness returned a non-object JSON-RPC message")
            await self._dispatch_message(message)

    async def _dispatch_message(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        if isinstance(method, str):
            if method == "$/cancel_request":
                params = message.get("params", {})
                request_id = params.get("requestId") if isinstance(params, Mapping) else None
                task = self._incoming.pop(cast(int | str, request_id), None)
                if task is not None:
                    task.cancel()
                return
            if "id" not in message:
                return
            request_id = cast(int | str, message["id"])
            task = asyncio.create_task(self._handle_server_request(request_id, method, message))
            self._incoming[request_id] = task
            task.add_done_callback(lambda _: self._incoming.pop(request_id, None))
            return

        request_id = message.get("id")
        if not isinstance(request_id, (int, str)):
            return
        future = self._pending.pop(request_id, None)
        if future is None or future.done():
            return
        error = message.get("error")
        if isinstance(error, Mapping):
            future.set_exception(
                RpcError(
                    int(error.get("code", -32603)),
                    str(error.get("message", "JSON-RPC request failed")),
                    error.get("data"),
                )
            )
            return
        result = message.get("result")
        if not isinstance(result, dict):
            future.set_exception(TransportError("JSON-RPC result must be an object"))
            return
        future.set_result(result)

    async def _handle_server_request(
        self, request_id: int | str, method: str, message: Mapping[str, Any]
    ) -> None:
        try:
            await self._respond_to_server_request(request_id, method, message)
        except asyncio.CancelledError:
            raise
        except Exception as error:  # noqa: BLE001 - callback failures terminate the transport
            await self._fail(error)

    async def _respond_to_server_request(
        self, request_id: int | str, method: str, message: Mapping[str, Any]
    ) -> None:
        if method != "session/request_permission":
            response: dict[str, object] = {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": "Method not found"},
            }
        else:
            params = message.get("params")
            try:
                if not isinstance(params, Mapping):
                    raise TransportError("permission request params must be an object")
                permission = PermissionRequest.from_json(params)
                choice = await self._choose_permission(permission)
                offered = {option.option_id for option in permission.options}
                if choice not in offered:
                    raise TransportError(f"permission handler selected an unoffered choice: {choice}")
                outcome: dict[str, object] = {
                    "outcome": {"outcome": "selected", "optionId": choice}
                }
            except Exception:  # noqa: BLE001 - a permission callback must fail closed
                outcome = {"outcome": {"outcome": "cancelled"}}
            response = {"jsonrpc": "2.0", "id": request_id, "result": outcome}
        async with self._send_lock:
            await self._send(response)

    async def _choose_permission(self, permission: PermissionRequest) -> PermissionChoice:
        if self._permission_handler is None:
            offered = {option.option_id for option in permission.options}
            if "deny-once" in offered:
                return "deny-once"
            raise TransportError("permission request has no deny-once choice")
        selected = self._permission_handler(permission)
        if inspect.isawaitable(selected):
            selected = await selected
        return cast(PermissionChoice, selected)

    async def _fail(self, error: BaseException) -> None:
        if self._terminal_error is None:
            self._terminal_error = error
        for future in self._pending.values():
            if not future.done():
                future.set_exception(error)
        self._pending.clear()

    async def _finish_close(self) -> None:
        self._closed = True
        for task in self._incoming.values():
            task.cancel()
        if self._incoming:
            await asyncio.gather(*self._incoming.values(), return_exceptions=True)
        self._incoming.clear()
        await self._fail(TransportError("ACP transport closed"))


class StdioTransport(_JsonRpcTransport):
    def __init__(
        self,
        binary: HarnessBinary,
        environment: Mapping[str, str],
        permission_handler: PermissionHandler | None = None,
    ) -> None:
        super().__init__(permission_handler)
        self._binary = binary
        self._environment = dict(environment)
        self._process: asyncio.subprocess.Process | None = None
        self._reader_task: asyncio.Task[None] | None = None
        self._stderr_task: asyncio.Task[None] | None = None
        self._stderr = bytearray()

    async def start(self) -> None:
        try:
            self._process = await asyncio.create_subprocess_exec(
                *self._binary.command,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                env=self._environment,
                limit=_MAX_MESSAGE_BYTES,
            )
        except OSError as error:
            raise TransportError(f"failed to start kiteframe-harness: {error}") from error
        self._stderr_task = asyncio.create_task(self._read_stderr())
        self._reader_task = asyncio.create_task(self._read_stdout())

    async def _read_stderr(self) -> None:
        assert self._process is not None and self._process.stderr is not None
        while chunk := await self._process.stderr.read(4096):
            remaining = _DIAGNOSTIC_BYTES - len(self._stderr)
            if remaining > 0:
                self._stderr.extend(chunk[:remaining])

    async def _read_stdout(self) -> None:
        assert self._process is not None and self._process.stdout is not None
        try:
            while line := await self._process.stdout.readline():
                await self._dispatch_payload(line)
            returncode = await self._process.wait()
            if self._stderr_task is not None:
                await self._stderr_task
            if not self._closed:
                diagnostic = self._stderr.decode("utf-8", errors="replace").strip()
                detail = f": {diagnostic}" if diagnostic else ""
                await self._fail(
                    TransportError(f"kiteframe-harness exited with status {returncode}{detail}")
                )
        except asyncio.CancelledError:
            raise
        except Exception as error:  # noqa: BLE001 - propagate reader failures to every waiter
            await self._fail(error)

    async def _send(self, message: Mapping[str, object]) -> None:
        if self._process is None or self._process.stdin is None:
            raise TransportError("kiteframe-harness stdin is unavailable")
        if self._process.returncode is not None:
            diagnostic = self._stderr.decode("utf-8", errors="replace").strip()
            raise TransportError(f"kiteframe-harness is not running: {diagnostic}")
        self._process.stdin.write(
            json.dumps(message, separators=(",", ":"), allow_nan=False).encode("utf-8") + b"\n"
        )
        await self._process.stdin.drain()

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        process = self._process
        if process is not None and process.returncode is None:
            if process.stdin is not None:
                process.stdin.close()
                with suppress(BrokenPipeError, ConnectionResetError):
                    await process.stdin.wait_closed()
            try:
                await asyncio.wait_for(process.wait(), timeout=5)
            except TimeoutError:
                process.terminate()
                try:
                    await asyncio.wait_for(process.wait(), timeout=2)
                except TimeoutError:
                    process.kill()
                    await process.wait()
        if self._reader_task is not None:
            with suppress(asyncio.CancelledError):
                await self._reader_task
        if self._stderr_task is not None:
            with suppress(asyncio.CancelledError):
                await self._stderr_task
        await self._finish_close()

    async def abort(self) -> None:
        if self._closed:
            return
        self._closed = True
        process = self._process
        if process is not None and process.returncode is None:
            process.kill()
            await process.wait()
        for task in (self._reader_task, self._stderr_task):
            if task is not None and not task.done():
                task.cancel()
        tasks = [task for task in (self._reader_task, self._stderr_task) if task is not None]
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        await self._finish_close()


class WebSocketTransport(_JsonRpcTransport):
    def __init__(
        self, url: str, token: str, permission_handler: PermissionHandler | None = None
    ) -> None:
        super().__init__(permission_handler)
        self._url = _acp_endpoint(url, websocket=True)
        self._token = token
        self._socket: ClientConnection | None = None
        self._reader_task: asyncio.Task[None] | None = None

    async def start(self) -> None:
        self._socket = await ws_connect(
            self._url,
            additional_headers={"Authorization": f"Bearer {self._token}"},
            open_timeout=10,
            close_timeout=1,
            max_size=_MAX_MESSAGE_BYTES,
        )
        self._reader_task = asyncio.create_task(self._read())

    async def _read(self) -> None:
        assert self._socket is not None
        try:
            async for payload in self._socket:
                await self._dispatch_payload(payload)
            if not self._closed:
                await self._fail(TransportError("WebSocket ACP transport closed"))
        except asyncio.CancelledError:
            raise
        except Exception as error:  # noqa: BLE001 - propagate socket failures to every waiter
            if not self._closed:
                await self._fail(error)

    async def _send(self, message: Mapping[str, object]) -> None:
        if self._socket is None:
            raise TransportError("WebSocket ACP transport isn't connected")
        await self._socket.send(json.dumps(message, separators=(",", ":"), allow_nan=False))

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._socket is not None:
            await self._socket.close()
        if self._reader_task is not None:
            with suppress(asyncio.CancelledError):
                await self._reader_task
        await self._finish_close()

    async def abort(self) -> None:
        await self.close()


class HttpTransport(_JsonRpcTransport):
    def __init__(
        self, url: str, token: str, permission_handler: PermissionHandler | None = None
    ) -> None:
        super().__init__(permission_handler)
        self._url = _acp_endpoint(url, websocket=False)
        self._token = token
        self._client: httpx.AsyncClient | None = None
        self._connection_id: str | None = None
        self._streams: dict[str | None, asyncio.Task[None]] = {}

    async def start(self) -> None:
        self._client = httpx.AsyncClient(
            headers={"Authorization": f"Bearer {self._token}"},
            timeout=httpx.Timeout(30, connect=10),
        )

    async def _send(self, message: Mapping[str, object]) -> None:
        if self._client is None:
            raise TransportError("Streamable HTTP transport isn't started")
        if self._connection_id is None:
            if message.get("method") != "initialize":
                raise TransportError("first Streamable HTTP request must initialize ACP")
            await self._initialize(message)
            return

        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "Acp-Connection-Id": self._connection_id,
        }
        params = message.get("params")
        if isinstance(params, Mapping) and isinstance(params.get("sessionId"), str):
            headers["Acp-Session-Id"] = cast(str, params["sessionId"])
        try:
            async with self._client.stream(
                "POST", self._url, headers=headers, json=dict(message)
            ) as response:
                _raise_http_status(response)
                body = await _read_limited_body(response)
                if body.strip():
                    await self._dispatch_payload(body)
        except TransportError as error:
            await self._fail(error)
            raise

    async def _initialize(self, message: Mapping[str, object]) -> None:
        assert self._client is not None
        try:
            async with self._client.stream(
                "POST",
                self._url,
                headers={"Accept": "application/json", "Content-Type": "application/json"},
                json=dict(message),
            ) as response:
                _raise_http_status(response)
                connection_id = response.headers.get("Acp-Connection-Id")
                body = await _read_limited_body(response)
        except TransportError as error:
            await self._fail(error)
            raise
        if not body.strip():
            raise TransportError("Streamable HTTP initialize returned an empty response")
        try:
            initialize_response = json.loads(body)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise TransportError("Streamable HTTP initialize returned malformed JSON") from error
        await self._dispatch_payload(body)
        if isinstance(initialize_response, dict) and "error" in initialize_response:
            return
        if connection_id is None:
            raise TransportError("Streamable HTTP initialize omitted Acp-Connection-Id")
        self._connection_id = connection_id
        await self._start_stream(None)

    async def open_session(self, session_id: str) -> None:
        await self._start_stream(session_id)

    async def _start_stream(self, session_id: str | None) -> None:
        if session_id in self._streams:
            return
        ready = asyncio.Event()
        failures: list[BaseException] = []
        task = asyncio.create_task(self._read_sse(session_id, ready, failures))
        self._streams[session_id] = task
        await asyncio.wait_for(ready.wait(), timeout=10)
        if failures:
            self._streams.pop(session_id, None)
            raise failures[0]

    async def _read_sse(
        self, session_id: str | None, ready: asyncio.Event, failures: list[BaseException]
    ) -> None:
        assert self._client is not None and self._connection_id is not None
        headers = {
            "Accept": "text/event-stream",
            "Acp-Connection-Id": self._connection_id,
        }
        if session_id is not None:
            headers["Acp-Session-Id"] = session_id
        line_buffer = bytearray()
        data = bytearray()

        async def process_line(line: bytes) -> None:
            if line.endswith(b"\r"):
                line = line[:-1]
            if not line:
                if data:
                    await self._dispatch_payload(bytes(data))
                    data.clear()
                return
            if not line.startswith(b"data:"):
                return
            value = line[5:]
            if value.startswith(b" "):
                value = value[1:]
            additional = len(value) + (1 if data else 0)
            if len(data) + additional > _MAX_MESSAGE_BYTES:
                raise TransportError("ACP message exceeds the 16 MiB client limit")
            if data:
                data.extend(b"\n")
            data.extend(value)

        try:
            async with self._client.stream("GET", self._url, headers=headers) as response:
                _raise_http_status(response)
                ready.set()
                async for chunk in response.aiter_bytes():
                    offset = 0
                    while newline := chunk.find(b"\n", offset) + 1:
                        _append_limited(line_buffer, chunk[offset : newline - 1])
                        await process_line(bytes(line_buffer))
                        line_buffer.clear()
                        offset = newline
                    _append_limited(line_buffer, chunk[offset:])
                if line_buffer:
                    await process_line(bytes(line_buffer))
                if data:
                    await self._dispatch_payload(bytes(data))
            if not self._closed:
                await self._fail(TransportError("Streamable HTTP event stream closed"))
        except asyncio.CancelledError:
            raise
        except Exception as error:  # noqa: BLE001 - surface all event-stream failures
            if not ready.is_set():
                failures.append(error)
                ready.set()
            if not self._closed:
                await self._fail(error)

    async def close(self) -> None:
        await self._shutdown(send_delete=True)

    async def abort(self) -> None:
        await self._shutdown(send_delete=False)

    async def _shutdown(self, *, send_delete: bool) -> None:
        if self._closed:
            return
        self._closed = True
        if send_delete and self._client is not None and self._connection_id is not None:
            with suppress(httpx.HTTPError):
                async with self._client.stream(
                    "DELETE",
                    self._url,
                    headers={"Acp-Connection-Id": self._connection_id},
                ):
                    pass
        for task in self._streams.values():
            task.cancel()
        if self._streams:
            await asyncio.gather(*self._streams.values(), return_exceptions=True)
        self._streams.clear()
        if self._client is not None:
            await self._client.aclose()
        await self._finish_close()


async def _read_limited_body(response: httpx.Response) -> bytes:
    body = bytearray()
    async for chunk in response.aiter_bytes():
        _append_limited(body, chunk)
    return bytes(body)


def _append_limited(buffer: bytearray, chunk: bytes) -> None:
    if len(buffer) + len(chunk) > _MAX_MESSAGE_BYTES:
        raise TransportError("ACP message exceeds the 16 MiB client limit")
    buffer.extend(chunk)


def _raise_http_status(response: httpx.Response) -> None:
    if response.status_code in (401, 403):
        raise AuthenticationError(
            response.status_code, f"Kiteframe authentication failed with HTTP {response.status_code}"
        )
    response.raise_for_status()


async def open_remote_transport(
    url: str,
    *,
    token: str,
    permission_handler: PermissionHandler | None = None,
) -> JsonRpcTransport:
    websocket = WebSocketTransport(url, token, permission_handler)
    try:
        await websocket.start()
        return websocket
    except InvalidStatus as error:
        status_code = error.response.status_code
        if status_code in (401, 403):
            raise AuthenticationError(
                status_code, f"Kiteframe authentication failed with HTTP {status_code}"
            ) from error
        if status_code not in _UPGRADE_FALLBACK_STATUSES:
            raise
    except (ssl.SSLError, ssl.CertificateError):
        raise
    except (OSError, TimeoutError):
        pass
    except InvalidHandshake:
        pass

    fallback = HttpTransport(url, token, permission_handler)
    await fallback.start()
    return fallback


def _acp_endpoint(url: str, *, websocket: bool) -> str:
    parsed = urlsplit(url)
    if parsed.scheme not in {"http", "https", "ws", "wss"} or not parsed.netloc:
        raise ValueError("Kiteframe URL must be an absolute HTTP or WebSocket URL")
    if websocket:
        scheme = "wss" if parsed.scheme in {"https", "wss"} else "ws"
    else:
        scheme = "https" if parsed.scheme in {"https", "wss"} else "http"
    path = parsed.path.rstrip("/")
    if not path.endswith("/acp"):
        path = f"{path}/acp" if path else "/acp"
    endpoint = SplitResult(scheme, parsed.netloc, path, parsed.query, "")
    return urlunsplit(endpoint)
