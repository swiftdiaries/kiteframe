from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path
from uuid import UUID

import pytest

import kiteframe.client as client_module
from kiteframe import (
    CapabilityTarget,
    HarnessConfig,
    Kiteframe,
    PermissionChoice,
    PermissionRequest,
    PolicyRule,
    RpcError,
    SessionPolicy,
    TransportError,
)
from kiteframe.binary import HarnessBinary

FAKE_HARNESS = Path(__file__).with_name("fake_harness.py")


def policy() -> SessionPolicy:
    return SessionPolicy(
        version=1,
        rules=(
            PolicyRule(
                rule_id="allow-reads",
                effect="permit",
                target=CapabilityTarget(("read_data",)),
            ),
        ),
    )


def config(tmp_path: Path) -> HarnessConfig:
    return HarnessConfig(
        database_url="postgres://localhost/kiteframe",
        principal="python-test",
        responses_base_url="https://model.example/v1",
        responses_api_key="model-secret",
        responses_model="test-model",
        working_directory=tmp_path,
    )


def empty_policy() -> SessionPolicy:
    return SessionPolicy(version=1, rules=())


@pytest.mark.asyncio
async def test_local_client_owns_ndjson_lifecycle_policy_permissions_and_event_cursor(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    log = tmp_path / "requests.ndjson"
    closed = tmp_path / "closed"
    monkeypatch.setenv("FAKE_LOG", str(log))
    monkeypatch.setenv("FAKE_CLOSED", str(closed))
    monkeypatch.setattr(
        client_module,
        "resolve_harness",
        lambda: HarnessBinary((sys.executable, str(FAKE_HARNESS))),
    )
    seen_permission: list[UUID] = []

    async def choose(permission: PermissionRequest) -> PermissionChoice:
        seen_permission.append(permission.session_id)
        return "allow-once"

    client = await Kiteframe.local(
        config=config(tmp_path), policy=empty_policy(), permission_handler=choose
    )
    async with client:
        published = await client.publish("workflow demo() -> string { start t() }")
        run = await client.start("demo", {"value": 1}, idempotency_key="start-demo")
        first_page = await client.events(run.id, after=0, limit=2)
        resumed = await client.events(run.id, after=first_page.cursor, limit=2)

    messages = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
    requests = [message for message in messages if "method" in message]
    assert [request["method"] for request in requests[:3]] == [
        "initialize",
        "session/new",
        "_kiteframe/session/policy/update",
    ]
    assert requests[2]["params"]["policy"] == empty_policy().to_json()
    assert requests[-1]["params"]["after"] == 2
    permission_responses = [message for message in messages if message.get("id") == "permission-1"]
    assert permission_responses == [
        {
            "jsonrpc": "2.0",
            "id": "permission-1",
            "result": {"outcome": {"outcome": "selected", "optionId": "allow-once"}},
        }
    ]
    assert published.name == "demo"
    assert run.workflow_name == "demo"
    assert first_page.cursor == 2
    assert resumed.cursor == 3
    assert seen_permission == [client.session_id]
    assert closed.read_text(encoding="utf-8") == "closed\n"


@pytest.mark.asyncio
async def test_local_startup_failure_surfaces_stderr_and_closes_process(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("FAKE_LOG", str(tmp_path / "requests.ndjson"))
    monkeypatch.setenv("FAKE_CLOSED", str(tmp_path / "closed"))
    monkeypatch.setenv("FAKE_STARTUP_FAILURE", "database migration unavailable")
    monkeypatch.setattr(
        client_module,
        "resolve_harness",
        lambda: HarnessBinary((sys.executable, str(FAKE_HARNESS))),
    )

    with pytest.raises(TransportError, match="database migration unavailable"):
        await Kiteframe.local(config=config(tmp_path), policy=policy())


@pytest.mark.asyncio
async def test_local_startup_timeout_terminates_a_hung_harness(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    script = tmp_path / "hung_harness.py"
    closed = tmp_path / "closed"
    script.write_text(
        "import pathlib, sys, time\n"
        "if sys.argv[1:] == ['--version']:\n print('kiteframe-harness 0.1.0'); raise SystemExit\n"
        "try:\n time.sleep(60)\n"
        f"finally:\n pathlib.Path({str(closed)!r}).write_text('closed')\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(
        client_module,
        "resolve_harness",
        lambda: HarnessBinary((sys.executable, str(script))),
    )
    timed_config = config(tmp_path)
    timed_config = HarnessConfig(
        database_url=timed_config.database_url,
        principal=timed_config.principal,
        responses_base_url=timed_config.responses_base_url,
        responses_api_key=timed_config.responses_api_key,
        responses_model=timed_config.responses_model,
        working_directory=tmp_path,
        startup_timeout=0.1,
    )

    started = asyncio.get_running_loop().time()
    with pytest.raises(TransportError, match="did not initialize within"):
        await Kiteframe.local(config=timed_config, policy=policy())
    assert asyncio.get_running_loop().time() - started < 1


class RecordingTransport:
    def __init__(
        self,
        *,
        protocol_version: int = 1,
        initialize_error: RpcError | None = None,
        reject_v2: RpcError | None = None,
    ) -> None:
        self.protocol_version = protocol_version
        self.initialize_error = initialize_error
        self.reject_v2 = reject_v2
        self.requests: list[tuple[str, dict[str, object]]] = []
        self.closed = False
        self.opened_session: str | None = None

    async def start(self) -> None:
        return None

    async def request(self, method: str, params: dict[str, object]) -> dict[str, object]:
        self.requests.append((method, params))
        if method == "initialize":
            if self.initialize_error is not None:
                raise self.initialize_error
            if params["protocolVersion"] == 2 and self.reject_v2 is not None:
                raise self.reject_v2
            info_key = "agentInfo" if self.protocol_version == 1 else "info"
            capabilities_key = "agentCapabilities" if self.protocol_version == 1 else "capabilities"
            return {
                "protocolVersion": self.protocol_version,
                info_key: {"name": "kiteframe", "version": "0.1.0"},
                capabilities_key: {},
                "_meta": params["_meta"],
            }
        if method == "session/new":
            return {"sessionId": "019ff400-0000-7000-8000-000000000010"}
        if method == "_kiteframe/session/policy/update":
            return {"revision": 1}
        raise AssertionError(f"unexpected method: {method}")

    async def open_session(self, session_id: str) -> None:
        self.opened_session = session_id

    async def close(self) -> None:
        self.closed = True

    async def abort(self) -> None:
        self.closed = True


@pytest.mark.asyncio
async def test_remote_negotiates_v1_without_offering_v2(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    transport = RecordingTransport(protocol_version=1)

    async def open_transport(*_: object, **__: object) -> RecordingTransport:
        return transport

    monkeypatch.setattr(client_module, "open_remote_transport", open_transport)
    monkeypatch.chdir(tmp_path)

    client = await Kiteframe.connect("https://kiteframe.example", token="secret", policy=policy())
    await client.close()

    assert transport.requests[0][1]["protocolVersion"] == 1
    assert transport.opened_session == str(client.session_id)


@pytest.mark.asyncio
async def test_remote_v2_application_error_is_not_retried_as_v1(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    transport = RecordingTransport(
        protocol_version=2,
        initialize_error=RpcError(-32000, "application request failed"),
    )

    async def open_transport(*_: object, **__: object) -> RecordingTransport:
        return transport

    monkeypatch.setattr(client_module, "open_remote_transport", open_transport)

    with pytest.raises(RpcError, match="application request failed"):
        await Kiteframe.connect(
            "https://kiteframe.example",
            token="secret",
            policy=policy(),
            enable_acp_v2=True,
        )

    assert [method for method, _ in transport.requests] == ["initialize"]
    assert transport.closed


@pytest.mark.asyncio
async def test_remote_v2_protocol_rejection_negotiates_v1_on_the_same_transport(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    transport = RecordingTransport(
        protocol_version=1,
        reject_v2=RpcError(-32602, "Invalid params"),
    )

    async def open_transport(*_: object, **__: object) -> RecordingTransport:
        return transport

    monkeypatch.setattr(client_module, "open_remote_transport", open_transport)

    client = await Kiteframe.connect(
        "https://kiteframe.example", token="secret", policy=policy(), enable_acp_v2=True
    )
    await client.close()

    assert [params["protocolVersion"] for method, params in transport.requests if method == "initialize"] == [2, 1]
    assert client.protocol_version == 1


@pytest.mark.asyncio
async def test_remote_startup_timeout_closes_a_peer_that_never_initializes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    transport = RecordingTransport()

    async def stall(method: str, params: dict[str, object]) -> dict[str, object]:
        transport.requests.append((method, params))
        await asyncio.Event().wait()
        raise AssertionError("unreachable")

    transport.request = stall  # type: ignore[method-assign]

    async def open_transport(*_: object, **__: object) -> RecordingTransport:
        return transport

    monkeypatch.setattr(client_module, "open_remote_transport", open_transport)

    with pytest.raises(TransportError, match="did not initialize within 0.01 seconds"):
        await Kiteframe.connect(
            "https://kiteframe.example",
            token="secret",
            policy=policy(),
            startup_timeout=0.01,
        )

    assert transport.closed


@pytest.mark.asyncio
async def test_remote_startup_timeout_includes_transport_establishment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def never_connect(*_: object, **__: object) -> RecordingTransport:
        await asyncio.Event().wait()
        raise AssertionError("unreachable")

    monkeypatch.setattr(client_module, "open_remote_transport", never_connect)

    with pytest.raises(TransportError, match="did not initialize within 0.01 seconds"):
        await Kiteframe.connect(
            "https://kiteframe.example",
            token="secret",
            policy=policy(),
            startup_timeout=0.01,
        )
