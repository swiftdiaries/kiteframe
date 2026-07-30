"""Resolution-only entrypoint for the pinned Deep Agents runtime."""

from __future__ import annotations

import time
from copy import deepcopy
from dataclasses import dataclass, replace
from typing import Any, NoReturn, cast

from deepagents import CompiledSubAgent, create_deep_agent
from kiteframe import (
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
    ResolvedSubagent,
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
from .context import KiteframeSessionContext, _snapshot_session_context
from .delegation import (
    DeclaredSubAgentInput,
    DelegationAncestryEntry,
    _admission_denied,
    bind_child_admission,
    intersect_child_envelope,
)
from .middleware import (
    KiteframeGuardMiddleware,
    build_declared_child_task_tool,
)
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


@dataclass(frozen=True, slots=True)
class _PreparedAgent:
    runtime_inputs: ResolvedRuntimeInputs
    components: ValidatedComponents
    session: KiteframeSessionContext
    declaration: ResolvedSubagent | None
    ancestry: tuple[DelegationAncestryEntry, ...]
    children: tuple[_PreparedAgent, ...]


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
            raise TypeError("runtime_inputs must be native ResolvedRuntimeInputs")
        if not isinstance(registry, FrozenComponentRegistry):
            raise TypeError("registry must be a FrozenComponentRegistry")
        if runtime_inputs.runtime_target != TARGET:
            raise _runtime_error("runtime target is unsupported")

        unsupported = (
            set(runtime_inputs.resolved_agent.required_features) - SUPPORTED_FEATURES
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
        *,
        declared_children: tuple[DeclaredSubAgentInput, ...] = (),
    ) -> CompiledStateGraph:
        """Compile one closed runtime snapshot through the public constructor."""

        session_snapshot = _snapshot_session_context(session)
        prepared = self._prepare_agent(
            runtime_inputs,
            registry,
            session_snapshot,
            declared_children,
            declaration=None,
            ancestry=(),
            identities=(
                (
                    runtime_inputs.resolved_agent.package_name,
                    runtime_inputs.resolved_agent.resolved_digest,
                ),
            ),
        )
        try:
            verify_compatibility()
        except Exception:
            raise _runtime_error("deep agents compatibility is unresolved") from None
        return self._compile_prepared(prepared)

    def _prepare_agent(
        self,
        runtime_inputs: ResolvedRuntimeInputs,
        registry: FrozenComponentRegistry,
        session: KiteframeSessionContext,
        declared_children: tuple[DeclaredSubAgentInput, ...],
        *,
        declaration: ResolvedSubagent | None,
        ancestry: tuple[DelegationAncestryEntry, ...],
        identities: tuple[tuple[str, str], ...],
    ) -> _PreparedAgent:
        """Validate a complete declared tree before constructing any graph."""

        validated = self.validate(runtime_inputs, registry)
        try:
            isolated_backend = validated.package_backend
            if runtime_inputs.runtime_binding.backend is not None:
                isolated_backend = deepcopy(validated.package_backend)
                if isolated_backend is validated.package_backend:
                    raise TypeError(
                        "package backend did not create an isolated copy"
                    )
            components = replace(
                validated,
                middleware=tuple(
                    deepcopy(component) for component in validated.middleware
                ),
                package_backend=isolated_backend,
            )
        except Exception:
            raise _runtime_error("runtime session isolation is unresolved") from None
        if type(declared_children) is not tuple or not all(
            type(child) is DeclaredSubAgentInput for child in declared_children
        ):
            raise TypeError(
                "declared_children must be exact immutable DeclaredSubAgentInput values"
            )
        resolved = runtime_inputs.resolved_agent
        declarations = resolved.subagents
        declaration_names = tuple(child.package_name for child in declarations)
        supplied_names = tuple(
            child.declaration.package_name for child in declared_children
        )
        if len(declaration_names) != len(set(declaration_names)) or len(
            supplied_names
        ) != len(set(supplied_names)):
            raise _admission_denied()
        if not declarations:
            if declared_children:
                raise _admission_denied()
            return _PreparedAgent(
                runtime_inputs=runtime_inputs,
                components=components,
                session=session,
                declaration=declaration,
                ancestry=ancestry,
                children=(),
            )
        if not declared_children:
            raise _runtime_error("declared child compilation is unresolved")

        declaration_identities = tuple(
            (
                child.package_name,
                child.package_version,
                child.resolved_digest,
            )
            for child in declarations
        )
        supplied_identities = tuple(
            (
                child.declaration.package_name,
                child.declaration.package_version,
                child.declaration.resolved_digest,
            )
            for child in declared_children
        )
        if (
            len(declaration_identities) != len(set(declaration_identities))
            or len(supplied_identities) != len(set(supplied_identities))
            or sorted(declaration_identities) != sorted(supplied_identities)
        ):
            raise _admission_denied()
        supplied_by_identity = dict(
            zip(
                supplied_identities,
                declared_children,
                strict=True,
            )
        )

        prepared_children: list[_PreparedAgent] = []
        for child_declaration in declarations:
            declaration_identity = (
                child_declaration.package_name,
                child_declaration.package_version,
                child_declaration.resolved_digest,
            )
            child = supplied_by_identity[declaration_identity]
            child_resolved = child.runtime_inputs.resolved_agent
            child_identity = (
                child_declaration.package_name,
                child_declaration.resolved_digest,
            )
            if (
                child_resolved.package_name != child_declaration.package_name
                or child_resolved.resolved_digest != child_declaration.resolved_digest
                or child.admission_request.agent
                != f"agent:{child_declaration.package_name}"
                or child.admission_request.portable_digest
                != child_resolved.portable_digest
                or child.admission_request.lock_digest != child_resolved.lock_digest
                or child.admission_request.resolved_digest
                != child_resolved.resolved_digest
                or child.admission_request.catalog_name != child_resolved.catalog_name
                or child.admission_request.catalog_revision
                != child_resolved.catalog_revision
                or child.admission_request.catalog_digest
                != child_resolved.catalog_digest
                or child_identity in identities
                or child.session.actor != session.actor
                or child.session.session != session.session
                or child.session.task != session.task
            ):
                raise _admission_denied()
            envelope = intersect_child_envelope(
                parent=session.grants,
                delegation=child_declaration,
                child_requirements=child_resolved.capability_requirements,
                child_admission=child.session,
                ancestry=ancestry,
                parent_authority_revisions=session.authority_revisions,
                parent_agent=f"agent:{resolved.package_name}",
                child_agent=f"agent:{child_declaration.package_name}",
            )
            if envelope.authority_revisions is not child.session.authority_revisions:
                raise _admission_denied()
            correlated_session = bind_child_admission(
                child.session,
                child.admission_request,
                child.admission,
                envelope.ancestry,
            )
            prepared_children.append(
                self._prepare_agent(
                    child.runtime_inputs,
                    registry,
                    correlated_session,
                    child.children,
                    declaration=child_declaration,
                    ancestry=envelope.ancestry,
                    identities=(*identities, child_identity),
                )
            )
        return _PreparedAgent(
            runtime_inputs=runtime_inputs,
            components=components,
            session=session,
            declaration=declaration,
            ancestry=ancestry,
            children=tuple(prepared_children),
        )

    def _compile_prepared(
        self,
        prepared: _PreparedAgent,
    ) -> CompiledStateGraph:
        """Construct one already-validated tree from its leaves upward."""

        runtime_inputs = prepared.runtime_inputs
        components = prepared.components
        session_snapshot = prepared.session
        resolved = runtime_inputs.resolved_agent
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
                session_snapshot.grants,
                grant_digest=session_snapshot.grant_digest,
                invoker=components.capability_provider,
                session=session_snapshot,
                checkpoint_store=checkpoint_store,
                suspension_bridge=suspension_bridge,
            )
            compiled_children: list[CompiledSubAgent] = []
            for child in prepared.children:
                child_graph = self._compile_prepared(child)
                child_declaration = child.declaration
                if child_declaration is None:
                    raise TypeError("compiled child declaration is unresolved")
                description = _system_prompt(child.runtime_inputs).strip()
                if not description:
                    raise TypeError("compiled child description is unresolved")
                compiled_children.append(
                    {
                        "name": child_declaration.package_name,
                        "description": description,
                        "runnable": child_graph,
                    }
                )
            declared_child_tool = (
                build_declared_child_task_tool(
                    backend=package_backend,
                    compiled_children=tuple(compiled_children),
                    declarations=resolved.subagents,
                    session=session_snapshot,
                )
                if compiled_children
                else None
            )
            guard = KiteframeGuardMiddleware(
                session=session_snapshot,
                admitted_tools=capability_tools,
                clock=_SystemClock(),
                declared_child_tool=declared_child_tool,
            )
            system_prompt = _system_prompt(runtime_inputs)
            skills = package_backend.skill_sources(resolved.skills)
            model_symbol = _primary_model_symbol(runtime_inputs)
            public_tools = (
                (*capability_tools, declared_child_tool.tool)
                if declared_child_tool is not None
                else capability_tools
            )
        except KiteframeDiagnosticError:
            raise
        except Exception:
            raise _runtime_error("runtime assembly validation failed") from None

        try:
            graph = create_deep_agent(
                model=components.primary_model,
                tools=public_tools,
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
