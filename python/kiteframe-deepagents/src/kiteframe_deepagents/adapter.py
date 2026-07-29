"""Resolution-only entrypoint for the pinned Deep Agents runtime."""

from __future__ import annotations

from kiteframe import (
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
)

from .components import (
    RUNTIME_COMPONENT_UNRESOLVED,
    ValidatedComponents,
    _runtime_error,
    validate_components,
)
from .target import SUPPORTED_FEATURES, TARGET


class DeepAgentsAdapter:
    """Validate closed Wave 3R inputs before any graph construction occurs."""

    @staticmethod
    def target() -> str:
        return TARGET

    @staticmethod
    def supported_features() -> frozenset[str]:
        return SUPPORTED_FEATURES

    def validate(
        self,
        runtime_inputs: ResolvedRuntimeInputs,
        registry: FrozenComponentRegistry,
    ) -> ValidatedComponents:
        if not isinstance(runtime_inputs, ResolvedRuntimeInputs):
            raise TypeError(
                "runtime_inputs must be native ResolvedRuntimeInputs"
            )
        if not isinstance(registry, FrozenComponentRegistry):
            raise TypeError("registry must be a FrozenComponentRegistry")
        if runtime_inputs.runtime_target != TARGET:
            raise _runtime_error("runtime target is unsupported")

        unsupported = (
            set(runtime_inputs.resolved_agent.required_features)
            - SUPPORTED_FEATURES
        )
        if unsupported:
            raise _runtime_error("required runtime feature is unsupported")

        try:
            return validate_components(runtime_inputs, registry)
        except KiteframeDiagnosticError as error:
            if getattr(error, "code", None) == RUNTIME_COMPONENT_UNRESOLVED:
                raise
            raise _runtime_error("runtime component validation failed") from error


__all__ = ["DeepAgentsAdapter"]
