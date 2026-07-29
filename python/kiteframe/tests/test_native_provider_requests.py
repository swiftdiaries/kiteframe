import hashlib
import json
import pickle
from pathlib import Path

import pytest

from kiteframe import (
    AdmissionRequest,
    CapabilityCatalog,
    CatalogRequest,
    InvocationOutcome,
    InvocationRequest,
    KiteframeDiagnosticError,
    StatusRequest,
    load_admission_request,
    load_capability_catalog,
    load_catalog_request,
    load_invocation_outcome,
    load_invocation_request,
    load_invocation_status,
    load_status_request,
    resolve_package,
)

VALID_TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


@pytest.fixture
def valid_catalog_request() -> bytes:
    return canonical_bytes(
        {
            "knownCatalogDigest": "09" * 32,
            "traceContext": {"traceparent": VALID_TRACEPARENT},
        }
    )


@pytest.fixture
def valid_admission_request() -> bytes:
    workspace = Path(__file__).resolve().parents[3]
    resolved = json.loads(
        (workspace / "tests/fixtures/resolved/support-agent.json").read_bytes()
    )
    capability = {
        "name": "cases.read",
        "version": "1.2.0",
    }
    return canonical_bytes(
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
            "lockDigest": "02" * 32,
            "optionalCapabilities": [],
            "portableDigest": "01" * 32,
            "requestDigest": (
                "a6f8a332833d30e14a05e70e719adf8c3156593a588151f2a4b96b0ca3ede119"
            ),
            "requiredCapabilities": [
                {
                    "capability": capability,
                    "resources": ["tenant:support"],
                }
            ],
            "resolvedDigest": "03" * 32,
            "resolvedRequirements": resolved["capabilityRequirements"],
            "session": "session:1",
            "task": "task:triage",
            "traceContext": {"traceparent": VALID_TRACEPARENT},
        }
    )


@pytest.fixture
def valid_invocation_request() -> bytes:
    return canonical_bytes(
        {
            "admissionId": "adm-1",
            "arguments": {"caseId": "case-1"},
            "capability": {
                "name": "cases.comment",
                "version": "1.0.0",
            },
            "evidenceRefs": {
                "approval": "evidence://approval/1",
            },
            "grantDigest": "0a" * 32,
            "invocationId": "inv-1",
            "preconditions": {},
            "selectedResource": "tenant:t1/case:case-1",
            "traceContext": {"traceparent": VALID_TRACEPARENT},
        }
    )


@pytest.fixture
def valid_status_request() -> bytes:
    return canonical_bytes(
        {
            "invocationId": "inv-1",
            "traceContext": {
                "baggage": {"kiteframe.request_id": "10" * 16},
                "traceparent": VALID_TRACEPARENT,
                "tracestate": "vendor=value",
            },
        }
    )


@pytest.fixture
def valid_capability_catalog() -> bytes:
    wire = {
        "descriptors": [],
        "identity": {
            "name": "provider.test",
            "revision": "revision-1",
        },
    }
    digest = hashlib.sha256(canonical_bytes(wire)).hexdigest()
    return canonical_bytes({**wire, "catalogDigest": digest})


def test_catalog_request_is_factory_only_and_canonical() -> None:
    with pytest.raises(TypeError):
        CatalogRequest()  # type: ignore[call-arg]

    request = CatalogRequest.default()
    next_request = CatalogRequest.default()

    assert isinstance(request, CatalogRequest)
    assert request.known_catalog_digest is None
    assert request.traceparent != next_request.traceparent
    assert request.canonical_json() == canonical_bytes(
        {"traceContext": {"traceparent": request.traceparent}}
    )


