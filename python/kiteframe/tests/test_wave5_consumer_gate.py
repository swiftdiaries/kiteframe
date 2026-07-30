import copy
import hashlib
import json
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any

import httpx
import pytest

from kiteframe import (
    CatalogRequest,
    KiteframeDiagnosticError,
    delegation_ancestry_digest,
    load_admission_request,
    load_capability_grant_set_for_request,
    load_effect_proposal,
    load_invocation_outcome,
    load_invocation_outcome_for_request,
    load_invocation_request,
    load_invocation_status_for_request,
    load_status_request,
    resolve_package,
)
from kiteframe.provider import (
    ProviderAuthRequest,
    ProviderHttpClient,
    ProviderTransportError,
)

PROFILE_PATH = (
    Path(__file__).parent / "fixtures/conformance/crankshaft-wfm-profile.json"
)
VALID_TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
GrantMutation = Callable[[dict[str, Any]], None]


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def canonical_digest(domain: bytes, value: object) -> str:
    return hashlib.sha256(domain + canonical_bytes(value)).hexdigest()


def load_profile() -> dict[str, Any]:
    value = json.loads(PROFILE_PATH.read_bytes())
    assert isinstance(value, dict)
    return value


def correlated_grant_bytes(
    profile: dict[str, Any],
    mutation: GrantMutation | None = None,
) -> bytes:
    grant_set = copy.deepcopy(profile["grantSet"])
    grant_set.pop("grantDigest")
    if mutation is not None:
        mutation(grant_set)
    grant_set["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        grant_set,
    )
    return canonical_bytes(grant_set)


def omit_required_grant(grant_set: dict[str, Any]) -> None:
    grant_set["grants"] = []


def broaden_resource(grant_set: dict[str, Any]) -> None:
    grant_set["grants"][0]["resources"] = ["tenant:acme/worker:42/shift:*"]


def broaden_mode(grant_set: dict[str, Any]) -> None:
    grant_set["grants"][0]["executionModes"] = ["deferred"]


def broaden_effect(grant_set: dict[str, Any]) -> None:
    grant_set["grants"][0]["maximumEffect"] = "irreversible_write"


def broaden_expiry(grant_set: dict[str, Any]) -> None:
    grant_set["grants"][0]["expiresAt"] = grant_set["expiresAt"] + 1


def weaken_evidence(grant_set: dict[str, Any]) -> None:
    grant_set["grants"][0]["requiredEvidence"]["approval"] = {"kind": "none"}


def weaken_freshness(grant_set: dict[str, Any]) -> None:
    grant_set["grants"][0]["freshness"]["maxAdmissionAgeSeconds"] = 301


def omit_precondition(grant_set: dict[str, Any]) -> None:
    grant_set["grants"][0]["preconditions"] = []


@pytest.fixture
def native_admission() -> tuple[dict[str, Any], Any]:
    profile = load_profile()
    request = load_admission_request(canonical_bytes(profile["admissionRequest"]))
    return profile, request


def test_required_omission_fails_and_optional_denial_is_directly_projected(
    native_admission: tuple[dict[str, Any], Any],
) -> None:
    profile, request = native_admission

    with pytest.raises(KiteframeDiagnosticError):
        load_capability_grant_set_for_request(
            correlated_grant_bytes(profile, omit_required_grant),
            request,
        )

    grant_set = load_capability_grant_set_for_request(
        correlated_grant_bytes(profile),
        request,
    )
    denial = grant_set.optional_denials[0]
    assert (denial.name, denial.version) == ("workforce.shift.note", "1.0.0")
    assert denial.diagnostic.code == "KF-AUTH-001"
    assert denial.diagnostic.stage == "admit"


@pytest.mark.parametrize(
    "mutation",
    [
        broaden_resource,
        broaden_mode,
        broaden_effect,
        broaden_expiry,
        weaken_evidence,
        weaken_freshness,
        omit_precondition,
    ],
)
def test_effective_grants_cannot_broaden_any_authority_dimension(
    native_admission: tuple[dict[str, Any], Any],
    mutation: GrantMutation,
) -> None:
    profile, request = native_admission

    with pytest.raises(KiteframeDiagnosticError):
        load_capability_grant_set_for_request(
            correlated_grant_bytes(profile, mutation),
            request,
        )


def test_authority_revision_digest_is_persisted_but_not_copied_to_invocation(
    native_admission: tuple[dict[str, Any], Any],
) -> None:
    profile, request = native_admission
    tampered = copy.deepcopy(profile["grantSet"])
    tampered["authorityRevisions"]["entries"][0]["revision"] = "policy-43"
    tampered.pop("grantDigest")
    tampered["grantDigest"] = canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        tampered,
    )

    assert tampered["grantDigest"] != profile["grantSet"]["grantDigest"]
    assert tampered["authorityRevisions"]["authorityRevisionDigest"] != (
        canonical_digest(
            b"kiteframe:authority-revision-set:v1\0",
            tampered["authorityRevisions"]["entries"],
        )
    )

    with pytest.raises(KiteframeDiagnosticError) as error:
        load_capability_grant_set_for_request(canonical_bytes(tampered), request)
    assert error.value.code == "KF-CAP-002"

    invocation = load_invocation_request(canonical_bytes(profile["invocationRequest"]))
    wire = json.loads(invocation.canonical_json())
    assert invocation.admission_id == profile["grantSet"]["admissionId"]
    assert invocation.grant_digest == profile["grantSet"]["grantDigest"]
    assert "authorityRevisions" not in wire
    assert "policyRevision" not in wire


