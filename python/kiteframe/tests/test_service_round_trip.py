import hashlib
import json
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from kiteframe import delegation_ancestry_digest
from kiteframe._native import (
    load_admission_request,
    load_capability_catalog,
    load_capability_grant_set,
    load_catalog_request,
    load_effect_proposal,
    load_invocation_outcome,
    load_invocation_request,
    load_invocation_status,
    load_status_request,
)

VALID_TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def canonical_digest(domain: bytes, value: object) -> str:
    return hashlib.sha256(domain + canonical_bytes(value)).hexdigest()


def valid_grant_json() -> bytes:
    values = (
        {
            "actor": "actor:alice",
            "admissionId": "adm-1",
            "admissionRequestDigest": "09" * 32,
            "agent": "agent:case-worker",
            "catalogDigest": "01" * 32,
            "catalogIdentity": {
                "name": "provider.test",
                "revision": "revision-1",
            },
            "delegationAncestryDigest": delegation_ancestry_digest([]),
            "authorityRevisions": {
                "authorityRevisionDigest": (
                    "bb4b094d4e6b440e6babaf51624f70d185297df32b5508d36ff03046dd77cbaa"
                ),
                "entries": [{"revision": "7", "source": "policy"}],
            },
            "expiresAt": 200,
            "grants": [
                {
                    "capability": {
                        "name": "cases.comment",
                        "version": "1.0.0",
                    },
                    "executionModes": ["immediate"],
                    "expiresAt": 180,
                    "freshness": {
                        "maxAdmissionAgeSeconds": None,
                        "maxInputAgeSeconds": None,
                        "policyRevisionRequired": False,
                    },
                    "maximumEffect": "read_only",
                    "preconditions": [],
                    "requiredEvidence": {
                        "approval": {"kind": "none"},
                        "confirmation": {"kind": "none"},
                        "consent": {"kind": "none"},
                    },
                    "resources": ["tenant:t1/case:case-1"],
                }
            ],
            "issuedAt": 100,
            "grantDigest": (
                "fa8573bd34fa3793fd71ff96692c3df5781a424a372af319486b7b9883451eed"
            ),
            "optionalDenials": [
                {
                    "capability": {
                        "name": "notes.read",
                        "version": "1.0.0",
                    },
                    "diagnostic": {
                        "category": "authorization",
                        "code": "KF-AUTH-001",
                        "details": {},
                        "help": None,
                        "message": "optional capability denied",
                        "package_path": None,
                        "retry": "never",
                        "severity": "warning",
                        "source_range": None,
                        "stage": "admit",
                    },
                }
            ],
            "policyRevision": "policy:7",
            "session": "session:1",
            "task": "task:triage",
        }
    )
    values.pop("grantDigest")
    values["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        values,
    )
    return canonical_bytes(values)


def valid_catalog_json() -> bytes:
    catalog = {
        "descriptors": [],
        "expiresAt": 200,
        "identity": {
            "name": "provider.test",
            "revision": "revision-1",
        },
        "issuedAt": 100,
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
    values = (
        {
            "actor": "actor:alice",
            "agent": "agent:case-worker",
            "catalogDigest": "04" * 32,
            "catalogIdentity": {
                "name": "provider.test",
                "revision": "revision-1",
            },
            "contextualFacts": {},
            "delegationAncestry": [],
            "delegationAncestryDigest": delegation_ancestry_digest([]),
            "lockDigest": "02" * 32,
            "optionalCapabilities": [],
            "portableDigest": "01" * 32,
            "requestDigest": (
                "a6f8a332833d30e14a05e70e719adf8c3156593a588151f2a4b96b0ca3ede119"
            ),
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
    values.pop("requestDigest")
    values["requestDigest"] = canonical_digest(
        b"kiteframe:admission-request:v1\0",
        values,
    )
    return canonical_bytes(values)


def valid_invocation_json() -> bytes:
    return canonical_bytes(
        {
            "admissionId": "adm-1",
            "arguments": {"caseId": "case-1"},
            "capability": {
                "name": "cases.comment",
                "version": "1.0.0",
            },
            "delegationAncestryDigest": delegation_ancestry_digest([]),
            "evidenceRefs": {"approval": "evidence://approval/1"},
            "grantDigest": "09" * 32,
            "invocationId": "inv-1",
            "preconditions": {},
            "selectedResource": "tenant:t1/case:case-1",
            "traceContext": {"traceparent": VALID_TRACEPARENT},
        }
    )


@pytest.fixture
def service_goldens() -> list[tuple[Callable[[bytes], Any], bytes]]:
    profile_path = (
        Path(__file__).parent
        / "fixtures/conformance/crankshaft-wfm-profile.json"
    )
    profile = json.loads(profile_path.read_bytes())
    return [
        (load_admission_request, canonical_bytes(profile["admissionRequest"])),
        (load_capability_grant_set, canonical_bytes(profile["grantSet"])),
        (load_invocation_request, canonical_bytes(profile["invocationRequest"])),
        (load_status_request, canonical_bytes(profile["statusRequest"])),
        (load_effect_proposal, canonical_bytes(profile["effectProposal"])),
        (load_invocation_outcome, canonical_bytes(profile["suspendedOutcome"])),
    ]


def test_service_variants_round_trip_without_field_loss(
    service_goldens: list[tuple[Callable[[bytes], Any], bytes]],
) -> None:
    for loader, payload in service_goldens:
        value = loader(payload)
        assert value.canonical_json() == payload
        assert not hasattr(value, "__dict__")


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

    effective = grant.grants[0]
    assert effective.execution_modes == ("immediate",)
    assert effective.maximum_effect == "read_only"
    assert effective.expires_at == 180
    assert effective.required_evidence["confirmation"]["kind"] == "none"
    assert effective.freshness["policyRevisionRequired"] is False
    assert effective.preconditions == ()

    revisions = grant.authority_revisions
    assert revisions.entries[0].source == "policy"
    assert revisions.entries[0].revision == "7"
    assert len(revisions.authority_revision_digest) == 64
    with pytest.raises(AttributeError):
        revisions.entries += ()  # type: ignore[misc]

    denial = grant.optional_denials[0]
    assert (denial.name, denial.version) == ("notes.read", "1.0.0")
    assert denial.diagnostic.code == "KF-AUTH-001"
    assert denial.diagnostic.category == "authorization"
    assert denial.diagnostic.severity == "warning"
    assert denial.diagnostic.stage == "admit"
    assert denial.diagnostic.retry == "never"
    assert denial.diagnostic.message == "optional capability denied"
    assert denial.diagnostic.details == ()


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
