"""Resolution-only entrypoint for the pinned Deep Agents runtime."""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Any, NoReturn, cast

from deepagents import create_deep_agent
from kiteframe import (
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
)
from langgraph.graph.state import CompiledStateGraph

from .compatibility import verify_compatibility
from .components import (
    RUNTIME_COMPONENT_UNRESOLVED,
    ValidatedComponents,
    _construction_error,
    _runtime_error,
    build_package_backend,
    validate_components,
)
from .context import KiteframeSessionContext
from .middleware import KiteframeGuardMiddleware
from .target import SUPPORTED_FEATURES, TARGET
from .tools import (
    CapabilitySuspensionBridge,
    IdempotencyCheckpointStore,
    PersistedIdempotencyKey,
    build_capability_tools,
)


@dataclass(frozen=True, slots=True)
class _SystemClock:
    def now(self) -> int:
        return int(time.time())


@dataclass(frozen=True, slots=True)
class _UnavailableCheckpointStore:
    async def persist_idempotency_key(
        self,
        record: PersistedIdempotencyKey,
    ) -> None:
        del record
        raise _runtime_error("idempotency checkpoint component is unresolved")


@dataclass(frozen=True, slots=True)
class _UnavailableSuspensionBridge:
    async def suspend(self, request: object, outcome: object) -> NoReturn:
        del request, outcome
        raise _runtime_error("capability suspension bridge is unresolved")


def _primary_model_symbol(inputs: ResolvedRuntimeInputs) -> str:
    for role, symbol in inputs.runtime_binding.model_symbols:
        if role == "primary":
            return symbol
    raise _runtime_error("primary model component metadata is unresolved")


def _system_prompt(inputs: ResolvedRuntimeInputs) -> str:
    prompts = inputs.resolved_agent.prompts
    if len(prompts) != 1:
        raise _runtime_error("root system prompt asset is unresolved")
    return prompts[0].text


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

    def compile(
        self,
        runtime_inputs: ResolvedRuntimeInputs,
        registry: FrozenComponentRegistry,
        session: KiteframeSessionContext,
    ) -> CompiledStateGraph:
        """Compile one closed runtime snapshot through the public constructor."""

        components = self.validate(runtime_inputs, registry)
        if not isinstance(session, KiteframeSessionContext):
            raise TypeError("session must be KiteframeSessionContext")

        resolved = runtime_inputs.resolved_agent
        if resolved.subagents:
            raise _runtime_error("declared child compilation is unresolved")
        try:
            verify_compatibility()
        except Exception:
            raise _runtime_error(
                "deep agents compatibility is unresolved"
            ) from None

        try:
            package_backend = build_package_backend(
                resolved.prompts,
                resolved.skills,
                components.package_backend,
            )
            checkpoint_store = (
                components.checkpointer
                if isinstance(
                    components.checkpointer,
                    IdempotencyCheckpointStore,
                )
                else _UnavailableCheckpointStore()
            )
            suspension_bridge = (
                components.checkpointer
                if isinstance(
                    components.checkpointer,
                    CapabilitySuspensionBridge,
                )
                else _UnavailableSuspensionBridge()
            )
            capability_tools = build_capability_tools(
                resolved.capability_requirements,
                session.grants,
                grant_digest=session.grant_digest,
                invoker=components.capability_provider,
                session=session,
                checkpoint_store=checkpoint_store,
                suspension_bridge=suspension_bridge,
            )
            guard = KiteframeGuardMiddleware(
                session=session,
                admitted_tools=capability_tools,
                clock=_SystemClock(),
            )
            system_prompt = _system_prompt(runtime_inputs)
            skills = package_backend.skill_sources(resolved.skills)
            model_symbol = _primary_model_symbol(runtime_inputs)
        except KiteframeDiagnosticError:
            raise
        except Exception:
            raise _runtime_error("runtime assembly validation failed") from None

        try:
            graph = create_deep_agent(
                model=components.primary_model,
                tools=capability_tools,
                system_prompt=system_prompt,
                middleware=(*components.middleware, guard),
                subagents=None,
                skills=skills or None,
                memory=None,
                permissions=None,
                backend=package_backend,
                interrupt_on=None,
                checkpointer=cast(Any, components.checkpointer),
                store=components.store,
                name=resolved.package_name,
            )
            if not isinstance(graph, CompiledStateGraph):
                raise TypeError("public constructor returned an invalid graph")
            return graph
        except Exception as error:
            raise _construction_error(
                model_symbol,
                type(error).__name__,
            ) from None


__all__ = ["DeepAgentsAdapter"]
