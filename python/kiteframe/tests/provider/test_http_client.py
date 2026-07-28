import hashlib
import json

import httpx
import pytest

from kiteframe import (
    CatalogRequest,
    KiteframeDiagnosticError,
    load_admission_request,
    load_catalog_request,
    load_invocation_request,
)
from kiteframe._native import (
    CapabilityCatalog,
    CapabilityGrantSet,
    InvocationOutcome,
    InvocationStatus,
)
from kiteframe.provider import (
    PROVIDER_RESPONSE_LIMIT_BYTES,
    ProviderHttpClient,
    ProviderTransportError,
    trace_headers,
)

VALID_TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def catalog_request():
    return load_catalog_request(
        canonical_bytes(
            {
                "knownCatalogDigest": "09" * 32,
                "traceContext": {
                    "baggage": {
                        "kiteframe.request_id": "10" * 16,
                        "kiteframe.session_id": "11" * 16,
                    },
                    "traceparent": VALID_TRACEPARENT,
                    "tracestate": "vendor=value",
                },
            }
        )
    )


def admission_request():
    capability = {
        "name": "cases.comment",
        "version": "1.0.0",
    }
    return load_admission_request(
        canonical_bytes(
            {
                "actor": "actor:alice",
                "agent": "agent:case-worker",
                "contextualFacts": {},
                "delegationAncestry": [],
                "lockDigest": "02" * 32,
                "optionalCapabilities": [],
                "portableDigest": "01" * 32,
                "requiredCapabilities": [
                    {
                        "capability": capability,
                        "resources": ["tenant:t1/case:case-1"],
                    }
                ],
                "resolvedDigest": "03" * 32,
                "resolvedRequirements": [
                    {
                        "identity": capability,
                        "required": True,
                        "resources": ["tenant:t1/case:case-1"],
                    }
                ],
                "session": "session:1",
                "task": "task:triage",
                "traceContext": {
                    "traceparent": VALID_TRACEPARENT,
                    "tracestate": "vendor=value",
                },
            }
        )
    )


def invocation_request():
    return load_invocation_request(
        canonical_bytes(
            {
                "admissionId": "adm-1",
                "arguments": {"caseId": "case-1"},
                "capability": {
                    "name": "cases.comment",
                    "version": "1.0.0",
                },
                "evidenceRefs": {"approval": "evidence://approval/1"},
                "idempotencyKey": "idem-1",
                "invocationId": "inv-1",
                "preconditions": {},
                "selectedResource": "tenant:t1/case:case-1",
                "traceContext": {
                    "traceparent": VALID_TRACEPARENT,
                    "tracestate": "vendor=value",
                },
            }
        )
    )


def capability_catalog_bytes() -> bytes:
    wire = {
        "descriptors": [],
        "identity": {
            "name": "provider.test",
            "revision": "revision-1",
        },
    }
    digest = hashlib.sha256(canonical_bytes(wire)).hexdigest()
    return canonical_bytes({**wire, "catalogDigest": digest})


def grant_set_bytes() -> bytes:
    grant_set = {
        "actor": "actor:alice",
        "admissionId": "adm-1",
        "agent": "agent:case-worker",
        "catalogDigest": "01" * 32,
        "expiresAt": 200,
        "grants": [
            {
                "capability": {
                    "name": "cases.comment",
                    "version": "1.0.0",
                },
                "resources": ["tenant:t1/case:case-1"],
            }
        ],
        "issuedAt": 100,
        "policyRevision": "policy:7",
        "session": "session:1",
        "task": "task:triage",
    }
    digest = hashlib.sha256(
        b"kiteframe:capability-grant-set:v1\0" + canonical_bytes(grant_set)
    ).hexdigest()
    return canonical_bytes({**grant_set, "grantDigest": digest})


def diagnostic_envelope() -> bytes:
    return canonical_bytes(
        {
            "diagnostics": [
                {
                    "category": "authorization",
                    "code": "KF-AUTH-001",
                    "details": {
                        "policyRevision": "policy:7",
                        "prompt": "must-not-escape",
                    },
                    "help": None,
                    "message": "admission was denied",
                    "package_path": None,
                    "retry": "after_user_action",
                    "severity": "error",
                    "source_range": None,
                    "stage": "admit",
                }
            ]
        }
    )


