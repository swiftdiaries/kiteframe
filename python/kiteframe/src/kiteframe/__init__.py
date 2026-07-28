"""Immutable Python projections of Rust-validated Kiteframe values."""

from ._native import (
    KiteframeDiagnosticError,
    ResolvedAgent,
    ResolvedCapabilityRequirement,
    ResolvedSubagent,
    load_resolved_agent,
    resolve_package,
)
from .registry import (
    ComponentKind,
    ComponentRegistry,
    ComponentUnresolvedError,
    FrozenComponentRegistry,
)

__all__ = [
    "KiteframeDiagnosticError",
    "ComponentKind",
    "ComponentRegistry",
    "ComponentUnresolvedError",
    "FrozenComponentRegistry",
    "ResolvedAgent",
    "ResolvedCapabilityRequirement",
    "ResolvedSubagent",
    "load_resolved_agent",
    "resolve_package",
]