def support_runtime_inputs() -> Any:
    workspace = Path(__file__).resolve().parents[3]
    package = workspace / "tests/fixtures/packages/support-agent"
    return resolve_package(
        package,
        package / "bindings/deepagents.yaml",
        workspace / "tests/fixtures/components/deepagents-test.json",
    )


def support_invocation_bytes() -> bytes:
    return canonical_bytes(
        {
            "admissionId": "admission:support-1",
            "arguments": {},
            "capability": {"name": "cases.read", "version": "1.2.0"},
            "delegationAncestryDigest": delegation_ancestry_digest([]),
            "evidenceRefs": {},
            "grantDigest": "55" * 32,
            "invocationId": "invocation:support-1",
            "preconditions": {},
            "selectedResource": "tenant:support",
            "traceContext": {"traceparent": VALID_TRACEPARENT},
        }
    )


@pytest.mark.parametrize(
    "payload",
    [
        {
            "invocation_id": "invocation:support-1",
            "result": [],
            "status": "succeeded",
        },
        {
            "error": {
                "category": "provider",
                "code": "UNDECLARED_ERROR",
                "message": "undeclared",
                "retry": "never",
            },
            "invocation_id": "invocation:support-1",
            "status": "failed",
        },
        {"invocation_id": "invocation:support-1", "status": "deferred"},
    ],
)
def test_invocation_outcome_must_match_the_embedded_lock(
    payload: dict[str, Any],
) -> None:
    inputs = support_runtime_inputs()
    request = load_invocation_request(support_invocation_bytes())
    requirement = inputs.resolved_agent.capability_requirements[0]

    with pytest.raises(KiteframeDiagnosticError):
        load_invocation_outcome_for_request(
            canonical_bytes(payload),
            request,
            requirement,
        )

    status_request = load_status_request(
        canonical_bytes(
            {
                "invocationId": request.invocation_id,
                "traceContext": {"traceparent": VALID_TRACEPARENT},
            }
        )
    )
    with pytest.raises(KiteframeDiagnosticError):
        load_invocation_status_for_request(
            canonical_bytes(
                {
                    "invocation_id": request.invocation_id,
                    "status": "pending",
                }
            ),
            status_request,
            request,
            requirement,
        )


def test_suspension_exposes_only_protected_refs_and_exact_proposal_digest() -> None:
    profile = load_profile()
    proposal = load_effect_proposal(canonical_bytes(profile["effectProposal"]))
    outcome = load_invocation_outcome(canonical_bytes(profile["suspendedOutcome"]))
    suspension = outcome.suspension

    assert suspension is not None
    assert suspension.evidence_kind == "approval"
    assert suspension.checkpoint_ref == "checkpoint:wfm-1"
    assert suspension.evidence_request_ref == "evidence-request:wfm-1"
    assert suspension.proposal_digest == proposal.proposal_digest
    assert set(json.loads(outcome.canonical_json())["suspension"]) == {
        "checkpointRef",
        "evidenceKind",
        "evidenceRequestRef",
        "proposalDigest",
    }


class StaticAuthenticator:
    async def credential_headers(
        self,
        request: ProviderAuthRequest,
    ) -> Mapping[str, str]:
        assert request.origin == "http://provider.test"
        return {"X-Api-Key": "deployment-secret"}


@pytest.mark.asyncio
async def test_status_propagates_native_trace_and_credentials_only_in_allowed_header(
) -> None:
    profile = load_profile()
    invocation = load_invocation_request(support_invocation_bytes())
    status_wire = dict(profile["statusRequest"])
    status_wire["invocationId"] = invocation.invocation_id
    request = load_status_request(canonical_bytes(status_wire))
    inputs = support_runtime_inputs()
    requirement = inputs.resolved_agent.capability_requirements[0]
    captured: list[httpx.Request] = []

    def handler(http_request: httpx.Request) -> httpx.Response:
        captured.append(http_request)
        return httpx.Response(
            200,
            content=canonical_bytes(
                {
                    "invocation_id": request.invocation_id,
                    "result": {},
                    "status": "succeeded",
                }
            ),
        )

    async with ProviderHttpClient(
        "http://provider.test",
        resolved_runtime_inputs=inputs,
        authenticator=StaticAuthenticator(),
        credential_header_allowlist=frozenset({"x-api-key"}),
        transport=httpx.MockTransport(handler),
        baggage_allowlist=frozenset(
            {"kiteframe.request_id", "kiteframe.session_id"}
        ),
    ) as client:
        result = await client.status(request, invocation, requirement)

    assert result.status == "succeeded"
    sent = captured[0]
    assert sent.headers["traceparent"] == request.traceparent
    assert sent.headers["tracestate"] == request.tracestate
    assert "kiteframe.request_id=" in sent.headers["baggage"]
    assert sent.headers["x-api-key"] == "deployment-secret"
    assert "deployment-secret" not in str(sent.url)
    assert sent.content == b""
    assert [
        name
        for name, value in sent.headers.items()
        if value == "deployment-secret"
    ] == ["x-api-key"]


@pytest.mark.asyncio
async def test_catalog_304_requires_an_exact_sent_known_digest() -> None:
    inputs = support_runtime_inputs()

    def handler(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(304, headers={"etag": f'"{"66" * 32}"'})

    async with ProviderHttpClient(
        "http://provider.test",
        resolved_runtime_inputs=inputs,
        transport=httpx.MockTransport(handler),
    ) as client:
        with pytest.raises(
            ProviderTransportError,
            match="unsolicited not-modified",
        ):
            await client.catalog(CatalogRequest.default())
