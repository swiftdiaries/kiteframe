"""Fail-closed dynamic tool visibility for the Deep Agents adapter."""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass, field, replace
from typing import Any, Protocol, runtime_checkable

from kiteframe import (
    AuthorityRevisionSet,
    EffectiveCapabilityGrant,
    KiteframeDiagnosticError,
)
from langchain.agents.middleware import (
    AgentMiddleware,
    ModelRequest,
    ToolCallRequest,
)
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


@dataclass(frozen=True, slots=True)
class KiteframeGuardMiddleware(AgentMiddleware):
    """Expose only tools backed by the complete current authority snapshot."""

    session: KiteframeSessionContext
    admitted_tools: tuple[CapabilityTool, ...]
    clock: SessionClock
    declared_child_tool: BaseTool | None = None
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
        if self.declared_child_tool is not None and (
            not isinstance(self.declared_child_tool, BaseTool)
            or self.declared_child_tool.name != "task"
        ):
            raise TypeError(
                "declared_child_tool must be the exact public task tool"
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
        grants: tuple[EffectiveCapabilityGrant, ...],
        authority_revisions: AuthorityRevisionSet,
        *,
        grant_digest: str | None = None,
        admitted_tools: tuple[CapabilityTool, ...] | None = None,
    ) -> KiteframeGuardMiddleware:
        """Return a new middleware and complete immutable session snapshot."""

        if not isinstance(grants, tuple) or not all(
            isinstance(grant, EffectiveCapabilityGrant) for grant in grants
        ):
            raise TypeError(
                "grants must be a tuple of native EffectiveCapabilityGrant"
            )
        if not isinstance(authority_revisions, AuthorityRevisionSet):
            raise TypeError(
                "authority_revisions must be native AuthorityRevisionSet"
            )
        session = replace(
            self.session,
            grants=grants,
            authority_revisions=authority_revisions,
            grant_digest=(
                self.session.grant_digest
                if grant_digest is None
                else grant_digest
            ),
        )
        return replace(
            self,
            session=session,
            admitted_tools=(
                self.admitted_tools
                if admitted_tools is None
                else admitted_tools
            ),
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

        child = self.declared_child_tool
        child_authority_matches = (
            session.admission_id == self.session.admission_id
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

        try:
            visible = await self._currently_invocable_tools()
        except KiteframeDiagnosticError:
            raise
        except Exception:
            raise _middleware_failed() from None
        name = request.tool_call.get("name")
        if not isinstance(name, str):
            raise _invocation_denied()
        tool = visible.get(name)
        if tool is None or request.tool is not tool:
            raise _invocation_denied()
        return await handler(request)


__all__ = [
    "AdmittedToolRegistry",
    "AuthorityProvider",
    "KiteframeGuardMiddleware",
    "SessionClock",
]