@pytest.mark.asyncio
async def test_client_calls_only_the_four_v1_routes_with_native_values() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        if request.url.path == "/v1/capability-catalog":
            return httpx.Response(200, content=capability_catalog_bytes())
        if request.url.path == "/v1/capability-admissions":
            return httpx.Response(200, content=grant_set_bytes())
        if request.url.path == "/v1/capability-invocations/cases.comment":
            return httpx.Response(
                200,
                content=canonical_bytes(
                    {"invocation_id": "inv-1", "status": "deferred"}
                ),
            )
        if request.url.path == "/v1/capability-invocations/inv-1":
            return httpx.Response(
                200,
                content=canonical_bytes(
                    {"invocation_id": "inv-1", "status": "pending"}
                ),
            )
        return httpx.Response(404, content=diagnostic_envelope())

    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(handler),
        baggage_allowlist=frozenset({"kiteframe.session_id"}),
    )
    try:
        catalog = await client.catalog(catalog_request())
        grant_set = await client.admit(admission_request())
        outcome = await client.invoke(invocation_request())
        status = await client.status("inv-1")
    finally:
        await client.aclose()

    assert isinstance(catalog, CapabilityCatalog)
    assert isinstance(grant_set, CapabilityGrantSet)
    assert isinstance(outcome, InvocationOutcome)
    assert isinstance(status, InvocationStatus)
    assert [(request.method, request.url.path) for request in seen] == [
        ("GET", "/v1/capability-catalog"),
        ("POST", "/v1/capability-admissions"),
        ("POST", "/v1/capability-invocations/cases.comment"),
        ("GET", "/v1/capability-invocations/inv-1"),
    ]
    assert seen[0].content == b""
    assert seen[0].headers["if-none-match"] == f'"{"09" * 32}"'
    assert seen[1].content == admission_request().canonical_json()
    assert seen[2].content == invocation_request().canonical_json()
    assert seen[0].headers["traceparent"] == VALID_TRACEPARENT
    assert seen[0].headers["tracestate"] == "vendor=value"
    assert seen[0].headers["baggage"] == (f"kiteframe.session_id={'11' * 16}")
    assert "baggage" not in seen[1].headers


@pytest.mark.asyncio
async def test_client_does_not_follow_redirects() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            307,
            headers={"location": "https://evil.invalid"},
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(ProviderTransportError, match="redirect"):
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()


@pytest.mark.asyncio
async def test_invalid_result_never_reaches_caller() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            content=canonical_bytes(
                {"invocation_id": "inv-1", "status": "not-a-status"}
            ),
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.invoke(invocation_request())
    finally:
        await client.aclose()

    assert error.value.code == "KF-CAP-002"


@pytest.mark.asyncio
async def test_every_success_body_uses_the_native_locked_schema_loader() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            content=b'{"catalogDigest":"'
            + b"00" * 32
            + b'","descriptors":[],"identity":{"name":"provider.test",'
            b'"revision":"revision-1"},"unlocked":true}',
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert error.value.code == "KF-CAP-002"


@pytest.mark.asyncio
async def test_response_body_is_bounded_before_parsing() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            content=b"x" * (PROVIDER_RESPONSE_LIMIT_BYTES + 1),
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(ProviderTransportError, match="body limit"):
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()


@pytest.mark.asyncio
async def test_structured_http_diagnostic_is_sanitized_and_raised() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(403, content=diagnostic_envelope())
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.admit(admission_request())
    finally:
        await client.aclose()

    assert error.value.code == "KF-AUTH-001"
    assert b"must-not-escape" not in error.value.diagnostics_json
    assert b"prompt" not in error.value.diagnostics_json
    assert json.loads(error.value.diagnostics_json)[0]["details"] == {
        "policyRevision": "policy:7"
    }


@pytest.mark.asyncio
async def test_malformed_http_error_never_exposes_response_body() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            500,
            content=b"provider credential is super-secret",
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(ProviderTransportError) as error:
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert "super-secret" not in str(error.value)


@pytest.mark.asyncio
async def test_non_json_diagnostic_constants_are_rejected() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            500,
            content=diagnostic_envelope().replace(
                b'"policy:7"',
                b"NaN",
            ),
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(
            ProviderTransportError,
            match="invalid diagnostic",
        ):
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()


def test_tls_is_required_outside_mock_transport() -> None:
    with pytest.raises(ValueError, match="HTTPS"):
        ProviderHttpClient("http://provider.test")

    with pytest.raises(ValueError, match="HTTPS"):
        ProviderHttpClient(
            "http://provider.test",
            transport=httpx.AsyncHTTPTransport(),
        )
    with pytest.raises(ValueError, match="HTTPS"):
        ProviderHttpClient(
            "ftp://provider.test",
            transport=httpx.MockTransport(lambda request: httpx.Response(500)),
        )


@pytest.mark.asyncio
async def test_mock_transport_may_use_plaintext_without_network_io() -> None:
    client = ProviderHttpClient(
        "http://provider.test",
        transport=httpx.MockTransport(
            lambda request: httpx.Response(
                200,
                content=capability_catalog_bytes(),
            )
        ),
    )
    try:
        catalog = await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert isinstance(catalog, CapabilityCatalog)


def test_baggage_drops_sensitive_and_unlisted_keys() -> None:
    headers = trace_headers(
        traceparent=VALID_TRACEPARENT,
        tracestate="vendor=value",
        baggage={
            "tenant.id": "t1",
            "prompt": "secret",
            "authorization": "tuple",
            "request.id": "request-1",
        },
        baggage_allowlist=frozenset({"tenant.id", "prompt", "authorization"}),
    )

    assert headers["traceparent"] == VALID_TRACEPARENT
    assert headers["tracestate"] == "vendor=value"
    assert headers["baggage"] == "tenant.id=t1"


def test_trace_headers_reject_header_injection() -> None:
    with pytest.raises(ValueError, match="traceparent"):
        trace_headers(traceparent=f"{VALID_TRACEPARENT}\r\nx-secret: value")