@pytest.mark.parametrize(
    ("projection", "loader", "fixture_name", "frozen_property"),
    [
        (
            CatalogRequest,
            load_catalog_request,
            "valid_catalog_request",
            "traceparent",
        ),
        (
            AdmissionRequest,
            load_admission_request,
            "valid_admission_request",
            "traceparent",
        ),
        (
            InvocationRequest,
            load_invocation_request,
            "valid_invocation_request",
            "invocation_id",
        ),
        (
            StatusRequest,
            load_status_request,
            "valid_status_request",
            "invocation_id",
        ),
        (
            CapabilityCatalog,
            load_capability_catalog,
            "valid_capability_catalog",
            "revision",
        ),
    ],
)
def test_native_provider_values_are_frozen_canonical_projections(
    projection: type,
    loader: object,
    fixture_name: str,
    frozen_property: str,
    request: pytest.FixtureRequest,
) -> None:
    expected = request.getfixturevalue(fixture_name)
    value = loader(expected)  # type: ignore[operator]

    assert isinstance(value, projection)
    assert value.canonical_json() == expected
    assert not hasattr(value, "__dict__")
    with pytest.raises(TypeError):
        projection()
    with pytest.raises(AttributeError):
        setattr(value, frozen_property, "forged")
    with pytest.raises(TypeError):
        pickle.dumps(value)


def test_provider_request_properties_are_stable_native_values(
    valid_catalog_request: bytes,
    valid_admission_request: bytes,
    valid_invocation_request: bytes,
    valid_status_request: bytes,
    valid_capability_catalog: bytes,
) -> None:
    catalog_request = load_catalog_request(valid_catalog_request)
    admission = load_admission_request(valid_admission_request)
    invocation = load_invocation_request(valid_invocation_request)
    status_request = load_status_request(valid_status_request)
    catalog = load_capability_catalog(valid_capability_catalog)

    assert catalog_request.known_catalog_digest == "09" * 32
    assert catalog_request.traceparent == VALID_TRACEPARENT
    assert admission.traceparent == VALID_TRACEPARENT
    assert admission.catalog_name == "provider.test"
    assert admission.catalog_revision == "revision-1"
    assert admission.catalog_digest == "04" * 32
    assert admission.request_digest == (
        "a6f8a332833d30e14a05e70e719adf8c3156593a588151f2a4b96b0ca3ede119"
    )
    assert admission.required_capabilities == (("cases.read", "1.2.0"),)
    assert invocation.invocation_id == "inv-1"
    assert invocation.admission_id == "adm-1"
    assert invocation.capability_name == "cases.comment"
    assert invocation.capability_version == "1.0.0"
    assert invocation.selected_resource == "tenant:t1/case:case-1"
    assert invocation.arguments == {"caseId": "case-1"}
    assert invocation.preconditions == {}
    assert invocation.evidence_refs == {
        "approval": "evidence://approval/1",
    }
    assert invocation.traceparent == VALID_TRACEPARENT
    assert invocation.baggage == {}
    assert status_request.invocation_id == "inv-1"
    assert status_request.traceparent == VALID_TRACEPARENT
    assert status_request.tracestate == "vendor=value"
    assert status_request.baggage == {"kiteframe.request_id": "10" * 16}
    assert catalog.name == "provider.test"
    assert catalog.revision == "revision-1"
    assert len(catalog.catalog_digest) == 64
    assert catalog.descriptor_digests == ()


def test_outcome_exposes_result_without_json_reparse() -> None:
    outcome = load_invocation_outcome(
        canonical_bytes(
            {
                "invocation_id": "inv-1",
                "result": {"caseId": "case-1", "accepted": True},
                "status": "succeeded",
            }
        )
    )

    assert isinstance(outcome, InvocationOutcome)
    result = outcome.result
    assert result == {"caseId": "case-1", "accepted": True}
    assert result is not None
    assert outcome.error is None
    assert outcome.diagnostic is None
    assert outcome.suspension is None
    result["accepted"] = False
    assert outcome.result == {"caseId": "case-1", "accepted": True}


