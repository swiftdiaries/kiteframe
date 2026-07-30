"""Fail-closed dynamic tool visibility for the Deep Agents adapter."""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass, field, replace
from typing import Any, Protocol, TypeAlias, runtime_checkable
from weakref import WeakKeyDictionary

from deepagents import CompiledSubAgent, SubAgentMiddleware
from deepagents.backends import BackendProtocol
from kiteframe import (
    EffectiveCapabilityGrant,
    KiteframeDiagnosticError,
    ResolvedSubagent,
)
from langchain.agents.middleware import (
    AgentMiddleware,
    ModelRequest,
    ToolCallRequest,
)
from langchain_core.runnables import Runnable
from langchain_core.tools import BaseTool

from .compatibility import AMBIENT_TOOL_NAMES
from .context import KiteframeSessionContext, _snapshot_session_context
from .tools import CapabilityTool

AUTHORIZATION_POLICY_STALE = "KF-AUTH-004"
INVOCATION_DENIED = "KF-AUTH-003"
RUNTIME_COMPONENT_UNRESOLVED = "KF-RUNTIME-001"
RUNTIME_CONSTRUCTION_FAILED = "KF-RUNTIME-002"
FORBIDDEN_AMBIENT_TOOL_NAMES = AMBIENT_TOOL_NAMES | frozenset(
    {"http", "mcp", "task"}
)


@runtime_checkable
class SessionClock(Protocol):
    """Synchronous session-time source used at each middleware hook."""

    def now(self) -> int: ...


@runtime_checkable
class AuthorityProvider(Protocol):
    """Deployment boundary for a complete current authority snapshot."""

    async def current(
        self,
        session: KiteframeSessionContext,
        now: int,
    ) -> KiteframeSessionContext: ...


@runtime_checkable
class AdmittedToolRegistry(Protocol):
    """Deployment boundary for tools rebuilt from a current authority snapshot."""

    async def admitted_tools(
        self,
        session: KiteframeSessionContext,
    ) -> tuple[CapabilityTool, ...]: ...


