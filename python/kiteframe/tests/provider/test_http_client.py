import gzip
import hashlib
import json
from functools import lru_cache
from pathlib import Path

import httpx
import pytest

from kiteframe import (
    CatalogRequest,
    KiteframeDiagnosticError,
    load_admission_request,
    load_catalog_request,
    load_invocation_request,
    provider,
    resolve_package,
)
from kiteframe._native import (
    CapabilityCatalog,
    CapabilityGrantSet,
    InvocationOutcome,
    InvocationStatus,
    ResolvedCapabilityRequirement,
    ResolvedRuntimeInputs,
)
from kiteframe.provider import (
    PROVIDER_RESPONSE_LIMIT_BYTES,
    ProviderTransportError,
    trace_headers,
)
from kiteframe.provider import (
    ProviderHttpClient as NativeProviderHttpClient,
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
                    "name": "cases.read",
                    "version": "1.2.0",
                },
                "evidenceRefs": {"approval": "evidence://approval/1"},
                "invocationId": "inv-1",
                "preconditions": {},
                "selectedResource": "tenant:support",
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


def grant_set_bytes(**overrides: object) -> bytes:
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
    grant_set.update(overrides)
    digest = hashlib.sha256(
        b"kiteframe:capability-grant-set:v1\0" + canonical_bytes(grant_set)
    ).hexdigest()
    return canonical_bytes({**grant_set, "grantDigest": digest})


def traceback_retains(error: BaseException, secret: str) -> bool:
    traceback = error.__traceback__
    while traceback is not None:
        filename = traceback.tb_frame.f_code.co_filename.replace("\\", "/")
        if "/kiteframe/provider/http.py" in filename:
            for value in traceback.tb_frame.f_locals.values():
                if isinstance(value, str) and secret in value:
                    return True
                if isinstance(value, (bytes, bytearray)) and secret.encode() in value:
                    return True
        traceback = traceback.tb_next
    return False


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


def unsafe_diagnostic_envelope(secret: str) -> bytes:
    return canonical_bytes(
        {
            "diagnostics": [
                {
                    "category": "authorization",
                    "code": "KF-AUTH-001",
                    "details": {
                        "apparentlySafe": secret,
                        "nested": [secret],
                    },
                    "help": secret,
                    "message": secret,
                    "package_path": secret,
                    "retry": "after_user_action",
                    "severity": "error",
                    "source_range": {"end": 9, "start": 1},
                    "stage": "admit",
                }
            ]
        }
    )


class UnsafeTransport(httpx.AsyncBaseTransport):
    async def handle_async_request(
        self,
        request: httpx.Request,
    ) -> httpx.Response:
        return httpx.Response(200, content=capability_catalog_bytes())


@lru_cache
def resolved_runtime_inputs() -> ResolvedRuntimeInputs:
    workspace = Path(__file__).resolve().parents[4]
    package = workspace / "tests/fixtures/packages/support-agent"
    return resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )


def runtime_requirement() -> ResolvedCapabilityRequirement:
    return resolved_runtime_inputs().resolved_agent.capability_requirements[0]


def ProviderHttpClient(  # noqa: N802
    base_url: str,
    *,
    transport: httpx.MockTransport | None = None,
    baggage_allowlist: frozenset[str] = frozenset(),
) -> NativeProviderHttpClient:
    return NativeProviderHttpClient(
        base_url,
        resolved_runtime_inputs(),
        transport=transport,
        baggage_allowlist=baggage_allowlist,
    )


def test_client_requires_frozen_resolved_runtime_inputs() -> None:
    with pytest.raises(TypeError):
        NativeProviderHttpClient("https://provider.test")  # type: ignore[call-arg]

    with pytest.raises(TypeError, match="ResolvedRuntimeInputs"):
        NativeProviderHttpClient(
            "https://provider.test",
            object(),  # type: ignore[arg-type]
        )


