"""Immutable per-session state shared by Kiteframe runtime guards."""

from __future__ import annotations

from dataclasses import dataclass

from kiteframe import AuthorityRevisionSet, EffectiveCapabilityGrant


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
    trace_context: object
    suspension: object | None = None


__all__ = ["KiteframeSessionContext"]
