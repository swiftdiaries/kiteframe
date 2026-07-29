"""Strict async client for the Kiteframe V1 capability provider profile."""

import json
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, Concatenate, ParamSpec, Self, TypeVar
from urllib.parse import quote

import httpx

from kiteframe._native import (
    AdmissionRequest,
    CapabilityGrantSet,
    CatalogFetchResult,
    CatalogRequest,
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
    KiteframeDiagnosticError,
    ResolvedCapabilityRequirement,
    ResolvedRuntimeInputs,
    StatusRequest,
    load_capability_catalog,
    load_capability_grant_set_for_request,
    load_invocation_outcome_for_request,
    load_invocation_status_for_request,
)

from .trace import trace_headers

PROVIDER_RESPONSE_LIMIT_BYTES = 1_048_576
_REDACTED_PROVIDER_MESSAGE = "provider request failed"
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
_LoaderParameters = ParamSpec("_LoaderParameters")
_Response = TypeVar("_Response")


@dataclass(frozen=True)
class _ProviderResponse:
    body: bytes
    status_code: int
    etag: str | None


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
    request: CatalogRequest | AdmissionRequest | InvocationRequest | StatusRequest,
    *,
    baggage_allowlist: frozenset[str],
) -> dict[str, str]:
    return trace_headers(
        traceparent=request.traceparent,
        tracestate=request.tracestate,
        baggage=request.baggage,
        baggage_allowlist=baggage_allowlist,
    )


async def _bounded_body(response: httpx.Response) -> bytes:
    content_encoding = response.headers.get("content-encoding")
    if content_encoding is not None and content_encoding.strip().lower() != "identity":
        raise ProviderTransportError("provider returned a forbidden content encoding")

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


def _load_native_response(
    loader: Callable[Concatenate[bytes, _LoaderParameters], _Response],
    body: bytes,
    *args: _LoaderParameters.args,
    **kwargs: _LoaderParameters.kwargs,
) -> _Response:
    try:
        return loader(body, *args, **kwargs)
    finally:
        body = b""


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
        not isinstance(code, str)
        or code not in _DIAGNOSTIC_CODES
        or not isinstance(category, str)
        or category not in _DIAGNOSTIC_CATEGORIES
        or not isinstance(severity, str)
        or severity not in {"error", "warning"}
        or not isinstance(stage, str)
        or stage not in _DIAGNOSTIC_STAGES
        or not isinstance(message, str)
        or not isinstance(retry, str)
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
        "details": {},
        "help": None,
        "message": _REDACTED_PROVIDER_MESSAGE,
        "package_path": None,
        "retry": retry,
        "severity": severity,
        "source_range": None,
        "stage": stage,
    }
    _optional_string(diagnostic, "help")
    _optional_string(diagnostic, "package_path")
    return sanitized


def _diagnostic_error(
    body: bytes,
) -> KiteframeDiagnosticError | ProviderTransportError:
    def reject_non_json_constant(_: str) -> None:
        raise ValueError("non-JSON numeric constant")

    try:
        envelope = json.loads(
            body,
            parse_constant=reject_non_json_constant,
        )
        raw_diagnostics = (
            envelope.get("diagnostics") if isinstance(envelope, dict) else envelope
        )
        if not isinstance(raw_diagnostics, list) or not raw_diagnostics:
            raise ProviderTransportError(
                "provider returned an invalid diagnostic response"
            )
        diagnostics = [
            _sanitize_diagnostic(diagnostic) for diagnostic in raw_diagnostics
        ]
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
        error = KiteframeDiagnosticError(_REDACTED_PROVIDER_MESSAGE)
        for attribute, value in (
            ("code", diagnostics[0]["code"]),
            ("diagnostics_json", _canonical_json(diagnostics)),
        ):
            setattr(error, attribute, value)
        return error
    except Exception:
        return ProviderTransportError(
            "provider returned an invalid diagnostic response"
        )
    finally:
        body = b""


def _requirement_digests(
    requirement: ResolvedCapabilityRequirement,
) -> tuple[str, str, str, str, str]:
    return (
        requirement.descriptor_digest,
        requirement.input_schema_digest,
        requirement.output_schema_digest,
        requirement.stable_error_set_digest,
        requirement.safety_metadata_digest,
    )