@pytest.mark.asyncio
async def test_unknown_invocation_identity_fails_before_transport() -> None:
    request = load_invocation_request(
        canonical_bytes(
            {
                "admissionId": "adm-1",
                "arguments": {},
                "capability": {
                    "name": "cases.comment",
                    "version": "1.0.0",
                },
                "evidenceRefs": {},
                "invocationId": "inv-1",
                "preconditions": {},
                "selectedResource": "tenant:support",
                "traceContext": {"traceparent": VALID_TRACEPARENT},
            }
        )
    )
    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(
            lambda request: pytest.fail("unknown capability reached transport")
        ),
    )
    try:
        with pytest.raises(ValueError, match="resolved runtime inputs"):
            await client.invoke(request)
    finally:
        await client.aclose()


@pytest.mark.asyncio
async def test_invoke_and_status_validate_with_the_indexed_locked_descriptor() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path in {
            "/v1/capability-invocations/cases.read",
            "/v1/capability-invocations/inv-1",
        }
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

    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(handler),
    )
    try:
        outcome = await client.invoke(invocation_request())
        status = await client.status("inv-1", runtime_requirement())
    finally:
        await client.aclose()

    assert outcome.result == {"caseId": "case-1"}
    assert status.result == {"caseId": "case-1"}


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "response",
    [
        {"invocation_id": "inv-1", "result": [], "status": "succeeded"},
        {"invocation_id": "inv-1", "status": "deferred"},
        {
            "error": {
                "category": "provider",
                "code": "PROVIDER_NATIVE_500",
                "message": "provider failed",
                "retry": "never",
            },
            "invocation_id": "inv-1",
            "status": "failed",
        },
    ],
)
async def test_invoke_rejects_responses_outside_the_locked_descriptor(
    response: dict[str, object],
) -> None:
    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(
            lambda request: httpx.Response(200, content=canonical_bytes(response))
        ),
    )
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.invoke(invocation_request())
    finally:
        await client.aclose()

    assert error.value.code == "KF-CAP-002"


@pytest.mark.asyncio
async def test_client_calls_only_the_four_v1_routes_with_native_values() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        if request.url.path == "/v1/capability-catalog":
            return httpx.Response(200, content=capability_catalog_bytes())
        if request.url.path == "/v1/capability-admissions":
            return httpx.Response(200, content=grant_set_bytes())
        if request.url.path == "/v1/capability-invocations/cases.read":
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
        if request.url.path == "/v1/capability-invocations/inv-1":
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
        status = await client.status("inv-1", runtime_requirement())
    finally:
        await client.aclose()

    assert isinstance(catalog, CapabilityCatalog)
    assert isinstance(grant_set, CapabilityGrantSet)
    assert isinstance(outcome, InvocationOutcome)
    assert isinstance(status, InvocationStatus)
    assert [(request.method, request.url.path) for request in seen] == [
        ("GET", "/v1/capability-catalog"),
        ("POST", "/v1/capability-admissions"),
        ("POST", "/v1/capability-invocations/cases.read"),
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
@pytest.mark.parametrize(
    ("field", "mismatch"),
    [
        ("actor", "actor:bob"),
        ("agent", "agent:other"),
        ("task", "task:other"),
        ("session", "session:other"),
    ],
)
async def test_admit_rejects_a_valid_grant_for_another_admission_identity(
    field: str,
    mismatch: str,
) -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            content=grant_set_bytes(**{field: mismatch}),
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.admit(admission_request())
    finally:
        await client.aclose()

    assert error.value.code == "KF-CAP-002"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "grants",
    [
        [
            {
                "capability": {
                    "name": "cases.close",
                    "version": "1.0.0",
                },
                "resources": ["tenant:t1/case:case-1"],
            }
        ],
        [
            {
                "capability": {
                    "name": "cases.comment",
                    "version": "1.0.0",
                },
                "resources": ["tenant:t1/case:*"],
            }
        ],
    ],
)
async def test_admit_rejects_unrequested_or_broader_grants(
    grants: list[dict[str, object]],
) -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            content=grant_set_bytes(grants=grants),
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.admit(admission_request())
    finally:
        await client.aclose()

    assert error.value.code == "KF-CAP-002"


