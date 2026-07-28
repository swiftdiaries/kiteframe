import hashlib
import json
import pickle
from pathlib import Path

import pytest

from kiteframe import load_resolved_agent, resolve_package
from kiteframe._native import (
    CapabilityGrant,
    CapabilityGrantSet,
    InvocationOutcome,
    InvocationStatus,
    load_capability_grant_set,
    load_invocation_outcome,
    load_invocation_status,
)


@pytest.fixture
def workspace() -> Path:
    return Path(__file__).resolve().parents[3]


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def valid_grant_set_bytes() -> bytes:
    grant_set = {
        "actor": "actor:alice",
        "admissionId": "adm-1",
        "agent": "agent:case-worker",
        "catalogDigest": "01" * 32,
        "expiresAt": 200,
        "grants": [
            {
                "capability": {
                    "name": "cases.comment",
                    "version": "1.0.0",
                },
                "resources": ["tenant:t1/case:case-1"],
            }
        ],
        "issuedAt": 100,
        "policyRevision": "policy:7",
        "session": "session:1",
        "task": "task:triage",
    }
    digest = hashlib.sha256(
        b"kiteframe:capability-grant-set:v1\0" + canonical_bytes(grant_set)
    ).hexdigest()
    return canonical_bytes({**grant_set, "grantDigest": digest})


def test_python_round_trip_preserves_exact_golden_bytes(workspace: Path) -> None:
    expected = (
        workspace / "tests/fixtures/resolved/support-agent.json"
    ).read_bytes()
    resolved = load_resolved_agent(expected)
    assert resolved.canonical_json() == expected


def test_digest_tuple_matches_rust_fixture(workspace: Path) -> None:
    fixture_root = workspace / "tests/fixtures"
    expected = json.loads(
        (fixture_root / "resolved/support-agent.digests.json").read_bytes()
    )
    package = fixture_root / "packages/support-agent"
    resolved = resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        fixture_root / "components/deepagents-test.json",
    )

    assert resolved.portable_digest == expected["portableDigest"]
    assert resolved.lock_digest == expected["lockDigest"]
    assert resolved.binding_digest == expected["bindingDigest"]
    assert resolved.resolved_digest == expected["resolvedDigest"]


def test_capability_grant_set_is_frozen_and_round_trips_canonically() -> None:
    expected = valid_grant_set_bytes()
    grant_set = load_capability_grant_set(expected)

    assert isinstance(grant_set, CapabilityGrantSet)
    assert grant_set.admission_id == "adm-1"
    assert grant_set.actor == "actor:alice"
    assert grant_set.agent == "agent:case-worker"
    assert grant_set.task == "task:triage"
    assert grant_set.session == "session:1"
    assert grant_set.policy_revision == "policy:7"
    assert grant_set.catalog_digest == "01" * 32
    assert grant_set.issued_at == 100
    assert grant_set.expires_at == 200
    assert len(grant_set.grant_digest) == 64
    assert isinstance(grant_set.grants, tuple)
    assert isinstance(grant_set.grants[0], CapabilityGrant)
    assert grant_set.grants[0].name == "cases.comment"
    assert grant_set.grants[0].version == "1.0.0"
    assert grant_set.grants[0].resources == ("tenant:t1/case:case-1",)
    assert grant_set.canonical_json() == expected

    with pytest.raises(TypeError):
        CapabilityGrantSet()  # type: ignore[call-arg]
    with pytest.raises(AttributeError):
        grant_set.grants = ()  # type: ignore[misc]
    with pytest.raises(TypeError):
        pickle.dumps(grant_set)
    assert not hasattr(grant_set, "__dict__")


@pytest.mark.parametrize(
    ("loader", "projection", "status"),
    [
        (load_invocation_outcome, InvocationOutcome, "deferred"),
        (load_invocation_status, InvocationStatus, "pending"),
    ],
)
def test_invocation_variants_are_stable_frozen_projections(
    loader: object,
    projection: type,
    status: str,
) -> None:
    expected = canonical_bytes(
        {"invocation_id": "inv-1", "status": status}
    )
    value = loader(expected)  # type: ignore[operator]

    assert isinstance(value, projection)
    assert value.status == status
    assert value.invocation_id == "inv-1"
    assert value.canonical_json() == expected

    with pytest.raises(TypeError):
        projection()
    with pytest.raises(AttributeError):
        value.status = "succeeded"
    with pytest.raises(TypeError):
        pickle.dumps(value)
    assert not hasattr(value, "__dict__")


def test_invalid_provider_output_never_becomes_a_projection() -> None:
    with pytest.raises(Exception) as caught:
        load_invocation_outcome(
            b'{"invocation_id":"inv-1","status":"not-a-status"}'
        )

    diagnostics = json.loads(caught.value.diagnostics_json)
    assert diagnostics[0]["code"] == "KF-CAP-002"