class ProviderHttpClient:
    """Strict client for the four standardized V1 provider routes."""

    def __init__(
        self,
        base_url: str,
        resolved_runtime_inputs: ResolvedRuntimeInputs,
        *,
        transport: httpx.MockTransport | None = None,
        baggage_allowlist: frozenset[str] = frozenset(),
    ) -> None:
        if not isinstance(resolved_runtime_inputs, ResolvedRuntimeInputs):
            raise TypeError(
                "resolved_runtime_inputs must be native ResolvedRuntimeInputs"
            )
        if transport is not None and not isinstance(
            transport,
            httpx.MockTransport,
        ):
            raise TypeError("transport must be an httpx.MockTransport")
        require_https(
            base_url,
            allow_mock=transport is not None,
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
        requirements: dict[
            tuple[str, str],
            ResolvedCapabilityRequirement,
        ] = {}
        for requirement in (
            resolved_runtime_inputs.resolved_agent.capability_requirements
        ):
            identity = (requirement.name, requirement.version)
            if identity in requirements:
                raise ValueError(
                    "resolved runtime inputs contain a duplicate capability identity"
                )
            requirements[identity] = requirement
        self._requirements = MappingProxyType(requirements)

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

    async def catalog(self, request: CatalogRequest) -> CatalogFetchResult:
        if not isinstance(request, CatalogRequest):
            raise TypeError("catalog request must be a native CatalogRequest")
        headers = _request_headers(
            request,
            baggage_allowlist=self._baggage_allowlist,
        )
        if request.known_catalog_digest is not None:
            headers["if-none-match"] = f'"{request.known_catalog_digest}"'
        response = await self._request(
            "GET",
            "/v1/capability-catalog",
            headers=headers,
            allow_not_modified=True,
        )
        if response.status_code == 304:
            if request.known_catalog_digest is None:
                raise ProviderTransportError("provider returned unsolicited not-modified")
            if response.body:
                raise ProviderTransportError(
                    "provider returned a non-empty not-modified response"
                )
            if response.etag != request.known_catalog_digest:
                raise ProviderTransportError("provider not-modified digest mismatch")
            return CatalogFetchResult.not_modified(request)

        if response.status_code != 200:
            raise ProviderTransportError("provider catalog response must use HTTP 200")

        catalog = _load_native_response(load_capability_catalog, response.body)
        if response.etag != catalog.catalog_digest:
            raise ProviderTransportError("provider catalog digest mismatch")
        return CatalogFetchResult.modified(catalog)

    async def admit(
        self,
        request: AdmissionRequest,
    ) -> CapabilityGrantSet:
        if not isinstance(request, AdmissionRequest):
            raise TypeError("admission request must be a native AdmissionRequest")
        response = await self._request(
            "POST",
            "/v1/capability-admissions",
            headers=_request_headers(
                request,
                baggage_allowlist=self._baggage_allowlist,
            ),
            content=request.canonical_json(),
        )
        return _load_native_response(
            load_capability_grant_set_for_request,
            response.body,
            request,
        )

    async def invoke(
        self,
        request: InvocationRequest,
    ) -> InvocationOutcome:
        if not isinstance(request, InvocationRequest):
            raise TypeError("invocation request must be a native InvocationRequest")
        identity = (request.capability_name, request.capability_version)
        requirement = self._requirements.get(identity)
        if requirement is None:
            raise ValueError(
                "invocation capability is not present in resolved runtime inputs"
            )
        name = quote(request.capability_name, safe=".")
        response = await self._request(
            "POST",
            f"/v1/capability-invocations/{name}",
            headers=_request_headers(
                request,
                baggage_allowlist=self._baggage_allowlist,
            ),
            content=request.canonical_json(),
        )
        return _load_native_response(
            load_invocation_outcome_for_request,
            response.body,
            request,
            requirement,
        )

    async def status(
        self,
        request: StatusRequest,
        requirement: ResolvedCapabilityRequirement,
    ) -> InvocationStatus:
        if not isinstance(request, StatusRequest):
            raise TypeError("status request must be a native StatusRequest")
        # Native validation owns the nonblank invariant. The adapter additionally
        # rejects RFC 3986 dot segments that HTTPX would normalize outside the
        # fixed invocation route.
        if request.invocation_id in {".", ".."}:
            raise ValueError("invocation ID cannot be an HTTP path dot segment")
        if not isinstance(requirement, ResolvedCapabilityRequirement):
            raise TypeError(
                "status requirement must be a native ResolvedCapabilityRequirement"
            )
        indexed = self._requirements.get((requirement.name, requirement.version))
        if indexed is None or _requirement_digests(indexed) != _requirement_digests(
            requirement
        ):
            raise ValueError(
                "status requirement is not present in resolved runtime inputs"
            )
        encoded_invocation_id = quote(request.invocation_id, safe="")
        response = await self._request(
            "GET",
            f"/v1/capability-invocations/{encoded_invocation_id}",
            headers=_request_headers(
                request,
                baggage_allowlist=self._baggage_allowlist,
            ),
        )
        return _load_native_response(
            load_invocation_status_for_request,
            response.body,
            request,
            indexed,
        )

    async def _request(
        self,
        method: str,
        route: str,
        *,
        headers: Mapping[str, str] | None = None,
        content: bytes | None = None,
        allow_not_modified: bool = False,
    ) -> _ProviderResponse:
        request_headers = {
            "accept": "application/json",
            "accept-encoding": "identity",
            **(headers or {}),
        }
        if content is not None:
            request_headers["content-type"] = "application/json"
        body = b""
        status_code: int | None = None
        etag: str | None = None
        failure_message: str | None = None
        response: httpx.Response | None = None
        try:
            async with self._client.stream(
                method,
                route,
                headers=request_headers,
                content=content,
            ) as response:
                status_code = response.status_code
                etag = response.headers.get("etag")
                if not 300 <= status_code < 400 or status_code == 304:
                    body = await _bounded_body(response)
        except httpx.HTTPError:
            failure_message = "provider request failed"
        except ProviderTransportError as error:
            failure_message = str(error)
        response = None

        if failure_message is not None:
            body = b""
            content = None
            raise ProviderTransportError(failure_message)
        if status_code is None:
            body = b""
            content = None
            raise ProviderTransportError("provider request failed")
        if 300 <= status_code < 400 and not (
            allow_not_modified and status_code == 304
        ):
            content = None
            raise ProviderTransportError(
                "provider redirect-class response is forbidden"
            )
        if status_code != 304 and not 200 <= status_code < 300:
            diagnostic_error = _diagnostic_error(body)
            body = b""
            content = None
            raise diagnostic_error
        return _ProviderResponse(body=body, status_code=status_code, etag=etag)


__all__ = [
    "PROVIDER_RESPONSE_LIMIT_BYTES",
    "ProviderHttpClient",
    "ProviderTransportError",
    "require_https",
]