@pytest.mark.asyncio
async def test_invoke_rejects_a_valid_outcome_for_another_invocation() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            content=canonical_bytes(
                {
                    "invocation_id": "inv-other",
                    "result": {"ok": True},
                    "status": "succeeded",
                }
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
async def test_status_rejects_a_valid_status_for_another_invocation() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            content=canonical_bytes(
                {
                    "invocation_id": "inv-other",
                    "status": "pending",
                }
            ),
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.status("inv-1", runtime_requirement())
    finally:
        await client.aclose()

    assert error.value.code == "KF-CAP-002"


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
async def test_compressed_response_is_rejected_before_decoding() -> None:
    seen_accept_encoding: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen_accept_encoding.append(request.headers["accept-encoding"])
        return httpx.Response(
            200,
            content=gzip.compress(b"x" * (PROVIDER_RESPONSE_LIMIT_BYTES + 1)),
            headers={"content-encoding": "gzip"},
        )

    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(handler),
    )
    try:
        with pytest.raises(ProviderTransportError, match="content encoding"):
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert seen_accept_encoding == ["identity"]


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
    assert json.loads(error.value.diagnostics_json)[0]["details"] == {}


@pytest.mark.asyncio
async def test_non_2xx_diagnostic_redacts_all_provider_free_form_values() -> None:
    secret = "provider-secret-value"
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            403,
            content=unsafe_diagnostic_envelope(secret),
        )
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.admit(admission_request())
    finally:
        await client.aclose()

    assert str(error.value) == "provider request failed"
    assert secret not in str(error.value)
    assert secret.encode() not in error.value.diagnostics_json
    assert json.loads(error.value.diagnostics_json) == [
        {
            "category": "authorization",
            "code": "KF-AUTH-001",
            "details": {},
            "help": None,
            "message": "provider request failed",
            "package_path": None,
            "retry": "after_user_action",
            "severity": "error",
            "source_range": None,
            "stage": "admit",
        }
    ]
    assert error.value.__cause__ is None
    assert error.value.__context__ is None


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
@pytest.mark.parametrize(
    "body",
    [
        b'{"diagnostics":[provider-secret-value',
        b"\xffprovider-secret-value",
    ],
)
async def test_malformed_diagnostic_does_not_retain_raw_exception(
    body: bytes,
) -> None:
    transport = httpx.MockTransport(lambda request: httpx.Response(500, content=body))
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(ProviderTransportError) as error:
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert "provider-secret-value" not in str(error.value)
    assert error.value.__cause__ is None
    assert error.value.__context__ is None


@pytest.mark.asyncio
async def test_unhashable_diagnostic_enum_is_totally_redacted() -> None:
    secret = "provider-secret-value"
    body = canonical_bytes(
        {
            "diagnostics": [
                {
                    "category": "authorization",
                    "code": [secret],
                    "details": {},
                    "help": None,
                    "message": secret,
                    "package_path": None,
                    "retry": "never",
                    "severity": "error",
                    "source_range": None,
                    "stage": "admit",
                }
            ]
        }
    )
    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(
            lambda request: httpx.Response(500, content=body)
        ),
    )
    try:
        with pytest.raises(ProviderTransportError) as error:
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert secret not in str(error.value)
    assert error.value.__cause__ is None
    assert error.value.__context__ is None
    assert not traceback_retains(error.value, secret)


