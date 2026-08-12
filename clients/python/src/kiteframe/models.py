from __future__ import annotations

import math
import os
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from types import MappingProxyType
from typing import Any, Literal, TypeAlias, cast
from uuid import UUID

Primitive: TypeAlias = str | int | float | bool | None
Capability: TypeAlias = Literal[
    "mcp_unknown", "read_data", "write_data", "network", "filesystem"
]
PermissionChoice: TypeAlias = Literal[
    "allow-once", "allow-session", "deny-once", "deny-session"
]
RunState: TypeAlias = Literal[
    "runnable",
    "running",
    "waiting_for_answer",
    "waiting_for_permission",
    "outcome_unknown",
    "completed",
    "failed",
    "cancelled",
]


class KiteframeError(RuntimeError):
    """Base class for client, protocol, and bundled-harness failures."""


class BinaryError(KiteframeError):
    """The bundled harness cannot be resolved or inspected."""


class VersionMismatchError(BinaryError):
    """The client and harness versions aren't exactly compatible."""


class TransportError(KiteframeError):
    """An established ACP transport failed."""


class AuthenticationError(TransportError):
    def __init__(self, status_code: int, message: str) -> None:
        self.status_code = status_code
        super().__init__(message)


class RpcError(KiteframeError):
    def __init__(self, code: int, message: str, data: object | None = None) -> None:
        self.code = code
        self.data = data
        super().__init__(f"JSON-RPC {code}: {message}")


@dataclass(frozen=True)
class HarnessConfig:
    database_url: str
    principal: str
    responses_base_url: str
    responses_api_key: str = field(repr=False)
    responses_model: str
    working_directory: str | os.PathLike[str] = field(default_factory=Path.cwd)
    enable_acp_v2: bool = False
    startup_timeout: float = 10.0

    def __post_init__(self) -> None:
        required = {
            "database_url": self.database_url,
            "principal": self.principal,
            "responses_base_url": self.responses_base_url,
            "responses_api_key": self.responses_api_key,
            "responses_model": self.responses_model,
        }
        empty = [name for name, value in required.items() if not value]
        if empty:
            raise ValueError(f"HarnessConfig values must not be empty: {', '.join(empty)}")
        if self.startup_timeout <= 0:
            raise ValueError("startup_timeout must be positive")
        object.__setattr__(self, "working_directory", Path(self.working_directory).resolve())

    def environment(self) -> dict[str, str]:
        environment = os.environ.copy()
        environment.update(
            {
                "KITEFRAME_DATABASE_URL": self.database_url,
                "KITEFRAME_PRINCIPAL": self.principal,
                "KITEFRAME_RESPONSES_BASE_URL": self.responses_base_url,
                "KITEFRAME_RESPONSES_API_KEY": self.responses_api_key,
                "KITEFRAME_RESPONSES_MODEL": self.responses_model,
                "KITEFRAME_ENABLE_ACP_V2": "true" if self.enable_acp_v2 else "false",
            }
        )
        environment.pop("KITEFRAME_BIND", None)
        environment.pop("KITEFRAME_BEARER_TOKEN", None)
        environment.pop("KITEFRAME_BEARER_BINDINGS", None)
        return environment


@dataclass(frozen=True)
class OneOfPrimitive:
    values: tuple[Primitive, ...]

    def __post_init__(self) -> None:
        object.__setattr__(self, "values", tuple(self.values))
        if not self.values:
            raise ValueError("oneOf must contain at least one primitive")
        for value in self.values:
            if not isinstance(value, (str, int, float, bool)) and value is not None:
                raise TypeError("oneOf values must be JSON primitives")
            if isinstance(value, float) and not math.isfinite(value):
                raise ValueError("oneOf numbers must be finite")

    def to_json(self) -> list[Primitive]:
        return list(self.values)


@dataclass(frozen=True)
class CapabilityTarget:
    names: tuple[Capability, ...]

    def __post_init__(self) -> None:
        object.__setattr__(self, "names", _validated_names(self.names))
        supported = {"mcp_unknown", "read_data", "write_data", "network", "filesystem"}
        if any(name not in supported for name in self.names):
            raise ValueError("policy target contains an unsupported capability")

    def to_json(self) -> dict[str, object]:
        return {"type": "capability", "names": list(self.names)}


