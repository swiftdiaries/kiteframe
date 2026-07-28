"""Fail-closed W3C trace-header projection for provider requests."""

import re
from collections.abc import Mapping
from urllib.parse import quote

_TRACEPARENT = re.compile(r"^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$")
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


def trace_headers(
    *,
    traceparent: str,
    tracestate: str | None = None,
    baggage: Mapping[str, str] | None = None,
    baggage_allowlist: frozenset[str] = frozenset(),
) -> dict[str, str]:
    """Return safe W3C headers, dropping unlisted or sensitive baggage."""

    if not _valid_traceparent(traceparent):
        raise ValueError("traceparent is not a valid W3C trace parent")

    headers = {"traceparent": traceparent}
    if tracestate is not None:
        if (
            not tracestate
            or _contains_header_break(tracestate)
            or len(tracestate.encode("utf-8")) > _MAX_TRACESTATE_BYTES
        ):
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
