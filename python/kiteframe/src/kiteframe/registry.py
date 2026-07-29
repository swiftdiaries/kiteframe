"""Deployment-owned component registration for runtime adapters."""

from collections.abc import Mapping
from dataclasses import dataclass
from enum import Enum
from types import MappingProxyType

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
    HARNESS_PROFILE = "harness_profile"


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


def _validate_component_kind(kind: object) -> ComponentKind:
    """Reject strings and other lookalikes at the deployment boundary."""
    if not isinstance(kind, ComponentKind):
        raise TypeError("kind must be a ComponentKind")
    return kind


def validate_registry_symbol(symbol: str) -> str:
    """Validate the same symbol grammar as Rust's ``RegistrySymbol``."""
    if not isinstance(symbol, str) or not symbol:
        raise ValueError("invalid RegistrySymbol")

    previous_separator = False
    for index, character in enumerate(symbol):
        is_lowercase_letter = (
            character.isascii()
            and character.islower()
            and character.isalpha()
        )
        is_noninitial_digit = (
            character.isascii() and character.isdigit() and index > 0
        )
        if is_lowercase_letter or is_noninitial_digit:
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

        kind = _validate_component_kind(kind)
        symbol = validate_registry_symbol(symbol)
        if symbol in self._symbols:
            raise DuplicateComponentRegistrationError(
                f"registry symbol {symbol!r} is already registered"
            )

        key = RegistryKey(kind, symbol)
        self._entries[key] = value
        self._symbols[symbol] = kind

    def resolve(self, kind: ComponentKind, symbol: str) -> object:
        """Resolve an already-registered deployment component before freeze."""
        try:
            symbol = validate_registry_symbol(symbol)
        except ValueError as error:
            raise ComponentUnresolvedError() from error

        if self._symbols.get(symbol) is not kind:
            raise ComponentUnresolvedError()
        return self._entries[RegistryKey(kind, symbol)]

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
        entries_snapshot: dict[RegistryKey, object] = {}
        expected_symbols: dict[str, ComponentKind] = {}
        for key, value in entries.items():
            if not isinstance(key, RegistryKey):
                raise TypeError("registry entry key must be a RegistryKey")
            kind = _validate_component_kind(key.kind)
            symbol = validate_registry_symbol(key.symbol)
            if symbol in expected_symbols:
                raise ValueError("registry mappings are inconsistent")
            entries_snapshot[RegistryKey(kind, symbol)] = value
            expected_symbols[symbol] = kind

        symbols_snapshot = {
            validate_registry_symbol(symbol): _validate_component_kind(kind)
            for symbol, kind in symbols.items()
        }
        if symbols_snapshot != expected_symbols:
            raise ValueError("registry mappings are inconsistent")

        object.__setattr__(
            self,
            "_entries",
            MappingProxyType(entries_snapshot),
        )
        object.__setattr__(
            self,
            "_symbols",
            MappingProxyType(symbols_snapshot),
        )

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
