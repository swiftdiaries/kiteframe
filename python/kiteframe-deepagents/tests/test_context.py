from __future__ import annotations

import json
from dataclasses import FrozenInstanceError
from pathlib import Path
from typing import Any, cast

import pytest
from kiteframe import load_capability_grant_set, load_invocation_outcome

import kiteframe_deepagents
import kiteframe_deepagents.context as context_module

WORKSPACE = Path(__file__).resolve().parents[3]


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def native_session_values() -> dict[str, Any]:
    fixture = json.loads(
        (
            WORKSPACE
            / "python/kiteframe/tests/fixtures/conformance/crankshaft-wfm-profile.json"
        ).read_bytes()
    )
    grant_set = load_capability_grant_set(canonical_bytes(fixture["grantSet"]))
    outcome = load_invocation_outcome(
        canonical_bytes(fixture["suspendedOutcome"])
    )
    assert outcome.suspension is not None
    return {
        "actor": "actor:manager-7",
        "session": "session:wfm-1",
        "task": "task:change-shift",
        "admission_id": grant_set.admission_id,
        "grant_digest": grant_set.grant_digest,
        "grants": grant_set.grants,
        "authority_revisions": grant_set.authority_revisions,
        "trace_context": context_module.KiteframeTraceContext(
            traceparent=(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            ),
            tracestate="wfm=alpha",
            baggage=(("kiteframe.session_id", "session:wfm-1"),),
        ),
        "suspension": outcome.suspension,
    }



def test_trace_context_is_deeply_immutable() -> None:
    trace_type = getattr(context_module, "KiteframeTraceContext", None)
    assert trace_type is not None

    trace = trace_type(
        traceparent=(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        ),
        tracestate="kiteframe=primary",
        baggage=(("kiteframe.session_id", "session-1"),),
    )

    with pytest.raises(FrozenInstanceError):
        trace.traceparent = "forged"
    with pytest.raises(TypeError):
        trace.baggage[0] = ("kiteframe.session_id", "forged")


def test_runtime_validation_contracts_are_public() -> None:
    assert (
        kiteframe_deepagents.KiteframeTraceContext
        is context_module.KiteframeTraceContext
    )
    assert kiteframe_deepagents.AuditSink.__name__ == "AuditSink"
    assert (
        kiteframe_deepagents.CheckpointerProtocol.__name__
        == "CheckpointerProtocol"
    )


@pytest.mark.parametrize(
    "overrides",
    [
        {"traceparent": object()},
        {"tracestate": object()},
        {"baggage": {"kiteframe.session_id": "session-1"}},
        {"baggage": (("kiteframe.session_id", object()),)},
    ],
)
def test_trace_context_rejects_non_scalar_or_non_tuple_state(
    overrides: dict[str, object],
) -> None:
    values: dict[str, object] = {
        "traceparent": (
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        ),
        "tracestate": None,
        "baggage": (),
    }
    values.update(overrides)

    with pytest.raises(TypeError):
        cast(Any, context_module.KiteframeTraceContext)(**values)


@pytest.mark.parametrize(
    ("field", "invalid"),
    [
        ("grants", []),
        ("grants", (object(),)),
        ("authority_revisions", object()),
        ("trace_context", object()),
        ("suspension", object()),
    ],
)
def test_session_context_requires_native_values_and_exact_tuples(
    field: str,
    invalid: object,
) -> None:
    values = native_session_values()
    values[field] = invalid

    with pytest.raises(TypeError):
        context_module.KiteframeSessionContext(**values)


def test_session_context_retains_frozen_native_suspension() -> None:
    context = context_module.KiteframeSessionContext(**native_session_values())

    assert context.suspension is not None
    assert context.suspension.checkpoint_ref == "checkpoint:wfm-1"
    with pytest.raises(FrozenInstanceError):
        context.suspension = None  # type: ignore[reportAttributeAccessIssue]
