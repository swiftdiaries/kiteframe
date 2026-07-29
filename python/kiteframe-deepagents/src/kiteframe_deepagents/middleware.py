"""Fail-closed dynamic tool visibility for the Deep Agents adapter."""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass, field, replace
from typing import Any, Protocol, runtime_checkable

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
from .context import KiteframeSessionContext
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


@dataclass(frozen=True, slots=True, init=False)
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

    @classmethod
    def _from_compiled_children(
        cls,
        *,
        tool: BaseTool,
        declarations: tuple[ResolvedSubagent, ...],
        session: KiteframeSessionContext,
        compiled_child_names: tuple[str, ...],
        producer: SubAgentMiddleware,
    ) -> DeclaredChildTaskTool:
        binding = object.__new__(cls)
        object.__setattr__(binding, "tool", tool)
        object.__setattr__(binding, "declarations", declarations)
        object.__setattr__(binding, "session", session)
        object.__setattr__(
            binding,
            "compiled_child_names",
            compiled_child_names,
        )
        object.__setattr__(binding, "producer", producer)
        return binding

    def _is_valid(self) -> bool:
        if (
            not isinstance(self.tool, BaseTool)
            or self.tool.name != "task"
            or not isinstance(self.producer, SubAgentMiddleware)
            or len(self.producer.tools) != 1
            or self.producer.tools[0] is not self.tool
            or self.producer.subagent_names
            != frozenset(self.compiled_child_names)
        ):
            return False
        try:
            _validate_child_declarations(self.declarations)
        except (TypeError, ValueError):
            return False
        return sorted(self.compiled_child_names) == sorted(
            declaration.package_name for declaration in self.declarations
        )


def build_declared_child_task_tool(
    *,
    backend: BackendProtocol,
    compiled_children: tuple[CompiledSubAgent, ...],
    declarations: tuple[ResolvedSubagent, ...],
    session: KiteframeSessionContext,
) -> DeclaredChildTaskTool:
    """Build the public Deep Agents task tool and bind its native declarations."""

    if not isinstance(backend, BackendProtocol):
        raise TypeError("backend must implement BackendProtocol")
    _validate_compiled_children(compiled_children)
    _validate_child_declarations(declarations)
    if not isinstance(session, KiteframeSessionContext):
        raise TypeError("session must be KiteframeSessionContext")
    if sorted(child["name"] for child in compiled_children) != sorted(
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
        len(producer.tools) != 1
        or not isinstance(producer.tools[0], BaseTool)
        or producer.tools[0].name != "task"
    ):
        raise TypeError("compiled-child producer did not create task tool")
    return DeclaredChildTaskTool._from_compiled_children(
        tool=producer.tools[0],
        declarations=declarations,
        session=session,
        compiled_child_names=tuple(
            child["name"] for child in compiled_children
        ),
        producer=producer,
    )


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
        if not isinstance(self.session, KiteframeSessionContext):
            raise TypeError("session must be KiteframeSessionContext")
        if not isinstance(self.admitted_tools, tuple) or not all(
            isinstance(tool, CapabilityTool) for tool in self.admitted_tools
        ):
            raise TypeError(
                "admitted_tools must be a tuple of CapabilityTool"
            )
        if not isinstance(self.clock, SessionClock):
            raise TypeError("clock must provide session time")
        if self.declared_child_tool is not None and not isinstance(
            self.declared_child_tool,
            DeclaredChildTaskTool,
        ):
            raise TypeError(
                "declared_child_tool must be DeclaredChildTaskTool"
            )
        if (
            self.declared_child_tool is not None
            and self.declared_child_tool.session is not self.session
        ):
            raise ValueError(
                "declared child tool must bind the exact authority session"
            )
        if (
            self.declared_child_tool is not None
            and not self.declared_child_tool._is_valid()
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

        if not isinstance(session, KiteframeSessionContext):
            raise TypeError("session must be KiteframeSessionContext")
        if (
            session.actor != self.session.actor
            or session.session != self.session.session
            or session.task != self.session.task
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
        if any(tool.session is not session for tool in admitted_tools):
            raise ValueError(
                "admitted tools must bind the exact authority session"
            )
        grant_identities = sorted(
            (grant.name, grant.version) for grant in session.grants
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
            session=session,
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
            current = await self.authority_provider.current(self.session, now)
        except Exception:
            raise _policy_stale() from None
        if (
            not isinstance(current, KiteframeSessionContext)
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
            tools = await self.tool_registry.admitted_tools(session)
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
            and child_binding._is_valid()
            and child_binding.session is self.session
            and session.admission_id == self.session.admission_id
            and session.grant_digest == self.session.grant_digest
            and (
                session.authority_revisions.authority_revision_digest
                == self.session.authority_revisions.authority_revision_digest
            )
            and sorted(_grant_projection(grant) for grant in session.grants)
            == sorted(
                _grant_projection(grant) for grant in self.session.grants
            )
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
