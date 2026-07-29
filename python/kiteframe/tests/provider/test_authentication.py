import json
import ssl
from collections.abc import Mapping
from dataclasses import asdict
from typing import Literal

import httpx
import pytest
from test_http_client import (
    admission_request,
    canonical_bytes,
    capability_catalog_bytes,
    catalog_request,
    grant_set_bytes,
    invocation_request,
    resolved_runtime_inputs,
    runtime_requirement,
    status_request,
    traceback_retains,
)

from kiteframe.provider import (
    ProviderAuthRequest,
    ProviderHttpClient,
    ProviderOperation,
    ProviderTransportError,
)


class RecordingAuthenticator:
    def __init__(self, headers: Mapping[str, str]) -> None:
        self.headers = headers
        self.requests: list[ProviderAuthRequest] = []

    async def credential_headers(
        self,
        request: ProviderAuthRequest,
    ) -> Mapping[str, str]:
        self.requests.append(request)
        return self.headers


class RefreshingAuthenticator:
    def __init__(self) -> None:
        self.calls = 0

    async def credential_headers(
        self,
        request: ProviderAuthRequest,
    ) -> Mapping[str, str]:
        self.calls += 1
        return {"x-provider-token": f"secret-{self.calls}"}


class FailingAuthenticator:
    async def credential_headers(
        self,
        request: ProviderAuthRequest,
    ) -> Mapping[str, str]:
        raise RuntimeError("credential-that-must-not-escape")


def response_for(request: httpx.Request) -> httpx.Response:
    if request.url.path == "/v1/capability-catalog":
        body = capability_catalog_bytes()
        return httpx.Response(
            200,
            content=body,
            headers={"etag": json.loads(body)["catalogDigest"]},
        )
    if request.url.path == "/v1/capability-admissions":
        return httpx.Response(200, content=grant_set_bytes())
    return httpx.Response(
        200,
        content=canonical_bytes(
            {
                "invocation_id": "inv-1",
                "result": {"caseId": "case-1"},
                "status": "succeeded",
            }
        ),
    )


async def call_operation(
    client: ProviderHttpClient,
    operation: ProviderOperation,
) -> None:
    if operation == "catalog":
        await client.catalog(catalog_request())
    elif operation == "admit":
        await client.admit(admission_request())
    elif operation == "invoke":
        await client.invoke(invocation_request())
    else:
        await client.status(
            status_request(), invocation_request(), runtime_requirement()
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("operation", "method", "route"),
    [
        ("catalog", "GET", "/v1/capability-catalog"),
        ("admit", "POST", "/v1/capability-admissions"),
        ("invoke", "POST", "/v1/capability-invocations/cases.read"),
        ("status", "GET", "/v1/capability-invocations/inv-1"),
    ],
)
async def test_authenticator_is_called_per_request_with_safe_metadata(
    operation: ProviderOperation,
    method: Literal["GET", "POST"],
    route: str,
) -> None:
    authenticator = RecordingAuthenticator({"x-provider-token": "secret-value"})
    captured: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        captured.append(request)
        return response_for(request)

    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=authenticator,
        credential_header_allowlist=frozenset({"x-provider-token"}),
        transport=httpx.MockTransport(handler),
    )
    try:
        await call_operation(client, operation)
    finally:
        await client.aclose()

    assert len(captured) == 1
    assert captured[0].headers["x-provider-token"] == "secret-value"
    assert [asdict(request) for request in authenticator.requests] == [
        {
            "method": method,
            "operation": operation,
            "origin": "https://provider.test",
            "route": route,
        }
    ]


@pytest.mark.asyncio
async def test_authenticator_refreshes_credentials_for_every_call() -> None:
    authenticator = RefreshingAuthenticator()
    captured_tokens: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        captured_tokens.append(request.headers["x-provider-token"])
        return response_for(request)

    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=authenticator,
        credential_header_allowlist=frozenset({"x-provider-token"}),
        transport=httpx.MockTransport(handler),
    )
    try:
        await client.catalog(catalog_request())
        await client.catalog(catalog_request())
    finally:
        await client.aclose()

    assert captured_tokens == ["secret-1", "secret-2"]


@pytest.mark.asyncio
async def test_credentials_never_enter_body_baggage_or_failure_text() -> None:
    secret = "credential-that-must-not-escape"
    captured: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        captured.append(request)
        return httpx.Response(503, content=b"invalid")

    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=RecordingAuthenticator(
            {"authorization": f"Bearer {secret}"}
        ),
        credential_header_allowlist=frozenset({"authorization"}),
        transport=httpx.MockTransport(handler),
    )
    try:
        with pytest.raises(Exception) as error:
            await client.invoke(invocation_request())
    finally:
        await client.aclose()

    assert len(captured) == 1
    assert secret.encode() not in captured[0].content
    assert secret not in captured[0].headers.get("baggage", "")
    assert secret not in str(error.value)
    assert secret not in repr(error.value)
    assert error.value.__cause__ is None
    assert error.value.__context__ is None
    assert not traceback_retains(error.value, secret)


def test_authenticator_requires_an_explicit_nonempty_allowlist() -> None:
    with pytest.raises(ValueError, match="allowlist"):
        ProviderHttpClient(
            "https://provider.test",
            resolved_runtime_inputs=resolved_runtime_inputs(),
            authenticator=RecordingAuthenticator({"authorization": "secret"}),
        )


