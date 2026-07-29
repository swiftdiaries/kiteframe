import hashlib
import json
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from kiteframe._native import (
    load_admission_request,
    load_capability_catalog,
    load_capability_grant_set,
    load_catalog_request,
    load_invocation_outcome,
    load_invocation_request,
    load_invocation_status,
)

VALID_TRACEPARENT = (
    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
)


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def valid_grant_json() -> bytes:
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


def valid_catalog_json() -> bytes:
    catalog = {
        "descriptors": [],
        "identity": {
            "name": "provider.test",
            "revision": "revision-1",
        },
    }
    digest = hashlib.sha256(canonical_bytes(catalog)).hexdigest()
    return canonical_bytes({**catalog, "catalogDigest": digest})


def valid_admission_json() -> bytes:
    workspace = Path(__file__).resolve().parents[3]
    resolved = json.loads(
        (workspace / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )
    requirement = resolved["capabilityRequirements"][0]
    capability = requirement["lockedCapability"]["identity"]
    return canonical_bytes(
        {
            "actor": "actor:alice",
            "agent": "agent:case-worker",
            "contextualFacts": {},
            "delegationAncestry": [],
            "lockDigest": "02" * 32,
            "optionalCapabilities": [],
            "portableDigest": "01" * 32,
            "requiredCapabilities": [
                {
                    "capability": capability,
                    "resources": requirement["resources"],
                }
            ],
            "resolvedDigest": "03" * 32,
            "resolvedRequirements": [requirement],
            "session": "session:1",
            "task": "task:triage",
            "traceContext": {"traceparent": VALID_TRACEPARENT},
        }
    )


def valid_invocation_json() -> bytes:
    return canonical_bytes(
        {
            "admissionId": "adm-1",
            "arguments": {"caseId": "case-1"},
            "capability": {
                "name": "cases.comment",
                "version": "1.0.0",
            },
            "evidenceRefs": {"approval": "evidence://approval/1"},
            "invocationId": "inv-1",
            "preconditions": {},
            "selectedResource": "tenant:t1/case:case-1",
            "traceContext": {"traceparent": VALID_TRACEPARENT},
        }
    )


@pytest.mark.parametrize(
    ("loader", "wire"),
    [
        (
            load_catalog_request,
            canonical_bytes(
                {
                    "knownCatalogDigest": "09" * 32,
                    "traceContext": {"traceparent": VALID_TRACEPARENT},
                }
            ),
        ),
        (load_admission_request, valid_admission_json()),
        (load_invocation_request, valid_invocation_json()),
        (load_capability_catalog, valid_catalog_json()),
        (load_capability_grant_set, valid_grant_json()),
        (
            load_invocation_outcome,
            canonical_bytes({"invocation_id": "inv-1", "status": "deferred"}),
        ),
        (
            load_invocation_status,
            canonical_bytes({"invocation_id": "inv-1", "status": "pending"}),
        ),
    ],
)
def test_native_service_values_round_trip_exact_canonical_bytes(
    loader: Callable[[bytes], Any],
    wire: bytes,
) -> None:
    assert loader(wire).canonical_json() == wire


def test_python_cannot_mutate_grant_then_reserialize() -> None:
    grant = load_capability_grant_set(valid_grant_json())

    with pytest.raises(AttributeError):
        grant.grants += ()  # type: ignore[misc]

    assert grant.canonical_json() == valid_grant_json()


@pytest.mark.parametrize(
    "loader",
    [load_invocation_outcome, load_invocation_status],
)
def test_status_first_values_round_trip_only_with_status_first_retry(
    loader: Callable[[bytes], Any],
) -> None:
    diagnostic = {
        "category": "capability",
        "code": "KF-CAP-003",
        "details": {},
        "help": None,
        "message": "status is required",
        "package_path": None,
        "retry": "status_first",
        "severity": "error",
        "source_range": None,
        "stage": "invoke",
    }
    wire = canonical_bytes(
        {
            "diagnostic": diagnostic,
            "invocation_id": "inv-1",
            "status": "outcome_unknown",
        }
    )

    assert loader(wire).canonical_json() == wire