def _diagnostic(
    code: str,
    *,
    category: str,
    message: str,
    stage: str,
) -> KiteframeDiagnosticError:
    error = KiteframeDiagnosticError(message)
    # Native exception attributes exist only when Rust creates the exception.
    setattr(error, "code", code)  # noqa: B010
    setattr(  # noqa: B010
        error,
        "diagnostics_json",
        json.dumps(
            [
                {
                    "category": category,
                    "code": code,
                    "details": {},
                    "help": None,
                    "message": message,
                    "package_path": None,
                    "retry": "never",
                    "severity": "error",
                    "source_range": None,
                    "stage": stage,
                }
            ],
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode(),
    )
    return error


def _policy_stale() -> KiteframeDiagnosticError:
    return _diagnostic(
        AUTHORIZATION_POLICY_STALE,
        category="authorization",
        message="current authorization policy could not be proven",
        stage="invoke",
    )


def _registry_unresolved() -> KiteframeDiagnosticError:
    return _diagnostic(
        RUNTIME_COMPONENT_UNRESOLVED,
        category="runtime",
        message="admitted capability tool registry is unresolved",
        stage="resolve",
    )


def _middleware_failed() -> KiteframeDiagnosticError:
    return _diagnostic(
        RUNTIME_CONSTRUCTION_FAILED,
        category="runtime",
        message="guard middleware failed",
        stage="runtime",
    )


def _invocation_denied() -> KiteframeDiagnosticError:
    return _diagnostic(
        INVOCATION_DENIED,
        category="authorization",
        message="capability invocation was denied",
        stage="invoke",
    )


def _comparable_json(value: Any) -> object:
    if isinstance(value, dict):
        return tuple(
            (key, _comparable_json(item))
            for key, item in sorted(value.items())
        )
    if isinstance(value, (list, tuple)):
        return tuple(_comparable_json(item) for item in value)
    return value


def _grant_projection(grant: EffectiveCapabilityGrant) -> tuple[object, ...]:
    return (
        grant.name,
        grant.version,
        grant.resources,
        grant.execution_modes,
        grant.maximum_effect,
        grant.expires_at,
        _comparable_json(grant.required_evidence),
        _comparable_json(grant.freshness),
        _comparable_json(grant.preconditions),
    )


def _suspension_projection(
    session: KiteframeSessionContext,
) -> tuple[str, str, str, str] | None:
    suspension = session.suspension
    if suspension is None:
        return None
    return (
        suspension.checkpoint_ref,
        suspension.evidence_kind,
        suspension.evidence_request_ref,
        suspension.proposal_digest,
    )


def _same_authority_session(
    left: KiteframeSessionContext,
    right: KiteframeSessionContext,
) -> bool:
    return (
        type(left) is KiteframeSessionContext
        and type(right) is KiteframeSessionContext
        and left.actor == right.actor
        and left.session == right.session
        and left.task == right.task
        and left.admission_id == right.admission_id
        and left.grant_digest == right.grant_digest
        and left.delegation_ancestry_digest
        == right.delegation_ancestry_digest
        and (
            left.authority_revisions.authority_revision_digest
            == right.authority_revisions.authority_revision_digest
        )
        and sorted(_grant_projection(grant) for grant in left.grants)
        == sorted(_grant_projection(grant) for grant in right.grants)
        and _suspension_projection(left) == _suspension_projection(right)
    )


def _validate_child_declarations(
    declarations: tuple[ResolvedSubagent, ...],
) -> None:
    if (
        not isinstance(declarations, tuple)
        or not declarations
        or not all(
            isinstance(declaration, ResolvedSubagent)
            for declaration in declarations
        )
    ):
        raise TypeError(
            "declarations must be a non-empty tuple of ResolvedSubagent"
        )
    identities = [
        (
            declaration.package_name,
            declaration.package_version,
            declaration.resolved_digest,
        )
        for declaration in declarations
    ]
    if len(identities) != len(set(identities)):
        raise ValueError("compiled child declarations must be unique")


def _validate_compiled_children(
    compiled_children: tuple[CompiledSubAgent, ...],
) -> None:
    if not isinstance(compiled_children, tuple) or not compiled_children:
        raise TypeError(
            "compiled_children must be a non-empty tuple of CompiledSubAgent"
        )
    required_keys = {"name", "description", "runnable"}
    names: list[str] = []
    for child in compiled_children:
        if not isinstance(child, dict) or set(child) != required_keys:
            raise TypeError(
                "each compiled child must contain exactly "
                "name, description, and runnable"
            )
        name = child["name"]
        description = child["description"]
        runnable = child["runnable"]
        if not isinstance(name, str) or not name:
            raise TypeError("compiled child name must be a non-empty string")
        if not isinstance(description, str) or not description:
            raise TypeError(
                "compiled child description must be a non-empty string"
            )
        if not isinstance(runnable, Runnable):
            raise TypeError("compiled child runnable must be Runnable")
        names.append(name)
    if len(names) != len(set(names)):
        raise ValueError("compiled child names must be unique")


@dataclass(
    frozen=True,
    slots=True,
    init=False,
    eq=False,
    weakref_slot=True,
)
class DeclaredChildTaskTool:
    """Trusted task tool bound to exact immutable compiled child declarations."""

    tool: BaseTool
    declarations: tuple[ResolvedSubagent, ...]
    session: KiteframeSessionContext
    compiled_child_names: tuple[str, ...] = field(repr=False)
    producer: SubAgentMiddleware = field(repr=False)

    def __init__(self, *_args: object, **_kwargs: object) -> None:
        raise TypeError(
            "DeclaredChildTaskTool requires the trusted compiled-child builder"
        )


_DeclaredChildProvenance: TypeAlias = tuple[
    BaseTool,
    tuple[ResolvedSubagent, ...],
    KiteframeSessionContext,
    tuple[str, ...],
    SubAgentMiddleware,
]


class _DeclaredChildTaskToolBuilder(Protocol):
    def __call__(
        self,
        *,
        backend: BackendProtocol,
        compiled_children: tuple[CompiledSubAgent, ...],
        declarations: tuple[ResolvedSubagent, ...],
        session: KiteframeSessionContext,
    ) -> DeclaredChildTaskTool: ...


def _declared_child_binding_boundary() -> tuple[
    _DeclaredChildTaskToolBuilder,
    Callable[[object], bool],
]:
    provenance_by_binding: WeakKeyDictionary[
        DeclaredChildTaskTool,
        _DeclaredChildProvenance,
    ] = WeakKeyDictionary()

    def builder(
        *,
        backend: BackendProtocol,
        compiled_children: tuple[CompiledSubAgent, ...],
        declarations: tuple[ResolvedSubagent, ...],
        session: KiteframeSessionContext,
    ) -> DeclaredChildTaskTool:
        """Build a public task tool and bind its native declarations."""

        if not isinstance(backend, BackendProtocol):
            raise TypeError("backend must implement BackendProtocol")
        _validate_compiled_children(compiled_children)
        _validate_child_declarations(declarations)
        if type(session) is not KiteframeSessionContext:
            raise TypeError("session must be exact KiteframeSessionContext")
        session_snapshot = _snapshot_session_context(session)
        compiled_child_names = tuple(
            child["name"] for child in compiled_children
        )
        if sorted(compiled_child_names) != sorted(
            declaration.package_name for declaration in declarations
        ):
            raise ValueError(
                "compiled child names must exactly match native declarations"
            )

        producer = SubAgentMiddleware(
            backend=backend,
            subagents=compiled_children,
        )
        if (
            type(producer) is not SubAgentMiddleware
            or len(producer.tools) != 1
            or not isinstance(producer.tools[0], BaseTool)
            or producer.tools[0].name != "task"
        ):
            raise TypeError("compiled-child producer did not create task tool")

        tool = producer.tools[0]
        binding = object.__new__(DeclaredChildTaskTool)
        object.__setattr__(binding, "tool", tool)
        object.__setattr__(binding, "declarations", declarations)
        object.__setattr__(binding, "session", session_snapshot)
        object.__setattr__(
            binding,
            "compiled_child_names",
            compiled_child_names,
        )
        object.__setattr__(binding, "producer", producer)
        provenance_by_binding[binding] = (
            tool,
            declarations,
            session_snapshot,
            compiled_child_names,
            producer,
        )
        return binding

    def verifier(candidate: object) -> bool:
        if type(candidate) is not DeclaredChildTaskTool:
            return False
        provenance = provenance_by_binding.get(candidate)
        if provenance is None:
            return False
        tool, declarations, session, compiled_child_names, producer = provenance
        if (
            candidate.tool is not tool
            or candidate.declarations is not declarations
            or candidate.session is not session
            or candidate.compiled_child_names is not compiled_child_names
            or candidate.producer is not producer
            or type(producer) is not SubAgentMiddleware
            or len(producer.tools) != 1
            or producer.tools[0] is not tool
            or tool.name != "task"
            or producer.subagent_names != frozenset(compiled_child_names)
        ):
            return False
        try:
            _validate_child_declarations(declarations)
        except (TypeError, ValueError):
            return False
        return sorted(compiled_child_names) == sorted(
            declaration.package_name for declaration in declarations
        )

    builder.__name__ = "build_declared_child_task_tool"
    builder.__qualname__ = "build_declared_child_task_tool"
    return builder, verifier


(
    build_declared_child_task_tool,
    _is_trusted_declared_child_binding,
) = _declared_child_binding_boundary()
del _declared_child_binding_boundary


@dataclass(frozen=True, slots=True)
class KiteframeGuardMiddleware(AgentMiddleware):
    """Expose only tools backed by the complete current authority snapshot."""

    session: KiteframeSessionContext
    admitted_tools: tuple[CapabilityTool, ...]
    clock: SessionClock
    declared_child_tool: DeclaredChildTaskTool | None = None
    authority_provider: AuthorityProvider | None = None
    tool_registry: AdmittedToolRegistry | None = None

    # Capability tools are registered through create_deep_agent(tools=...).
    tools: Sequence[BaseTool] = field(default=(), init=False)

    def __post_init__(self) -> None:
        if type(self.session) is not KiteframeSessionContext:
            raise TypeError("session must be exact KiteframeSessionContext")
        session_snapshot = _snapshot_session_context(self.session)
        object.__setattr__(self, "session", session_snapshot)
        if not isinstance(self.admitted_tools, tuple) or not all(
            isinstance(tool, CapabilityTool) for tool in self.admitted_tools
        ):
            raise TypeError(
                "admitted_tools must be a tuple of CapabilityTool"
            )
        if not isinstance(self.clock, SessionClock):
            raise TypeError("clock must provide session time")
        if self.declared_child_tool is not None and (
            type(self.declared_child_tool) is not DeclaredChildTaskTool
        ):
            raise TypeError(
                "declared_child_tool must be exact DeclaredChildTaskTool"
            )
        if (
            self.declared_child_tool is not None
            and not _same_authority_session(
                self.declared_child_tool.session,
                self.session,
            )
        ):
            raise ValueError(
                "declared child tool must bind the exact authority session"
            )
        if (
            self.declared_child_tool is not None
            and not _is_trusted_declared_child_binding(
                self.declared_child_tool
            )
        ):
            raise TypeError(
                "declared_child_tool must come from the trusted "
                "compiled-child builder"
            )
        if self.authority_provider is not None and not isinstance(
            self.authority_provider,
            AuthorityProvider,
        ):
            raise TypeError("authority_provider must provide current authority")
        if self.tool_registry is not None and not isinstance(
            self.tool_registry,
            AdmittedToolRegistry,
        ):
            raise TypeError("tool_registry must resolve admitted tools")

    def with_authority(
        self,
        session: KiteframeSessionContext,
        admitted_tools: tuple[CapabilityTool, ...],
    ) -> KiteframeGuardMiddleware:
        """Atomically replace one complete session and its rebuilt tools."""

        if type(session) is not KiteframeSessionContext:
            raise TypeError("session must be exact KiteframeSessionContext")
        session_snapshot = _snapshot_session_context(session)
        if (
            session_snapshot.actor != self.session.actor
            or session_snapshot.session != self.session.session
            or session_snapshot.task != self.session.task
        ):
            raise ValueError(
                "authority replacement must retain actor/session/task identity"
            )
        if not isinstance(admitted_tools, tuple) or not all(
            isinstance(tool, CapabilityTool) for tool in admitted_tools
        ):
            raise TypeError(
                "admitted_tools must be a tuple of CapabilityTool"
            )
        if any(
            not _same_authority_session(tool.session, session_snapshot)
            for tool in admitted_tools
        ):
            raise ValueError(
                "admitted tools must bind the exact authority session"
            )
        grant_identities = sorted(
            (grant.name, grant.version)
            for grant in session_snapshot.grants
        )
        tool_identities = sorted(
            (tool.requirement.name, tool.requirement.version)
            for tool in admitted_tools
        )
        if grant_identities != tool_identities:
            raise ValueError(
                "admitted tools must cover the exact authority grant set"
            )
        return replace(
            self,
            session=session_snapshot,
            admitted_tools=admitted_tools,
            declared_child_tool=None,
            authority_provider=None,
            tool_registry=None,
        )

    def _now(self) -> int:
        try:
            now = self.clock.now()
        except Exception:
            raise _middleware_failed() from None
        if not isinstance(now, int) or isinstance(now, bool) or now < 0:
            raise _middleware_failed()
        return now

    async def _current_session(self, now: int) -> KiteframeSessionContext:
        if self.authority_provider is None:
            return self.session
        try:
            provider_session = _snapshot_session_context(self.session)
            current = _snapshot_session_context(
                await self.authority_provider.current(provider_session, now)
            )
        except Exception:
            raise _policy_stale() from None
        if (
            type(current) is not KiteframeSessionContext
            or current.actor != self.session.actor
            or current.session != self.session.session
            or current.task != self.session.task
        ):
            raise _policy_stale()
        return current

    async def _current_tools(
        self,
        session: KiteframeSessionContext,
    ) -> tuple[CapabilityTool, ...]:
        if self.tool_registry is None:
            return self.admitted_tools
        try:
            registry_session = _snapshot_session_context(session)
            tools = await self.tool_registry.admitted_tools(registry_session)
        except Exception:
            raise _registry_unresolved() from None
        if not isinstance(tools, tuple) or not all(
            isinstance(tool, CapabilityTool) for tool in tools
        ):
            raise _registry_unresolved()
        return tools

    @staticmethod
    def _matches_current_authority(
        tool: CapabilityTool,
        session: KiteframeSessionContext,
        grants: dict[tuple[str, str], EffectiveCapabilityGrant],
        now: int,
    ) -> bool:
        tool_session = tool.session
        if (
            tool.name in FORBIDDEN_AMBIENT_TOOL_NAMES
            or tool_session.actor != session.actor
            or tool_session.session != session.session
            or tool_session.task != session.task
            or tool_session.admission_id != session.admission_id
            or tool_session.grant_digest != session.grant_digest
            or tool_session.delegation_ancestry_digest
            != session.delegation_ancestry_digest
            or (
                tool_session.authority_revisions.authority_revision_digest
                != session.authority_revisions.authority_revision_digest
            )
        ):
            return False
        identity = (tool.requirement.name, tool.requirement.version)
        if tool.name != identity[0] or (
            tool.grant.name,
            tool.grant.version,
        ) != identity:
            return False
        current = grants.get(identity)
        return (
            current is not None
            and current.expires_at > now
            and _grant_projection(current) == _grant_projection(tool.grant)
        )

    async def visible_tools(self, *, now: int) -> tuple[BaseTool, ...]:
        """Compute the exact model-visible tools for one model request."""

        if not isinstance(now, int) or isinstance(now, bool) or now < 0:
            raise _middleware_failed()
        session = await self._current_session(now)
        admitted_tools = await self._current_tools(session)
        if session.suspension is not None:
            return ()

        grants: dict[tuple[str, str], EffectiveCapabilityGrant] = {}
        for grant in session.grants:
            identity = (grant.name, grant.version)
            if identity in grants:
                raise _middleware_failed()
            grants[identity] = grant

        visible: list[BaseTool] = []
        names: set[str] = set()
        for tool in admitted_tools:
            if tool.name in names:
                raise _registry_unresolved()
            names.add(tool.name)
            if self._matches_current_authority(tool, session, grants, now):
                visible.append(tool)

        child_binding = self.declared_child_tool
        child = None if child_binding is None else child_binding.tool
        child_authority_matches = (
            child_binding is not None
            and _is_trusted_declared_child_binding(child_binding)
            and _same_authority_session(
                child_binding.session,
                self.session,
            )
            and _same_authority_session(session, self.session)
        )
        if child is not None and child_authority_matches:
            if child.name in names:
                raise _registry_unresolved()
            visible.append(child)
        return tuple(visible)

    async def _currently_invocable_tools(
        self,
    ) -> dict[str, BaseTool]:
        visible = await self.visible_tools(now=self._now())
        return {tool.name: tool for tool in visible}

    async def awrap_model_call(
        self,
        request: ModelRequest[Any],
        handler: Callable[[ModelRequest[Any]], Awaitable[Any]],
    ) -> Any:
        """Replace, never widen, the tools on each async model request."""

        try:
            visible = await self.visible_tools(now=self._now())
        except KiteframeDiagnosticError:
            raise
        except Exception:
            raise _middleware_failed() from None
        return await handler(request.override(tools=list(visible)))

    async def awrap_tool_call(
        self,
        request: ToolCallRequest,
        handler: Callable[[ToolCallRequest], Awaitable[Any]],
    ) -> Any:
        """Deny forged, hidden, expired, and same-name substituted calls."""

        tool_call = request.tool_call
        if not isinstance(tool_call, Mapping):
            raise _invocation_denied()
        name = tool_call.get("name")
        if not isinstance(name, str):
            raise _invocation_denied()
        try:
            visible = await self._currently_invocable_tools()
        except KiteframeDiagnosticError:
            raise
        except Exception:
            raise _middleware_failed() from None
        tool = visible.get(name)
        if tool is None:
            raise _invocation_denied()
        return await handler(request.override(tool=tool))


__all__ = [
    "AdmittedToolRegistry",
    "AuthorityProvider",
    "DeclaredChildTaskTool",
    "KiteframeGuardMiddleware",
    "SessionClock",
    "build_declared_child_task_tool",
]
