from __future__ import annotations

import asyncio
import gc
import ssl
from collections.abc import AsyncIterator, Mapping
from typing import Any

import pytest
from websockets.datastructures import Headers
from websockets.exceptions import InvalidStatus
from websockets.http11 import Response

import kiteframe.transport as transport_module
from kiteframe import AuthenticationError, TransportError
from kiteframe.transport import _JsonRpcTransport, _read_limited_body, open_remote_transport


def rejected(status_code: int) -> InvalidStatus:
    return InvalidStatus(Response(status_code, "Rejected", Headers()))


class FakeHttpTransport:
    def __init__(self, *_: object, **__: object) -> None:
        self.started = False

    async def start(self) -> None:
        self.started = True


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [OSError("offline"), rejected(426)])
async def test_remote_falls_back_only_for_network_or_upgrade_incompatibility(
    monkeypatch: pytest.MonkeyPatch, failure: Exception
) -> None:
    async def reject(*_: object, **__: object) -> Any:
        raise failure

    monkeypatch.setattr(transport_module, "ws_connect", reject)
    monkeypatch.setattr(transport_module, "HttpTransport", FakeHttpTransport)

    selected = await open_remote_transport("https://example.test", token="secret")

    assert isinstance(selected, FakeHttpTransport)
    assert selected.started


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [401, 403])
async def test_remote_never_falls_back_on_websocket_authentication_failure(
    monkeypatch: pytest.MonkeyPatch, status: int
) -> None:
    async def reject(*_: object, **__: object) -> Any:
        raise rejected(status)

    fallback_started = False

    class ForbiddenFallback(FakeHttpTransport):
        async def start(self) -> None:
            nonlocal fallback_started
            fallback_started = True

    monkeypatch.setattr(transport_module, "ws_connect", reject)
    monkeypatch.setattr(transport_module, "HttpTransport", ForbiddenFallback)

    with pytest.raises(AuthenticationError) as captured:
        await open_remote_transport("https://example.test", token="bad-secret")

    assert captured.value.status_code == status
    assert not fallback_started


@pytest.mark.asyncio
async def test_remote_never_falls_back_on_server_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    async def reject(*_: object, **__: object) -> Any:
        raise rejected(500)

    fallback_started = False

    class ForbiddenFallback(FakeHttpTransport):
        async def start(self) -> None:
            nonlocal fallback_started
            fallback_started = True

    monkeypatch.setattr(transport_module, "ws_connect", reject)
    monkeypatch.setattr(transport_module, "HttpTransport", ForbiddenFallback)

    with pytest.raises(InvalidStatus):
        await open_remote_transport("https://example.test", token="secret")

    assert not fallback_started


@pytest.mark.asyncio
async def test_remote_never_falls_back_on_tls_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    async def reject(*_: object, **__: object) -> Any:
        raise ssl.SSLCertVerificationError("untrusted certificate")

    fallback_started = False

    class ForbiddenFallback(FakeHttpTransport):
        async def start(self) -> None:
            nonlocal fallback_started
            fallback_started = True

    monkeypatch.setattr(transport_module, "ws_connect", reject)
    monkeypatch.setattr(transport_module, "HttpTransport", ForbiddenFallback)

    with pytest.raises(ssl.SSLCertVerificationError):
        await open_remote_transport("https://example.test", token="secret")

    assert not fallback_started


class RecordingJsonRpcTransport(_JsonRpcTransport):
    def __init__(self) -> None:
        super().__init__()
        self.sent = 0
        self.sent_event = asyncio.Event()

    async def _send(self, message: Mapping[str, object]) -> None:
        del message
        self.sent += 1
        self.sent_event.set()


@pytest.mark.asyncio
async def test_reader_failure_is_terminal_for_later_requests() -> None:
    transport = RecordingJsonRpcTransport()
    first = asyncio.create_task(transport.request("initialize", {}))
    await transport.sent_event.wait()
    failure = TransportError("reader failed")
    await transport._fail(failure)

    with pytest.raises(TransportError, match="reader failed"):
        await first
    with pytest.raises(TransportError, match="reader failed"):
        await transport.request("initialize", {})

    assert transport.sent == 1


@pytest.mark.asyncio
async def test_send_failure_consumes_the_failed_pending_future() -> None:
    class FailingSendTransport(_JsonRpcTransport):
        async def _send(self, message: Mapping[str, object]) -> None:
            del message
            error = TransportError("send failed")
            await self._fail(error)
            raise error

    loop = asyncio.get_running_loop()
    unhandled: list[dict[str, Any]] = []
    previous_handler = loop.get_exception_handler()
    loop.set_exception_handler(lambda _loop, context: unhandled.append(context))
    try:
        with pytest.raises(TransportError, match="send failed"):
            await FailingSendTransport().request("initialize", {})
        gc.collect()
        await asyncio.sleep(0)
    finally:
        loop.set_exception_handler(previous_handler)

    assert not [
        context
        for context in unhandled
        if context.get("message") == "Future exception was never retrieved"
    ]


@pytest.mark.asyncio
async def test_callback_send_failure_is_terminal_without_an_unhandled_task() -> None:
    class FailingCallbackTransport(_JsonRpcTransport):
        async def _send(self, message: Mapping[str, object]) -> None:
            del message
            raise TransportError("callback send failed")

    loop = asyncio.get_running_loop()
    unhandled: list[dict[str, Any]] = []
    previous_handler = loop.get_exception_handler()
    loop.set_exception_handler(lambda _loop, context: unhandled.append(context))
    transport = FailingCallbackTransport()
    try:
        await transport._dispatch_message(
            {
                "jsonrpc": "2.0",
                "id": "server-request",
                "method": "unknown/server/request",
                "params": {},
            }
        )
        await asyncio.sleep(0)
        await asyncio.sleep(0)
        with pytest.raises(TransportError, match="callback send failed"):
            await transport.request("initialize", {})
        gc.collect()
        await asyncio.sleep(0)
    finally:
        loop.set_exception_handler(previous_handler)

    assert not [
        context
        for context in unhandled
        if context.get("message") == "Task exception was never retrieved"
    ]


@pytest.mark.asyncio
async def test_http_body_limit_is_enforced_while_streaming(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class ChunkedResponse:
        async def aiter_bytes(self) -> AsyncIterator[bytes]:
            yield b"1234"
            yield b"56789"

    monkeypatch.setattr(transport_module, "_MAX_MESSAGE_BYTES", 8)

    with pytest.raises(TransportError, match="exceeds"):
        await _read_limited_body(ChunkedResponse())  # type: ignore[arg-type]