@dataclass(frozen=True)
class ToolTarget:
    names: tuple[str, ...]

    def __post_init__(self) -> None:
        object.__setattr__(self, "names", _validated_names(self.names))

    def to_json(self) -> dict[str, object]:
        return {"type": "tool", "names": list(self.names)}


def _validated_names(names: tuple[str, ...]) -> tuple[str, ...]:
    normalized = tuple(names)
    if not normalized or any(not name for name in normalized):
        raise ValueError("policy targets must contain non-empty names")
    if len(set(normalized)) != len(normalized):
        raise ValueError("policy target names must be unique")
    return normalized


@dataclass(frozen=True)
class PolicyRule:
    rule_id: str
    effect: Literal["permit", "forbid"]
    target: CapabilityTarget | ToolTarget
    input: Mapping[str, OneOfPrimitive] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.rule_id:
            raise ValueError("rule_id must not be empty")
        if self.effect not in ("permit", "forbid"):
            raise ValueError("effect must be permit or forbid")
        object.__setattr__(self, "input", MappingProxyType(dict(self.input)))

    def to_json(self) -> dict[str, object]:
        return {
            "rule_id": self.rule_id,
            "effect": self.effect,
            "target": self.target.to_json(),
            "input": {pointer: constraint.to_json() for pointer, constraint in self.input.items()},
        }


@dataclass(frozen=True)
class SessionPolicy:
    version: Literal[1]
    rules: tuple[PolicyRule, ...]

    def __post_init__(self) -> None:
        if self.version != 1:
            raise ValueError("only session policy version 1 is supported")
        object.__setattr__(self, "rules", tuple(self.rules))
        rule_ids = [rule.rule_id for rule in self.rules]
        if len(set(rule_ids)) != len(rule_ids):
            raise ValueError("session policy rule IDs must be unique")

    def to_json(self) -> dict[str, object]:
        return {"version": self.version, "rules": [rule.to_json() for rule in self.rules]}


@dataclass(frozen=True)
class PublishedWorkflow:
    id: UUID
    name: str
    version: int
    definition_hash: str

    @classmethod
    def from_json(cls, value: Mapping[str, Any]) -> PublishedWorkflow:
        return cls(
            id=UUID(str(value["id"])),
            name=str(value["name"]),
            version=int(value["version"]),
            definition_hash=str(value["definitionHash"]),
        )


@dataclass(frozen=True)
class Run:
    id: UUID
    session_id: UUID
    workflow_name: str
    version: int
    state: RunState
    revision: int

    @classmethod
    def from_json(cls, value: Mapping[str, Any]) -> Run:
        return cls(
            id=UUID(str(value["id"])),
            session_id=UUID(str(value["sessionId"])),
            workflow_name=str(value["workflowName"]),
            version=int(value["version"]),
            state=cast(RunState, value["state"]),
            revision=int(value["revision"]),
        )


@dataclass(frozen=True)
class RunEvent:
    sequence: int
    kind: str
    metadata: Mapping[str, Any]
    created_at: str

    @classmethod
    def from_json(cls, value: Mapping[str, Any]) -> RunEvent:
        metadata = value.get("metadata")
        if not isinstance(metadata, Mapping):
            raise TransportError("run event metadata must be an object")
        return cls(
            sequence=int(value["sequence"]),
            kind=str(value["kind"]),
            metadata=MappingProxyType(dict(metadata)),
            created_at=str(value["createdAt"]),
        )


@dataclass(frozen=True)
class EventPage:
    events: tuple[RunEvent, ...]
    cursor: int


@dataclass(frozen=True)
class PermissionOption:
    option_id: PermissionChoice
    name: str
    kind: str


@dataclass(frozen=True)
class PermissionRequest:
    session_id: UUID
    options: tuple[PermissionOption, ...]
    context: Mapping[str, Any]

    @classmethod
    def from_json(cls, value: Mapping[str, Any]) -> PermissionRequest:
        raw_options = value.get("options")
        if not isinstance(raw_options, list):
            raise TransportError("permission request options must be a list")
        options = tuple(
            PermissionOption(
                option_id=cast(PermissionChoice, option["optionId"]),
                name=str(option.get("name", option["optionId"])),
                kind=str(option.get("kind", "")),
            )
            for option in raw_options
            if isinstance(option, Mapping)
        )
        return cls(
            session_id=UUID(str(value["sessionId"])),
            options=options,
            context=MappingProxyType(dict(value)),
        )