@pytest.mark.asyncio
async def test_deeply_nested_diagnostic_is_totally_redacted() -> None:
    secret = "provider-secret-value"
    body = (
        b'{"diagnostics":'
        + b"[" * 1_100
        + canonical_bytes(secret)
        + b"]" * 1_100
        + b"}"
    )
    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(
            lambda request: httpx.Response(500, content=body)
        ),
    )
    try:
        with pytest.raises(ProviderTransportError) as error:
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert secret not in str(error.value)
    assert error.value.__cause__ is None
    assert error.value.__context__ is None
    assert not traceback_retains(error.value, secret)


@pytest.mark.asyncio
async def test_invalid_success_body_is_absent_from_public_traceback_locals() -> None:
    secret = "provider-secret-value"
    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(
            lambda request: httpx.Response(
                200,
                content=canonical_bytes({"secret": secret}),
            )
        ),
    )
    try:
        with pytest.raises(KiteframeDiagnosticError) as error:
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert error.value.code == "KF-CAP-002"
    assert secret not in str(error.value)
    assert secret.encode() not in error.value.diagnostics_json
    assert not traceback_retains(error.value, secret)


@pytest.mark.asyncio
async def test_httpx_failure_does_not_retain_raw_exception() -> None:
    def fail(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError(
            "provider-secret-value",
            request=request,
        )

    client = ProviderHttpClient(
        "https://provider.test",
        transport=httpx.MockTransport(fail),
    )
    try:
        with pytest.raises(ProviderTransportError) as error:
            await client.catalog(CatalogRequest.default())
    finally:
        await client.aclose()

    assert "provider-secret-value" not in str(error.value)
    assert error.value.__cause__ is None
    assert error.value.__context__ is None


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

    with pytest.raises(TypeError, match="MockTransport"):
        ProviderHttpClient(
            "http://provider.test",
            transport=httpx.AsyncHTTPTransport(),  # type: ignore[arg-type]
        )
    with pytest.raises(ValueError, match="HTTPS"):
        ProviderHttpClient(
            "ftp://provider.test",
            transport=httpx.MockTransport(lambda request: httpx.Response(500)),
        )


def test_arbitrary_async_transport_cannot_bypass_tls() -> None:
    with pytest.raises(TypeError, match="MockTransport"):
        ProviderHttpClient(
            "https://provider.test",
            transport=UnsafeTransport(),  # type: ignore[arg-type]
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


@pytest.mark.asyncio
@pytest.mark.parametrize("invocation_id", [".", ".."])
async def test_status_rejects_path_normalizing_dot_segments(
    invocation_id: str,
) -> None:
    transport = httpx.MockTransport(
        lambda request: pytest.fail("invalid status ID reached transport")
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(ValueError, match="invocation ID"):
            await client.status(invocation_id, runtime_requirement())
    finally:
        await client.aclose()


@pytest.mark.asyncio
@pytest.mark.parametrize("invocation_id", [" ", "\t"])
async def test_status_enforces_native_nonblank_invocation_id(
    invocation_id: str,
) -> None:
    transport = httpx.MockTransport(
        lambda request: pytest.fail("invalid status ID reached transport")
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    try:
        with pytest.raises(ValueError, match="invocation ID"):
            await client.status(invocation_id, runtime_requirement())
    finally:
        await client.aclose()


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


def test_trace_headers_reject_reserved_traceparent_version() -> None:
    with pytest.raises(ValueError, match="traceparent"):
        trace_headers(traceparent=f"ff-{VALID_TRACEPARENT[3:]}")


@pytest.mark.parametrize(
    "tracestate",
    [",", "vendor=", "Vendor=value", "a=1,a=2", "1vendor=value"],
)
def test_trace_headers_reject_invalid_tracestate(tracestate: str) -> None:
    with pytest.raises(ValueError, match="tracestate"):
        trace_headers(
            traceparent=VALID_TRACEPARENT,
            tracestate=tracestate,
        )


def test_v1_provider_api_does_not_expose_audit_sink() -> None:
    assert not hasattr(provider, "AuditSink")
