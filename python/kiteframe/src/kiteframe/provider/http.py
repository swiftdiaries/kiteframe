"""Strict async client for the Kiteframe V1 capability provider profile."""

import json
from collections.abc import Mapping
from typing import Any, Self
from urllib.parse import quote

import httpx

from kiteframe._native import (
    AdmissionRequest,
    CapabilityCatalog,
    CapabilityGrantSet,
    CatalogRequest,
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
    KiteframeDiagnosticError,
    load_capability_catalog,
    load_capability_grant_set,
    load_invocation_outcome,
    load_invocation_status,
)

from .trace import _is_sensitive_key, trace_headers

PROVIDER_RESPONSE_LIMIT_BYTES = 1_048_576
_DIAGNOSTIC_CODES = frozenset(
    {
        "KF-PKG-001",
        "KF-PKG-002",
        "KF-LOCK-001",
        "KF-LOCK-002",
        "KF-CAT-001",
        "KF-FEAT-001",
        "KF-AUTH-001",
        "KF-AUTH-002",
        "KF-AUTH-003",
        "KF-AUTH-004",
        "KF-CAP-001",
        "KF-CAP-002",
        "KF-CAP-003",
        "KF-AUDIT-001",
        "KF-RUNTIME-001",
        "KF-RUNTIME-002",
        "KF-CLI-001",
        "KF-CLI-002",
    }
)
_DIAGNOSTIC_CATEGORIES = frozenset(
    {
        "package",
        "lock",
        "catalog",
        "feature",
        "authorization",
        "capability",
        "audit",
        "runtime",
    }
)
_DIAGNOSTIC_STAGES = frozenset(
    {
        "parse",
        "validate",
        "lock",
        "resolve",
        "admit",
        "invoke",
        "audit",
        "runtime",
    }
)
_DIAGNOSTIC_STAGE_ORDER = {
    stage: index
    for index, stage in enumerate(
        (
            "parse",
            "validate",
            "lock",
            "resolve",
            "admit",
            "invoke",
            "audit",
            "runtime",
        )
    )
}
_DIAGNOSTIC_CODE_ORDER = {
    code: index
    for index, code in enumerate(
        (
            "KF-PKG-001",
            "KF-PKG-002",
            "KF-LOCK-001",
            "KF-LOCK-002",
            "KF-CAT-001",
            "KF-FEAT-001",
            "KF-AUTH-001",
            "KF-AUTH-002",
            "KF-AUTH-003",
            "KF-AUTH-004",
            "KF-CAP-001",
            "KF-CAP-002",
            "KF-CAP-003",
            "KF-AUDIT-001",
            "KF-RUNTIME-001",
            "KF-RUNTIME-002",
            "KF-CLI-001",
            "KF-CLI-002",
        )
    )
}
_DIAGNOSTIC_RETRIES = frozenset(
    {"never", "after_refresh", "after_user_action", "status_first"}
)


class ProviderTransportError(RuntimeError):
    """A redacted failure at the HTTP provider boundary."""


def require_https(base_url: str, *, allow_mock: bool = False) -> None:
    """Require a credential-free HTTPS origin unless a mock transport is used."""

    try:
        url = httpx.URL(base_url)
    except (httpx.InvalidURL, TypeError) as error:
        raise ValueError("provider base URL must be a valid HTTPS origin") from error

    if (
        not url.is_absolute_url
        or url.host is None
        or url.scheme not in ({"http", "https"} if allow_mock else {"https"})
    ):
        raise ValueError("provider base URL must use HTTPS")
    if url.username or url.password:
        raise ValueError("provider base URL must not contain credentials")
    if url.path not in ("", "/") or url.query or url.fragment:
        raise ValueError("provider base URL must be an origin without path or query")


