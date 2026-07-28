"""Deployment-owned component registration for runtime adapters."""

from dataclasses import dataclass
from enum import Enum
from types import MappingProxyType
from typing import Mapping


COMPONENT_UNRESOLVED = "KF-RUNTIME-001"


class ComponentKind(str, Enum):
    """The Rust-owned component kinds that a binding may reference."""

    MODEL = "model"
    MIDDLEWARE = "middleware"
    BACKEND = "backend"
    CHECKPOINTER = "checkpointer"
    CAPABILITY_PROVIDER = "capability_provider"
    AUDIT_SINK = "audit_sink"
    REDACTION_POLICY = "redaction_policy"
    RETENTION_POLICY = "retention_policy"
    ACCESS_POLICY = "access_policy"
    ENCRYPTED_CONTENT_STORE = "encrypted_content_store"


class ComponentUnresolvedError(RuntimeError):
    """A deployment registry cannot satisfy a binding component reference."""

    code = COMPONENT_UNRESOLVED

    def __init__(self) -> None:
        super().__init__("runtime component is unresolved")


class DuplicateComponentRegistrationError(ValueError):
    """A deployment attempted to replace an existing component symbol."""

    code = COMPONENT_UNRESOLVED


@dataclass(frozen=True, slots=True)
class RegistryKey:
    kind: ComponentKind
    symbol: str


def validate_registry_symbol(symbol: str) -> str:
    """Validate the same symbol grammar as Rust's ``RegistrySymbol``."""
    if not isinstance(symbol, str) or not symbol:
        raise ValueError("invalid RegistrySymbol")

    previous_separator = False
    for index, character in enumerate(symbol):
        if character.isascii() and character.islower() and character.isalpha():
            previous_separator = False
        elif character.isascii() and character.isdigit() and index > 0:
            previous_separator = False
        elif character in "._-" and index > 0 and not previous_separator:
            previous_separator = True
        else:
            raise ValueError("invalid RegistrySymbol")

    if previous_separator:
        raise ValueError("invalid RegistrySymbol")
    return symbol


class ComponentRegistry:
    """Mutable deployment configuration that becomes immutable before compile."""

    def __init__(self) -> None:
        self._entries: dict[RegistryKey, object] = {}
        self._symbols: dict[str, ComponentKind] = {}
        self._frozen = False

    def register(self, kind: ComponentKind, symbol: str, value: object) -> None:
        if self._frozen:
            raise RuntimeError("component registry is frozen")

        symbol = validate_registry_symbol(symbol)
        if symbol in self._symbols:
            raise DuplicateComponentRegistrationError(
                f"registry symbol {symbol!r} is already registered"
            )

        key = RegistryKey(kind, symbol)
        self._entries[key] = value
        self._symbols[symbol] = kind

    def freeze(self) -> "FrozenComponentRegistry":
        self._frozen = True
        return FrozenComponentRegistry(
            MappingProxyType(dict(self._entries)),
            MappingProxyType(dict(self._symbols)),
        )


@dataclass(frozen=True, slots=True, init=False)
class FrozenComponentRegistry:
    """An immutable, instance-scoped snapshot for runtime construction."""

    _entries: Mapping[RegistryKey, object]
    _symbols: Mapping[str, ComponentKind]

    def __init__(
        self,
        entries: Mapping[RegistryKey, object],
        symbols: Mapping[str, ComponentKind],
    ) -> None:
        object.__setattr__(self, "_entries", MappingProxyType(dict(entries)))
        object.__setattr__(self, "_symbols", MappingProxyType(dict(symbols)))

    def resolve(self, kind: ComponentKind, symbol: str) -> object:
        try:
            symbol = validate_registry_symbol(symbol)
        except ValueError as error:
            raise ComponentUnresolvedError() from error

        if self._symbols.get(symbol) is not kind:
            raise ComponentUnresolvedError()
        return self._entries[RegistryKey(kind, symbol)]


__all__ = [
    "COMPONENT_UNRESOLVED",
    "ComponentKind",
    "ComponentRegistry",
    "ComponentUnresolvedError",
    "DuplicateComponentRegistrationError",
    "FrozenComponentRegistry",
    "RegistryKey",
    "validate_registry_symbol",
]
