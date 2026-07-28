import asyncio

import pytest

from kiteframe.registry import (
    ComponentKind,
    ComponentRegistry,
    FrozenComponentRegistry,
    RegistryKey,
)


def test_duplicate_registration_is_rejected_without_overwrite() -> None:
    registry = ComponentRegistry()
    first = object()

    registry.register(ComponentKind.MODEL, "models.primary", first)

    with pytest.raises(ValueError, match="already registered") as duplicate:
        registry.register(ComponentKind.MODEL, "models.primary", object())

    assert duplicate.value.code == "KF-RUNTIME-001"
    assert registry.freeze().resolve(ComponentKind.MODEL, "models.primary") is first


@pytest.mark.parametrize(
    ("kind", "symbol"),
    [
        (ComponentKind.MODEL, "backends.workspace"),
        (ComponentKind.MODEL, "models.missing"),
    ],
)
def test_wrong_kind_and_absent_symbol_use_component_unresolved(
    kind: ComponentKind,
    symbol: str,
) -> None:
    registry = ComponentRegistry()
    registry.register(ComponentKind.BACKEND, "backends.workspace", object())
    frozen = registry.freeze()

    with pytest.raises(Exception) as unresolved:
        frozen.resolve(kind, symbol)

    assert unresolved.value.code == "KF-RUNTIME-001"


def test_frozen_registry_cannot_be_mutated() -> None:
    registry = ComponentRegistry()
    registry.freeze()

    with pytest.raises(RuntimeError, match="frozen"):
        registry.register(ComponentKind.MODEL, "models.late", object())


def test_frozen_registry_constructor_snapshots_mutable_mappings() -> None:
    first = object()
    second = object()
    key = RegistryKey(ComponentKind.MODEL, "models.primary")
    entries = {key: first}
    symbols = {"models.primary": ComponentKind.MODEL}

    frozen = FrozenComponentRegistry(entries, symbols)
    entries[key] = second
    symbols["models.primary"] = ComponentKind.BACKEND

    assert frozen.resolve(ComponentKind.MODEL, "models.primary") is first


def test_frozen_registries_are_isolated_across_100_concurrent_tasks() -> None:
    left_value = object()
    right_value = object()
    left = ComponentRegistry()
    right = ComponentRegistry()
    left.register(ComponentKind.MODEL, "models.primary", left_value)
    right.register(ComponentKind.MODEL, "models.primary", right_value)
    frozen_left = left.freeze()
    frozen_right = right.freeze()

    async def resolve_both(
        barrier: asyncio.Barrier,
    ) -> tuple[object, object]:
        await barrier.wait()
        return (
            frozen_left.resolve(ComponentKind.MODEL, "models.primary"),
            frozen_right.resolve(ComponentKind.MODEL, "models.primary"),
        )

    async def run_concurrently() -> list[tuple[object, object]]:
        barrier = asyncio.Barrier(100)
        return await asyncio.gather(
            *(resolve_both(barrier) for _ in range(100))
        )

    results = asyncio.run(run_concurrently())

    assert all(
        left_result is left_value and right_result is right_value
        for left_result, right_result in results
    )
