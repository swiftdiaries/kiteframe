"""Deployment-owned provider authentication values."""

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Literal, Protocol, runtime_checkable

ProviderOperation = Literal["catalog", "admit", "invoke", "status"]

_FORBIDDEN_CREDENTIAL_HEADERS = frozenset(
    {
        "accept",
        "accept-encoding",
        "baggage",
        "content-length",
        "content-type",
        "cookie",
        "host",
        "set-cookie",
        "traceparent",
        "tracestate",
    }
)
_HEADER_NAME_CHARACTERS = frozenset(
    "!#$%&'*+-.^_`|~0123456789abcdefghijklmnopqrstuvwxyz"
)


@dataclass(frozen=True, slots=True)
class ProviderAuthRequest:
    """Credential-free request metadata exposed to deployment authentication."""

    operation: ProviderOperation
    method: Literal["GET", "POST"]
    origin: str
    route: str


@runtime_checkable
class ProviderAuthenticator(Protocol):
    """Deployment protocol that supplies fresh credentials for one request."""

    async def credential_headers(
        self,
        request: ProviderAuthRequest,
    ) -> Mapping[str, str]: ...


def normalize_credential_header_allowlist(
    header_names: frozenset[str],
) -> frozenset[str]:
    """Normalize and validate deployment-configured credential header names."""

    normalized: set[str] = set()
    for header_name in header_names:
        candidate = _normalize_header_name(
            header_name,
            failure_message="credential header allowlist is invalid",
        )
        if candidate in normalized:
            raise ValueError("credential header allowlist contains duplicate names")
        if _is_forbidden(candidate):
            raise ValueError("credential header allowlist contains a forbidden header")
        normalized.add(candidate)
    return frozenset(normalized)


def normalize_credential_headers(
    headers: Mapping[str, str],
    *,
    allowlist: frozenset[str],
) -> dict[str, str]:
    """Validate one authenticator result without exposing credential values."""

    normalized: dict[str, str] = {}
    for header_name, value in headers.items():
        candidate = _normalize_header_name(
            header_name,
            failure_message="credential header name is invalid",
        )
        if candidate in normalized:
            raise ValueError("credential header contains duplicate names")
        if _is_forbidden(candidate):
            raise ValueError("credential header is forbidden")
        if candidate not in allowlist:
            raise ValueError("credential header is outside the explicit allowlist")
        if not isinstance(value, str) or any(
            character in value for character in "\r\n\0"
        ):
            raise ValueError("credential header value is invalid")
        try:
            value.encode("ascii")
        except UnicodeError:
            raise ValueError("credential header value is invalid") from None
        normalized[candidate] = value
    return normalized


def _normalize_header_name(header_name: object, *, failure_message: str) -> str:
    if not isinstance(header_name, str):
        raise ValueError(failure_message)
    try:
        normalized = header_name.encode("ascii").decode("ascii").lower()
    except UnicodeError:
        raise ValueError(failure_message) from None
    if not normalized or any(
        character not in _HEADER_NAME_CHARACTERS for character in normalized
    ):
        raise ValueError(failure_message)
    return normalized


def _is_forbidden(header_name: str) -> bool:
    return (
        header_name in _FORBIDDEN_CREDENTIAL_HEADERS
        or header_name.startswith("proxy-")
    )


__all__ = [
    "ProviderAuthRequest",
    "ProviderAuthenticator",
    "ProviderOperation",
]
