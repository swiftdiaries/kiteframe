"""Immutable per-session state shared by Kiteframe runtime guards."""

from __future__ import annotations

from dataclasses import dataclass

from kiteframe import (
    AuthorityRevisionSet,
    EffectiveCapabilityGrant,
    Suspension,
)


@dataclass(frozen=True, slots=True)
class KiteframeTraceContext:
    """Immutable W3C trace values carried into native request construction."""

    traceparent: str
    tracestate: str | None = None
    baggage: tuple[tuple[str, str], ...] = ()

    def __post_init__(self) -> None:
        if not isinstance(self.traceparent, str) or not self.traceparent:
            raise TypeError("traceparent must be a non-empty string")
        if self.tracestate is not None and not isinstance(
            self.tracestate,
            str,
        ):
            raise TypeError("tracestate must be a string or None")
        if not isinstance(self.baggage, tuple) or any(
            not isinstance(entry, tuple)
            or len(entry) != 2
            or not all(isinstance(value, str) for value in entry)
            for entry in self.baggage
        ):
            raise TypeError("baggage must be a tuple of string pairs")


@dataclass(frozen=True, slots=True)
class KiteframeSessionContext:
    """Authorization and trace state isolated to one agent session."""

    actor: str
    session: str
    task: str
    admission_id: str
    grant_digest: str
    grants: tuple[EffectiveCapabilityGrant, ...]
    authority_revisions: AuthorityRevisionSet
    trace_context: KiteframeTraceContext
    suspension: Suspension | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.grants, tuple) or not all(
            isinstance(grant, EffectiveCapabilityGrant)
            for grant in self.grants
        ):
            raise TypeError(
                "grants must be a tuple of native EffectiveCapabilityGrant"
            )
        if not isinstance(self.authority_revisions, AuthorityRevisionSet):
            raise TypeError(
                "authority_revisions must be native AuthorityRevisionSet"
            )
        if not isinstance(self.trace_context, KiteframeTraceContext):
            raise TypeError(
                "trace_context must be immutable KiteframeTraceContext"
            )
        if self.suspension is not None and not isinstance(
            self.suspension,
            Suspension,
        ):
            raise TypeError("suspension must be native Suspension or None")


__all__ = ["KiteframeSessionContext", "KiteframeTraceContext"]