@pytest.mark.asyncio
async def test_credential_header_names_are_normalized_to_lowercase_ascii() -> None:
    captured: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        captured.append(request)
        return response_for(request)

    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=RecordingAuthenticator({"X-PROVIDER-Token": "secret"}),
        credential_header_allowlist=frozenset({"X-Provider-Token"}),
        transport=httpx.MockTransport(handler),
    )
    try:
        await client.catalog(catalog_request())
    finally:
        await client.aclose()

    assert list(captured[0].headers.keys()).count("x-provider-token") == 1
    assert captured[0].headers["x-provider-token"] == "secret"


@pytest.mark.parametrize(
    "allowlist",
    [
        frozenset({"X-Provider-Token", "x-provider-token"}),
        frozenset({"x-prøvider-token"}),
    ],
)
def test_credential_allowlist_rejects_duplicate_or_non_ascii_names(
    allowlist: frozenset[str],
) -> None:
    with pytest.raises(ValueError, match="allowlist"):
        ProviderHttpClient(
            "https://provider.test",
            resolved_runtime_inputs=resolved_runtime_inputs(),
            authenticator=RecordingAuthenticator({"x-provider-token": "secret"}),
            credential_header_allowlist=allowlist,
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "headers",
    [
        {"X-Provider-Token": "one", "x-provider-token": "two"},
        {"x-provider-token": "secret", "x-other-token": "secret"},
        {"x-prøvider-token": "secret"},
    ],
)
async def test_authenticator_rejects_duplicate_unlisted_or_non_ascii_names(
    headers: Mapping[str, str],
) -> None:
    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=RecordingAuthenticator(headers),
        credential_header_allowlist=frozenset({"x-provider-token"}),
        transport=httpx.MockTransport(response_for),
    )
    try:
        with pytest.raises(ValueError, match="credential header"):
            await client.catalog(catalog_request())
    finally:
        await client.aclose()


@pytest.mark.parametrize(
    "header",
    [
        "host",
        "content-length",
        "content-type",
        "accept",
        "accept-encoding",
        "traceparent",
        "tracestate",
        "baggage",
        "cookie",
        "set-cookie",
        "proxy-authorization",
        "proxy-connection",
    ],
)
def test_authenticator_cannot_control_blocked_headers(header: str) -> None:
    with pytest.raises(ValueError, match="forbidden"):
        ProviderHttpClient(
            "https://provider.test",
            resolved_runtime_inputs=resolved_runtime_inputs(),
            authenticator=RecordingAuthenticator({header: "secret"}),
            credential_header_allowlist=frozenset({header}),
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "value",
    ["secret\rvalue", "secret\nvalue", "secret\0value", "secret-\N{SNOWMAN}"],
)
async def test_unsafe_credential_values_are_rejected(value: str) -> None:
    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=RecordingAuthenticator({"x-provider-token": value}),
        credential_header_allowlist=frozenset({"x-provider-token"}),
        transport=httpx.MockTransport(response_for),
    )
    try:
        with pytest.raises(ValueError, match="credential header value"):
            await client.catalog(catalog_request())
    finally:
        await client.aclose()


@pytest.mark.asyncio
async def test_authenticator_failure_text_is_redacted() -> None:
    secret = "credential-that-must-not-escape"
    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=FailingAuthenticator(),
        credential_header_allowlist=frozenset({"authorization"}),
        transport=httpx.MockTransport(response_for),
    )
    try:
        with pytest.raises(ProviderTransportError) as error:
            await client.catalog(catalog_request())
    finally:
        await client.aclose()

    assert str(error.value) == "provider authentication failed"
    assert secret not in str(error.value)
    assert secret not in repr(error.value)
    assert error.value.__cause__ is None
    assert error.value.__context__ is None
    assert not traceback_retains(error.value, secret)


@pytest.mark.asyncio
async def test_transport_failure_clears_credentials_from_traceback_locals() -> None:
    secret = "credential-that-must-not-escape"
    captured: list[httpx.Request] = []

    def fail(request: httpx.Request) -> httpx.Response:
        captured.append(request)
        raise httpx.ConnectError(secret, request=request)

    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        authenticator=RecordingAuthenticator(
            {"authorization": f"Bearer {secret}"}
        ),
        credential_header_allowlist=frozenset({"authorization"}),
        transport=httpx.MockTransport(fail),
    )
    try:
        with pytest.raises(ProviderTransportError) as error:
            await client.invoke(invocation_request())
    finally:
        await client.aclose()

    assert len(captured) == 1
    assert secret.encode() not in captured[0].content
    assert secret not in captured[0].headers.get("baggage", "")
    assert secret not in str(error.value)
    assert secret not in repr(error.value)
    assert error.value.__cause__ is None
    assert error.value.__context__ is None
    assert not traceback_retains(error.value, secret)


@pytest.mark.asyncio
async def test_client_accepts_a_deployment_built_tls_context() -> None:
    tls_context = ssl.create_default_context()
    client = ProviderHttpClient(
        "https://provider.test",
        resolved_runtime_inputs=resolved_runtime_inputs(),
        tls_context=tls_context,
    )
    try:
        transport = client._client._transport  # type: ignore[reportPrivateUsage]
        assert transport._pool._ssl_context is tls_context  # type: ignore[reportPrivateUsage]
    finally:
        await client.aclose()


@pytest.mark.asyncio
@pytest.mark.parametrize("candidate", [False, "certificate.pem", object()])
async def test_tls_context_rejects_values_that_could_disable_verification(
    candidate: object,
) -> None:
    client: ProviderHttpClient | None = None
    try:
        with pytest.raises(TypeError, match="ssl.SSLContext"):
            client = ProviderHttpClient(
                "https://provider.test",
                resolved_runtime_inputs=resolved_runtime_inputs(),
                tls_context=candidate,  # type: ignore[arg-type]
            )
    finally:
        if client is not None:
            await client.aclose()
