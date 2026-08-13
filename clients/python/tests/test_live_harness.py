from __future__ import annotations

import os
from pathlib import Path
from uuid import uuid4

import pytest

import kiteframe.client as client_module
from kiteframe import HarnessConfig, Kiteframe, SessionPolicy
from kiteframe.binary import HarnessBinary


@pytest.mark.asyncio
async def test_local_client_initializes_a_real_harness_when_integration_inputs_are_configured(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    database_url = os.getenv("KITEFRAME_INTEGRATION_DATABASE_URL")
    harness_path = os.getenv("KITEFRAME_HARNESS_PATH")
    if database_url is None or harness_path is None:
        pytest.skip("real harness integration inputs are not configured")

    monkeypatch.setattr(
        client_module, "resolve_harness", lambda: HarnessBinary((harness_path,))
    )
    client = await Kiteframe.local(
        config=HarnessConfig(
            database_url=database_url,
            principal=f"python-live-{uuid4()}",
            responses_base_url="http://127.0.0.1:9/v1",
            responses_api_key="integration-test",
            responses_model="test-model",
            working_directory=tmp_path,
        ),
        policy=SessionPolicy(version=1, rules=()),
    )
    try:
        assert client.protocol_version == 1
        assert client.session_id
    finally:
        await client.close()