def _canonical_json(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def _request_headers(
    request: CatalogRequest | AdmissionRequest | InvocationRequest,
    *,
    baggage_allowlist: frozenset[str],
) -> dict[str, str]:
    wire = json.loads(request.canonical_json())
    context = wire["traceContext"]
    return trace_headers(
        traceparent=request.traceparent,
        tracestate=request.tracestate,
        baggage=context.get("baggage", {}),
        baggage_allowlist=baggage_allowlist,
    )


async def _bounded_body(response: httpx.Response) -> bytes:
    content_length = response.headers.get("content-length")
    if content_length is not None:
        try:
            declared_length = int(content_length)
        except ValueError as error:
            raise ProviderTransportError(
                "provider returned an invalid content length"
            ) from error
        if declared_length < 0 or declared_length > PROVIDER_RESPONSE_LIMIT_BYTES:
            raise ProviderTransportError("provider response exceeded body limit")

    body = bytearray()
    async for chunk in response.aiter_bytes():
        if len(body) + len(chunk) > PROVIDER_RESPONSE_LIMIT_BYTES:
            raise ProviderTransportError("provider response exceeded body limit")
        body.extend(chunk)
    return bytes(body)


def _sanitize_detail(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: _sanitize_detail(child)
            for key, child in value.items()
            if isinstance(key, str) and not _is_sensitive_key(key)
        }
    if isinstance(value, list):
        return [_sanitize_detail(child) for child in value]
    return value


def _optional_string(
    diagnostic: Mapping[str, Any],
    key: str,
) -> str | None:
    value = diagnostic.get(key)
    if value is not None and not isinstance(value, str):
        raise ProviderTransportError("provider returned an invalid diagnostic response")
    return value


def _sanitize_diagnostic(diagnostic: object) -> dict[str, Any]:
    if not isinstance(diagnostic, dict):
        raise ProviderTransportError("provider returned an invalid diagnostic response")

    code = diagnostic.get("code")
    category = diagnostic.get("category")
    severity = diagnostic.get("severity")
    stage = diagnostic.get("stage")
    message = diagnostic.get("message")
    retry = diagnostic.get("retry")
    details = diagnostic.get("details")
    if (
        code not in _DIAGNOSTIC_CODES
        or category not in _DIAGNOSTIC_CATEGORIES
        or severity not in {"error", "warning"}
        or stage not in _DIAGNOSTIC_STAGES
        or not isinstance(message, str)
        or retry not in _DIAGNOSTIC_RETRIES
        or not isinstance(details, dict)
    ):
        raise ProviderTransportError("provider returned an invalid diagnostic response")

    source_range = diagnostic.get("source_range")
    if source_range is not None and (
        not isinstance(source_range, dict)
        or set(source_range) != {"start", "end"}
        or not isinstance(source_range["start"], int)
        or isinstance(source_range["start"], bool)
        or not isinstance(source_range["end"], int)
        or isinstance(source_range["end"], bool)
        or source_range["start"] < 0
        or source_range["end"] < source_range["start"]
        or source_range["end"] > 4_294_967_295
    ):
        raise ProviderTransportError("provider returned an invalid diagnostic response")

    sanitized: dict[str, Any] = {
        "category": category,
        "code": code,
        "details": _sanitize_detail(details),
        "help": _optional_string(diagnostic, "help"),
        "message": message,
        "package_path": _optional_string(diagnostic, "package_path"),
        "retry": retry,
        "severity": severity,
        "source_range": source_range,
        "stage": stage,
    }
    return sanitized


def _diagnostic_error(body: bytes) -> KiteframeDiagnosticError:
    def reject_non_json_constant(_: str) -> None:
        raise ValueError("non-JSON numeric constant")

    try:
        envelope = json.loads(
            body,
            parse_constant=reject_non_json_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise ProviderTransportError(
            "provider returned an invalid diagnostic response"
        ) from error

    raw_diagnostics = (
        envelope.get("diagnostics") if isinstance(envelope, dict) else envelope
    )
    if not isinstance(raw_diagnostics, list) or not raw_diagnostics:
        raise ProviderTransportError("provider returned an invalid diagnostic response")
    diagnostics = [_sanitize_diagnostic(diagnostic) for diagnostic in raw_diagnostics]
    diagnostics.sort(
        key=lambda diagnostic: (
            _DIAGNOSTIC_STAGE_ORDER[diagnostic["stage"]],
            diagnostic.get("package_path") is not None,
            diagnostic.get("package_path") or "",
            diagnostic.get("source_range") is not None,
            (diagnostic.get("source_range") or {}).get("start", 0),
            (diagnostic.get("source_range") or {}).get("end", 0),
            _DIAGNOSTIC_CODE_ORDER[diagnostic["code"]],
        )
    )
    error = KiteframeDiagnosticError(diagnostics[0]["message"])
    for attribute, value in (
        ("code", diagnostics[0]["code"]),
        ("diagnostics_json", _canonical_json(diagnostics)),
    ):
        setattr(error, attribute, value)
    return error


class ProviderHttpClient:
    """Strict client for the four standardized V1 provider routes."""

    def __init__(
        self,
        base_url: str,
        *,
        transport: httpx.AsyncBaseTransport | None = None,
        baggage_allowlist: frozenset[str] = frozenset(),
    ) -> None:
        require_https(
            base_url,
            allow_mock=isinstance(transport, httpx.MockTransport),
        )
        self._client = httpx.AsyncClient(
            base_url=base_url,
            follow_redirects=False,
            verify=True,
            transport=transport,
            timeout=httpx.Timeout(10.0),
            trust_env=False,
        )
        self._baggage_allowlist = frozenset(baggage_allowlist)

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: object,
        exc_value: object,
        traceback: object,
    ) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        await self._client.aclose()

    async def catalog(self, request: CatalogRequest) -> CapabilityCatalog:
        if not isinstance(request, CatalogRequest):
            raise TypeError("catalog request must be a native CatalogRequest")
        headers = _request_headers(
            request,
            baggage_allowlist=self._baggage_allowlist,
        )
        if request.known_catalog_digest is not None:
            headers["if-none-match"] = f'"{request.known_catalog_digest}"'
        body = await self._request(
            "GET",
            "/v1/capability-catalog",
            headers=headers,
        )
        return load_capability_catalog(body)

    async def admit(
        self,
        request: AdmissionRequest,
    ) -> CapabilityGrantSet:
        if not isinstance(request, AdmissionRequest):
            raise TypeError("admission request must be a native AdmissionRequest")
        body = await self._request(
            "POST",
            "/v1/capability-admissions",
            headers=_request_headers(
                request,
                baggage_allowlist=self._baggage_allowlist,
            ),
            content=request.canonical_json(),
        )
        return load_capability_grant_set(body)

    async def invoke(
        self,
        request: InvocationRequest,
    ) -> InvocationOutcome:
        if not isinstance(request, InvocationRequest):
            raise TypeError("invocation request must be a native InvocationRequest")
        name = quote(request.capability_name, safe=".")
        body = await self._request(
            "POST",
            f"/v1/capability-invocations/{name}",
            headers=_request_headers(
                request,
                baggage_allowlist=self._baggage_allowlist,
            ),
            content=request.canonical_json(),
        )
        return load_invocation_outcome(body)

    async def status(self, invocation_id: str) -> InvocationStatus:
        if (
            not isinstance(invocation_id, str)
            or not invocation_id
            or "\r" in invocation_id
            or "\n" in invocation_id
            or "\0" in invocation_id
        ):
            raise ValueError("invocation ID must be a non-empty string")
        encoded_invocation_id = quote(invocation_id, safe="")
        body = await self._request(
            "GET",
            f"/v1/capability-invocations/{encoded_invocation_id}",
        )
        return load_invocation_status(body)

    async def _request(
        self,
        method: str,
        route: str,
        *,
        headers: Mapping[str, str] | None = None,
        content: bytes | None = None,
    ) -> bytes:
        request_headers = {"accept": "application/json", **(headers or {})}
        if content is not None:
            request_headers["content-type"] = "application/json"
        try:
            async with self._client.stream(
                method,
                route,
                headers=request_headers,
                content=content,
            ) as response:
                if 300 <= response.status_code < 400:
                    raise ProviderTransportError(
                        "provider redirect-class response is forbidden"
                    )
                body = await _bounded_body(response)
        except httpx.HTTPError as error:
            raise ProviderTransportError("provider request failed") from error

        if not 200 <= response.status_code < 300:
            raise _diagnostic_error(body)
        return body


__all__ = [
    "PROVIDER_RESPONSE_LIMIT_BYTES",
    "ProviderHttpClient",
    "ProviderTransportError",
    "require_https",
]