def test_failure_and_denial_are_structured() -> None:
    failure = load_invocation_outcome(
        canonical_bytes(
            {
                "error": {
                    "category": "conflict",
                    "code": "CASE_CONFLICT",
                    "message": "case changed",
                    "retry": "after_refresh",
                },
                "invocation_id": "inv-1",
                "status": "failed",
            }
        )
    )
    denial = load_invocation_status(
        canonical_bytes(
            {
                "diagnostic": {
                    "category": "authorization",
                    "code": "KF-AUTH-003",
                    "details": {},
                    "help": None,
                    "message": "invocation denied",
                    "package_path": None,
                    "retry": "never",
                    "severity": "error",
                    "source_range": None,
                    "stage": "invoke",
                },
                "invocation_id": "inv-1",
                "status": "denied",
            }
        )
    )

    error = failure.error
    diagnostic = denial.diagnostic
    assert error is not None
    assert diagnostic is not None
    assert error.code == "CASE_CONFLICT"
    assert error.category == "conflict"
    assert error.retry == "after_refresh"
    assert error.message == "case changed"
    assert diagnostic.code == "KF-AUTH-003"
    assert diagnostic.details == ()


def test_suspension_is_a_frozen_structured_projection() -> None:
    status = load_invocation_status(
        canonical_bytes(
            {
                "invocation_id": "inv-1",
                "status": "suspended",
                "suspension": {
                    "checkpointRef": "checkpoint:opaque:1",
                    "evidenceKind": "approval",
                    "evidenceRequestRef": "evidence-request:opaque:1",
                    "proposalDigest": "0b" * 32,
                },
            }
        )
    )

    suspension = status.suspension
    assert suspension is not None
    assert suspension.checkpoint_ref == "checkpoint:opaque:1"
    assert suspension.evidence_kind == "approval"
    assert suspension.evidence_request_ref == "evidence-request:opaque:1"
    assert suspension.proposal_digest == "0b" * 32
    with pytest.raises(AttributeError):
        suspension.checkpoint_ref = "forged"  # type: ignore[reportAttributeAccessIssue]


def test_resolved_requirement_exposes_its_frozen_locked_descriptor() -> None:
    workspace = Path(__file__).resolve().parents[3]
    package = workspace / "tests/fixtures/packages/support-agent"
    inputs = resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )
    requirement = inputs.resolved_agent.capability_requirements[0]
    descriptor = requirement.descriptor

    assert descriptor.name == "cases.read"
    assert descriptor.version == "1.2.0"
    assert descriptor.summary == "Read a case"
    assert descriptor.input_schema["type"] == "object"
    assert descriptor.output_schema["type"] == "object"
    assert descriptor.stable_errors == ()
    assert descriptor.execution_modes == ("immediate",)
    assert descriptor.resource_selector_schema == {"type": "string"}
    assert descriptor.effect == "read_only"
    assert descriptor.idempotency == {"kind": "none"}
    assert descriptor.freshness["policyRevisionRequired"] is False
    assert descriptor.preconditions == ()
    assert descriptor.confirmation == {"kind": "none"}
    assert descriptor.approval == {"kind": "none"}
    assert descriptor.consent == {"kind": "none"}
    assert descriptor.descriptor_digest == requirement.descriptor_digest
    with pytest.raises(AttributeError):
        requirement.descriptor = None  # type: ignore[reportAttributeAccessIssue]


def test_provider_requests_reject_noncanonical_bytes(
    valid_invocation_request: bytes,
) -> None:
    with pytest.raises(KiteframeDiagnosticError, match="canonical") as error:
        load_invocation_request(b" " + valid_invocation_request)

    assert error.value.code == "KF-CAP-002"


def test_catalog_loader_rejects_schema_invalid_output() -> None:
    schema_invalid_catalog = canonical_bytes(
        {
            "catalogDigest": "00" * 32,
            "descriptors": [],
            "identity": {
                "name": "provider.test",
                "revision": "revision-1",
            },
            "schemaOnlyField": True,
        }
    )

    with pytest.raises(KiteframeDiagnosticError) as error:
        load_capability_catalog(schema_invalid_catalog)

    assert error.value.code == "KF-CAP-002"
    assert b"schemaOnlyField" not in error.value.diagnostics_json
