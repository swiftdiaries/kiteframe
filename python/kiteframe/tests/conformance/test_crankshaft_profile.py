import json
from pathlib import Path

from kiteframe import (
    load_admission_request,
    load_capability_grant_set_for_request,
    load_effect_proposal,
    load_invocation_outcome,
    load_invocation_request,
    load_status_request,
)


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()


def load_fixture(name: str) -> dict[str, object]:
    path = Path(__file__).parents[1] / "fixtures/conformance" / name
    value = json.loads(path.read_bytes())
    assert isinstance(value, dict)
    return value


def resolved_agent_schema_property_names() -> set[str]:
    workspace = Path(__file__).resolve().parents[4]
    schema = json.loads(
        (workspace / "schemas/v1alpha1/resolved-agent.schema.json").read_bytes()
    )
    properties = schema.get("properties")
    if properties is None:
        properties = schema["$defs"]["ResolvedAgent"]["properties"]
    return set(properties)


def test_crankshaft_profile_uses_only_portable_kiteframe_contracts() -> None:
    profile = load_fixture("crankshaft-wfm-profile.json")
    request = load_admission_request(canonical_bytes(profile["admissionRequest"]))
    grant = load_capability_grant_set_for_request(
        canonical_bytes(profile["grantSet"]),
        request,
    )
    invocation = load_invocation_request(canonical_bytes(profile["invocationRequest"]))
    status = load_status_request(canonical_bytes(profile["statusRequest"]))
    proposal = load_effect_proposal(canonical_bytes(profile["effectProposal"]))
    suspended = load_invocation_outcome(canonical_bytes(profile["suspendedOutcome"]))

    assert grant.grants[0].name == "workforce.shift.change"
    assert grant.authority_revisions.entries[0].source == "wfm-policy"
    assert grant.optional_denials[0].name == "workforce.shift.note"
    assert invocation.admission_id == grant.admission_id
    assert invocation.grant_digest == grant.grant_digest
    assert status.traceparent == request.traceparent == invocation.traceparent
    assert suspended.suspension is not None
    assert proposal.proposal_digest == suspended.suspension.proposal_digest
    assert "crankshaft" not in resolved_agent_schema_property_names()

    portable_values = dict(profile)
    portable_values.pop("profile")
    assert "crankshaft" not in canonical_bytes(portable_values).decode().lower()
