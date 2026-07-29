"""Resolution-only validation of deployment-owned runtime components."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Literal, Protocol, runtime_checkable

from deepagents.backends import BackendProtocol
from kiteframe import (
    CompilationReport,
    ComponentKind,
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
)
from kiteframe.registry import ComponentUnresolvedError
from langchain.agents.middleware import AgentMiddleware
from langchain_core.language_models import BaseChatModel
from langchain_core.runnables import RunnableConfig
from langgraph.checkpoint.base import CheckpointTuple
from langgraph.store.base import BaseStore

from .compatibility import (
    AMBIENT_TOOL_NAMES,
    DEEPAGENTS_VERSION,
    KiteframeHarnessProfileToken,
)

RUNTIME_COMPONENT_UNRESOLVED = "KF-RUNTIME-001"


@runtime_checkable
class DurableCheckpointer(Protocol):
    """The minimum restart-safe checkpointer attestation used by Kiteframe."""

    kiteframe_durable: Literal[True]

    async def aget_tuple(
        self,
        config: RunnableConfig,
    ) -> CheckpointTuple | None: ...


@dataclass(frozen=True, slots=True)
class ValidatedComponents:
    """The exact registry objects resolved for one immutable input snapshot."""

    models: tuple[tuple[str, BaseChatModel], ...]
    middleware: tuple[AgentMiddleware, ...]
    package_backend: BackendProtocol | object | None
    checkpointer: DurableCheckpointer | None
    store: BaseStore | None
    capability_provider: object
    audit_sink: object
    harness_profile: KiteframeHarnessProfileToken
    compilation_report: CompilationReport

    @property
    def primary_model(self) -> BaseChatModel:
        for role, model in self.models:
            if role == "primary":
                return model
        raise _runtime_error("primary model component is unresolved")


def _runtime_error(message: str) -> KiteframeDiagnosticError:
    error = KiteframeDiagnosticError(message)
    # PyO3 exception projections expose these attributes only on native raises.
    setattr(error, "code", RUNTIME_COMPONENT_UNRESOLVED)  # noqa: B010
    setattr(  # noqa: B010
        error,
        "diagnostics_json",
        json.dumps(
            [
                {
                    "category": "runtime",
                    "code": RUNTIME_COMPONENT_UNRESOLVED,
                    "details": {},
                    "help": None,
                    "message": message,
                    "package_path": None,
                    "retry": "never",
                    "severity": "error",
                    "source_range": None,
                    "stage": "compile",
                }
            ],
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode(),
    )
    return error


def _target_descriptors(
    inputs: ResolvedRuntimeInputs,
) -> dict[str, object]:
    return {descriptor.symbol: descriptor for descriptor in inputs.target_components}


def _require_descriptor(
    descriptors: dict[str, object],
    symbol: str,
    kind: ComponentKind,
) -> object:
    descriptor = descriptors.get(symbol)
    if descriptor is None or getattr(descriptor, "kind", None) != kind.value:
        raise _runtime_error(f"{kind.value} component metadata is unresolved")
    return descriptor


def _resolve(
    registry: FrozenComponentRegistry,
    kind: ComponentKind,
    symbol: str,
) -> object:
    try:
        return registry.resolve(kind, symbol)
    except ComponentUnresolvedError as error:
        raise _runtime_error(f"{kind.value} component is unresolved") from error


def _model_key(model: BaseChatModel) -> str:
    value = getattr(model, "model_name", None)
    if not isinstance(value, str) or not value:
        raise _runtime_error("model component has no stable model key")
    return value


def _requires_durable_checkpoint(inputs: ResolvedRuntimeInputs) -> bool:
    return any(
        "suspendable" in requirement.descriptor.execution_modes
        for requirement in inputs.resolved_agent.capability_requirements
    )


def validate_components(
    inputs: ResolvedRuntimeInputs,
    registry: FrozenComponentRegistry,
) -> ValidatedComponents:
    """Resolve and validate every trusted object before graph construction."""

    descriptors = _target_descriptors(inputs)
    binding = inputs.runtime_binding

    models: list[tuple[str, BaseChatModel]] = []
    for role, symbol in binding.model_symbols:
        _require_descriptor(descriptors, symbol, ComponentKind.MODEL)
        model = _resolve(registry, ComponentKind.MODEL, symbol)
        if not isinstance(model, BaseChatModel):
            raise _runtime_error("model component has an invalid public type")
        models.append((role, model))

    middleware: list[AgentMiddleware] = []
    for symbol in binding.middleware_symbols:
        _require_descriptor(descriptors, symbol, ComponentKind.MIDDLEWARE)
        component = _resolve(registry, ComponentKind.MIDDLEWARE, symbol)
        if not isinstance(component, AgentMiddleware):
            raise _runtime_error("middleware component has an invalid public type")
        middleware.append(component)

    package_backend: object | None = None
    if binding.backend is not None:
        _require_descriptor(descriptors, binding.backend, ComponentKind.BACKEND)
        package_backend = _resolve(registry, ComponentKind.BACKEND, binding.backend)

    checkpointer: DurableCheckpointer | None = None
    checkpointer_descriptor: object | None = None
    if binding.checkpointer is not None:
        checkpointer_descriptor = _require_descriptor(
            descriptors,
            binding.checkpointer,
            ComponentKind.CHECKPOINTER,
        )
        candidate = _resolve(
            registry,
            ComponentKind.CHECKPOINTER,
            binding.checkpointer,
        )
        if isinstance(candidate, DurableCheckpointer) and (
            candidate.kiteframe_durable is True
        ):
            checkpointer = candidate

    requires_durable = _requires_durable_checkpoint(inputs)
    descriptor_is_durable = (
        checkpointer_descriptor is not None
        and getattr(checkpointer_descriptor, "durable", False) is True
    )
    if requires_durable and (
        checkpointer is None or not descriptor_is_durable
    ):
        raise _runtime_error(
            "suspendable capability requires a durable checkpointer"
        )

    store: BaseStore | None = None
    capture = binding.content_capture
    if capture is not None and capture.enabled:
        symbol = capture.encrypted_content_store
        _require_descriptor(
            descriptors,
            symbol,
            ComponentKind.ENCRYPTED_CONTENT_STORE,
        )
        candidate = _resolve(
            registry,
            ComponentKind.ENCRYPTED_CONTENT_STORE,
            symbol,
        )
        if not isinstance(candidate, BaseStore):
            raise _runtime_error("store component has an invalid public type")
        store = candidate

    _require_descriptor(
        descriptors,
        binding.capability_provider,
        ComponentKind.CAPABILITY_PROVIDER,
    )
    capability_provider = _resolve(
        registry,
        ComponentKind.CAPABILITY_PROVIDER,
        binding.capability_provider,
    )
    _require_descriptor(
        descriptors,
        binding.audit_sink,
        ComponentKind.AUDIT_SINK,
    )
    audit_sink = _resolve(
        registry,
        ComponentKind.AUDIT_SINK,
        binding.audit_sink,
    )

    profile_symbol = binding.harness_profile
    if profile_symbol is None:
        raise _runtime_error("harness profile component metadata is unresolved")
    _require_descriptor(
        descriptors,
        profile_symbol,
        ComponentKind.HARNESS_PROFILE,
    )
    harness_profile = _resolve(
        registry,
        ComponentKind.HARNESS_PROFILE,
        profile_symbol,
    )
    primary = next(
        (model for role, model in models if role == "primary"),
        None,
    )
    if primary is None:
        raise _runtime_error("primary model component is unresolved")
    expected_profile = KiteframeHarnessProfileToken(
        model_key=_model_key(primary),
        deepagents_version=DEEPAGENTS_VERSION,
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent_disabled=True,
    )
    if (
        not isinstance(harness_profile, KiteframeHarnessProfileToken)
        or harness_profile != expected_profile
    ):
        raise _runtime_error("harness profile token does not match the model")

    return ValidatedComponents(
        models=tuple(models),
        middleware=tuple(middleware),
        package_backend=package_backend,
        checkpointer=checkpointer,
        store=store,
        capability_provider=capability_provider,
        audit_sink=audit_sink,
        harness_profile=harness_profile,
        compilation_report=inputs.compilation_report,
    )


__all__ = [
    "DurableCheckpointer",
    "ValidatedComponents",
    "validate_components",
]
