from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any, Self
from uuid import UUID, uuid4

from .binary import package_version, resolve_harness, validate_harness_version
from .models import (
    EventPage,
    HarnessConfig,
    PermissionChoice,
    PermissionRequest,
    PublishedWorkflow,
    RpcError,
    Run,
    RunEvent,
    SessionPolicy,
    TransportError,
    VersionMismatchError,
)
from .transport import (
    JsonRpcTransport,
    PermissionHandler,
    StdioTransport,
    open_remote_transport,
)

_EXTENSION_VERSION = 1


class Kiteframe:
    def __init__(
        self,
        transport: JsonRpcTransport,
        session_id: UUID,
        protocol_version: int,
    ) -> None:
        self._transport = transport
        self.session_id = session_id
        self.protocol_version = protocol_version
        self._closed = False

    @classmethod
    async def local(
        cls,
        *,
        config: HarnessConfig,
        policy: SessionPolicy,
        permission_handler: PermissionHandler | None = None,
    ) -> Kiteframe:
        binary = resolve_harness()
        expected_version = package_version()
        transport: JsonRpcTransport | None = None
        try:
            async with asyncio.timeout(config.startup_timeout):
                await validate_harness_version(
                    binary, expected_version, timeout=min(config.startup_timeout, 5.0)
                )
                transport = StdioTransport(binary, config.environment(), permission_handler)
                await transport.start()
                return await cls._initialize(
                    transport,
                    policy=policy,
                    working_directory=Path(config.working_directory),
                    enable_acp_v2=config.enable_acp_v2,
                    expected_version=expected_version,
                )
        except asyncio.CancelledError:
            if transport is not None:
                await transport.abort()
            raise
        except TimeoutError as error:
            if transport is not None:
                await transport.abort()
            raise TransportError(
                f"kiteframe-harness did not initialize within {config.startup_timeout:g} seconds"
            ) from error
        except BaseException:
            if transport is not None:
                await transport.close()
            raise

    @classmethod
    async def connect(
        cls,
        url: str,
        *,
        token: str,
        policy: SessionPolicy,
        permission_handler: PermissionHandler | None = None,
        enable_acp_v2: bool = False,
        working_directory: str | Path | None = None,
        startup_timeout: float = 10.0,
    ) -> Kiteframe:
        if startup_timeout <= 0:
            raise ValueError("startup_timeout must be positive")
        transport: JsonRpcTransport | None = None
        try:
            async with asyncio.timeout(startup_timeout):
                transport = await open_remote_transport(
                    url, token=token, permission_handler=permission_handler
                )
                return await cls._initialize(
                    transport,
                    policy=policy,
                    working_directory=(
                        Path.cwd() if working_directory is None else Path(working_directory)
                    ),
                    enable_acp_v2=enable_acp_v2,
                    expected_version=package_version(),
                )
        except asyncio.CancelledError:
            if transport is not None:
                await transport.abort()
            raise
        except TimeoutError as error:
            if transport is not None:
                await transport.abort()
            raise TransportError(
                f"remote Kiteframe did not initialize within {startup_timeout:g} seconds"
            ) from error
        except BaseException:
            if transport is not None:
                await transport.close()
            raise

    @classmethod
    async def _initialize(
        cls,
        transport: JsonRpcTransport,
        *,
        policy: SessionPolicy,
        working_directory: Path,
        enable_acp_v2: bool,
        expected_version: str,
    ) -> Kiteframe:
        offered_versions = (2, 1) if enable_acp_v2 else (1,)
        metadata: dict[str, object] = {
            "kiteframe": {
                "clientVersion": expected_version,
                "extensionVersion": _EXTENSION_VERSION,
            }
        }
        protocol_version, initialized = await cls._negotiate_protocol(
            transport, offered_versions, metadata, expected_version
        )
        cls._validate_initialized(
            initialized,
            protocol_version=protocol_version,
            expected_version=expected_version,
            metadata=metadata,
        )
        cwd = str(working_directory.resolve())
        session_params: dict[str, object] = {"cwd": cwd}
        if protocol_version == 1:
            session_params["mcpServers"] = []
        session = await transport.request("session/new", session_params)
        session_id = UUID(str(session["sessionId"]))
        await transport.open_session(str(session_id))
        await transport.request(
            "_kiteframe/session/policy/update",
            {
                "expectedRevision": 0,
                "idempotencyKey": f"python-policy-{uuid4()}",
                "policy": policy.to_json(),
            },
        )
        return cls(transport, session_id, protocol_version)

    @staticmethod
    async def _negotiate_protocol(
        transport: JsonRpcTransport,
        offered_versions: tuple[int, ...],
        metadata: dict[str, object],
        expected_version: str,
    ) -> tuple[int, dict[str, Any]]:
        for index, protocol_version in enumerate(offered_versions):
            if protocol_version == 1:
                initialize: dict[str, object] = {
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "kiteframe-python", "version": expected_version},
                    "_meta": metadata,
                }
            else:
                initialize = {
                    "protocolVersion": 2,
                    "capabilities": {},
                    "info": {"name": "kiteframe-python", "version": expected_version},
                    "_meta": metadata,
                }
            try:
                return protocol_version, await transport.request("initialize", initialize)
            except RpcError as error:
                retryable_negotiation = error.code == -32602
                if index + 1 == len(offered_versions) or not retryable_negotiation:
                    raise
        raise AssertionError("at least one ACP protocol version is always offered")

    @staticmethod
    def _validate_initialized(
        initialized: dict[str, Any],
        *,
        protocol_version: int,
        expected_version: str,
        metadata: dict[str, object],
    ) -> None:
        if initialized.get("protocolVersion") != protocol_version:
            raise VersionMismatchError(
                f"harness negotiated ACP {initialized.get('protocolVersion')!r}; "
                f"client offered ACP {protocol_version}"
            )
        info = initialized.get("agentInfo" if protocol_version == 1 else "info")
        if not isinstance(info, dict) or info.get("name") != "kiteframe":
            raise VersionMismatchError("ACP peer did not identify itself as the Kiteframe harness")
        harness_version = info.get("version")
        if harness_version != expected_version:
            raise VersionMismatchError(
                f"kiteframe client {expected_version} is incompatible with harness {harness_version}"
            )
        if initialized.get("_meta") != metadata:
            raise VersionMismatchError("Kiteframe extension version negotiation failed")

    async def publish(self, source: str) -> PublishedWorkflow:
        result = await self._transport.request(
            "_kiteframe/workflow/publish", {"source": source}
        )
        return PublishedWorkflow.from_json(result)

    async def start(self, name: str, input: object, *, idempotency_key: str) -> Run:
        result = await self._transport.request(
            "_kiteframe/run/start",
            {
                "sessionId": str(self.session_id),
                "workflowName": name,
                "input": input,
                "idempotencyKey": idempotency_key,
            },
        )
        return Run.from_json(result)

    async def get(self, run_id: UUID) -> Run:
        result = await self._transport.request(
            "_kiteframe/run/get", {"runId": str(run_id)}
        )
        return Run.from_json(result)

    async def events(self, run_id: UUID, *, after: int = 0, limit: int = 64) -> EventPage:
        if after < 0:
            raise ValueError("event cursor must not be negative")
        if not 1 <= limit <= 256:
            raise ValueError("event limit must be between 1 and 256")
        result = await self._transport.request(
            "_kiteframe/run/event",
            {"runId": str(run_id), "after": after, "limit": limit},
        )
        raw_events = result.get("events")
        if not isinstance(raw_events, list):
            raise TypeError("event response must contain an events list")
        events = tuple(RunEvent.from_json(event) for event in raw_events)
        cursor = events[-1].sequence if events else after
        return EventPage(events=events, cursor=cursor)

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        await self._transport.close()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.close()


__all__ = ["Kiteframe", "PermissionChoice", "PermissionHandler", "PermissionRequest"]
