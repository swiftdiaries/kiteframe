"""Fail-closed W3C trace-header projection for provider requests."""

import re
from collections.abc import Mapping
from urllib.parse import quote

_TRACEPARENT = re.compile(r"^00-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$")
_TRACESTATE_SIMPLE_KEY = re.compile(r"^[a-z0-9][a-z0-9_\-*/]{0,255}$")
_TRACESTATE_TENANT_KEY = re.compile(
    r"^[a-z0-9][a-z0-9_\-*/]{0,240}@[a-z0-9][a-z0-9_\-*/]{0,13}$"
)
_TRACESTATE_VALUE = re.compile(
    r"^[\x21-\x2b\x2d-\x3c\x3e-\x7e]"
    r"[\x20-\x2b\x2d-\x3c\x3e-\x7e]{0,254}"
    r"[\x21-\x2b\x2d-\x3c\x3e-\x7e]$"
    r"|^[\x21-\x2b\x2d-\x3c\x3e-\x7e]$"
)
_BAGGAGE_KEY = re.compile(r"^[A-Za-z0-9!#$%&'*+\-.^_`|~]+$")
_SENSITIVE_KEY_PARTS = (
    "argument",
    "authorization",
    "credential",
    "password",
    "prompt",
    "result",
    "secret",
    "token",
    "tuple",
)
_MAX_TRACESTATE_BYTES = 512
_MAX_BAGGAGE_BYTES = 8_192


def _contains_header_break(value: str) -> bool:
    return "\r" in value or "\n" in value or "\0" in value


def _is_sensitive_key(key: str) -> bool:
    normalized = key.casefold().replace("-", "_").replace(".", "_")
    return any(part in normalized for part in _SENSITIVE_KEY_PARTS)


def _valid_traceparent(value: str) -> bool:
    if _TRACEPARENT.fullmatch(value) is None:
        return False
    _, trace_id, parent_id, _ = value.split("-")
    return trace_id != "0" * 32 and parent_id != "0" * 16


def _valid_tracestate(value: str) -> bool:
    try:
        encoded = value.encode("ascii")
    except UnicodeEncodeError:
        return False
    if not value or len(encoded) > _MAX_TRACESTATE_BYTES:
        return False

    members = value.split(",")
    if len(members) > 32:
        return False
    seen: set[str] = set()
    for member in members:
        if member.count("=") != 1 or member.strip() != member:
            return False
        key, member_value = member.split("=", 1)
        if (
            key in seen
            or (
                _TRACESTATE_SIMPLE_KEY.fullmatch(key) is None
                and _TRACESTATE_TENANT_KEY.fullmatch(key) is None
            )
            or _TRACESTATE_VALUE.fullmatch(member_value) is None
        ):
            return False
        seen.add(key)
    return True


def trace_headers(
    *,
    traceparent: str,
    tracestate: str | None = None,
    baggage: Mapping[str, str] | None = None,
    baggage_allowlist: frozenset[str] = frozenset(),
) -> dict[str, str]:
    """Return safe W3C version-00 headers and canonical tracestate members.

    The accepted tracestate contract is intentionally narrower than the W3C
    receive grammar: it requires a non-empty comma-separated ``key=value``
    list without optional whitespace or empty members.
    """

    if not _valid_traceparent(traceparent):
        raise ValueError("traceparent is not a valid W3C trace parent")

    headers = {"traceparent": traceparent}
    if tracestate is not None:
        if _contains_header_break(tracestate) or not _valid_tracestate(tracestate):
            raise ValueError("tracestate is not a valid W3C trace state")
        headers["tracestate"] = tracestate

    entries: list[str] = []
    for key, value in sorted((baggage or {}).items()):
        if (
            key not in baggage_allowlist
            or _is_sensitive_key(key)
            or _BAGGAGE_KEY.fullmatch(key) is None
            or not isinstance(value, str)
            or _contains_header_break(value)
        ):
            continue
        encoded_value = quote(value, safe="!#$&'*+-.^_`|~:/?@")
        candidate = ",".join([*entries, f"{key}={encoded_value}"])
        if len(candidate.encode("utf-8")) > _MAX_BAGGAGE_BYTES:
            continue
        entries.append(f"{key}={encoded_value}")

    if entries:
        headers["baggage"] = ",".join(entries)
    return headers


__all__ = ["trace_headers"]
