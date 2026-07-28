import hashlib
import json
import pickle

import pytest

from kiteframe import (
    AdmissionRequest,
    CapabilityCatalog,
    CatalogRequest,
    InvocationRequest,
    KiteframeDiagnosticError,
    load_admission_request,
    load_capability_catalog,
    load_catalog_request,
    load_invocation_request,
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
    capability = {
        "name": "cases.comment",
        "version": "1.0.0",
    }
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
                    "resources": ["tenant:t1/case:case-1"],
                }
            ],
            "resolvedDigest": "03" * 32,
            "resolvedRequirements": [
                {
                    "identity": capability,
                    "required": True,
                    "resources": ["tenant:t1/case:case-1"],
                }
            ],
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
            "invocationId": "inv-1",
            "preconditions": {},
            "selectedResource": "tenant:t1/case:case-1",
            "traceContext": {"traceparent": VALID_TRACEPARENT},
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
    valid_capability_catalog: bytes,
) -> None:
    catalog_request = load_catalog_request(valid_catalog_request)
    admission = load_admission_request(valid_admission_request)
    invocation = load_invocation_request(valid_invocation_request)
    catalog = load_capability_catalog(valid_capability_catalog)

    assert catalog_request.known_catalog_digest == "09" * 32
    assert catalog_request.traceparent == VALID_TRACEPARENT
    assert admission.traceparent == VALID_TRACEPARENT
    assert admission.required_capabilities == (("cases.comment", "1.0.0"),)
    assert invocation.invocation_id == "inv-1"
    assert invocation.admission_id == "adm-1"
    assert invocation.capability_name == "cases.comment"
    assert invocation.capability_version == "1.0.0"
    assert invocation.selected_resource == "tenant:t1/case:case-1"
    assert invocation.traceparent == VALID_TRACEPARENT
    assert catalog.name == "provider.test"
    assert catalog.revision == "revision-1"
    assert len(catalog.catalog_digest) == 64
    assert catalog.descriptor_digests == ()


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
