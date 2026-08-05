# Kiteframe V1 Wave 6 Observability and Conformance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the complete Kiteframe V1 failure model, all ten site journeys with explicit host/Kiteframe ownership, the provider-neutral Crankshaft conformance profile, correlated privacy-safe observability, adversarial hardening, and traceable release evidence.

**Architecture:** Stable Kiteframe correlation names live in an SDK-neutral Rust contract module; Rust and Python instrumentation project them into OpenTelemetry without feeding telemetry back into authorization. A deterministic end-to-end harness combines the Wave 3R contracts, Rust provider, fake semantic operations/model, Deep Agents adapter, OpenFGA, durable checkpointer, invocation store, audit ledger, and explicit host-boundary fixtures. Runtime-neutral journey and Crankshaft-shaped conformance vectors prove portable ownership without adding a workflow DSL, hosted conversation platform, portable progress stream, Crankshaft dependency, or second V1 runtime.

**Tech Stack:** Rust 1.97.1, Python 3.11+, OpenTelemetry Rust/Python, Deep Agents 0.6.12, OpenFGA, Docker Compose, pytest, cargo test/fuzz.

## Global Constraints

- Wave 3R and Waves 4–5 are hard prerequisites. Conformance must exercise the exact `ResolvedRuntimeInputs`, embedded `LockedCapability`, `EffectiveCapabilityGrant`, `AuthorityRevisionSet`, `StatusRequest`, `CatalogFetchResult`, `ProviderAuthenticator`, and expanded `Suspension` contracts.
- Rust and Python emit OpenTelemetry traces and metrics with W3C context propagation through runtime, provider, and OpenFGA.
- GenAI operations are pinned for `create_agent`, `invoke_agent`, and `execute_tool`, but V1 stable `kiteframe.*` attributes never rename or disappear.
- Prompt, message, argument, result, confirmation text, approval evidence, and consent evidence capture is disabled by default.
- Opt-in capture requires declared classification, field-level redaction, deployment policy approval, retention/access policy, and an opaque external encrypted content reference.
- Telemetry exporter failure and backpressure never change authorization or invocation outcomes and never block the write-ahead audit path.
- Audit remains separate, unsampled, append-only, and independently durable.
- Every safety claim in the design has a unit, integration, property, fuzz, concurrency, restart, or end-to-end test.
- Second-runtime adapters and portability spikes are V2 work and MUST NOT enter the V1 release gate.
- The ten site journeys have explicit ownership rows. Host fixtures own conversation restoration, clarification, task synthesis, UI streaming, and presentation. Kiteframe owns package/runtime construction, declared delegation narrowing, current reads/effects, safe denial, proposal/evidence-bound suspension, restart/status, audit, and correlation.
- Journey conformance must not imply a workflow/planner DSL, hosted conversation store, automatic task synthesis, portable UI, or portable progress streaming. Host streaming may project Kiteframe statuses but is not a Kiteframe execution mode.
- The Crankshaft profile is provider-neutral conformance data only. It proves dual-auth correlation, authority narrowing, field projection, revision freshness, audit/outcome linkage, and status-first restart without importing Crankshaft crates or claiming its provider exists.
- `StatusRequest.trace_context` propagates through Python, HTTP, provider, persistence lookup, telemetry, and correlated audit/status evidence.
- Invocation conformance treats `AuthorityRevisionSet` as provider-produced state: clients bind by admission ID plus canonical grant-set `grant_digest`, while the provider loads persisted admission revisions and obtains current revisions before authorization.
- Credential leakage scans cover source fixtures, serialized contracts, logs, diagnostics, traces, audit records, invocation/status storage, snapshots, and failure output.

---

## File Structure

```text
crates/kiteframe-contract/src/
└── telemetry.rs                               # Stable attribute names and capture policy types
crates/kiteframe-otel/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # Rust span/metric helpers
    ├── provider.rs                            # Provider and OpenFGA instrumentation
    └── capture.rs                             # Classification/redaction/reference gate
python/kiteframe-deepagents/src/kiteframe_deepagents/
└── telemetry.py                               # create/invoke/execute spans and stable attributes
tests/conformance/
├── diagnostic-vectors.json
├── authority-vectors.json
├── invocation-vectors.json
├── journey-vectors.json
├── crankshaft-profile-vectors.json
└── telemetry-vectors.json
tests/e2e/
├── pyproject.toml
├── docker-compose.yml
├── conftest.py
├── fixtures/
│   ├── packages/
│   ├── catalogs/
│   ├── policies/
│   └── operations/
├── test_read_paths.py
├── test_effect_paths.py
├── test_suspension_restart.py
├── test_delegation.py
├── test_tamper_and_outages.py
├── test_journeys.py
├── test_crankshaft_profile.py
└── test_telemetry_audit.py
docs/
└── v1alpha1-conformance.md                    # Requirement-to-test and V1 release decision
```

### Task 1: Freeze stable telemetry attributes and privacy-gated content capture

**Files:**
- Create: `crates/kiteframe-contract/src/telemetry.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Create: `crates/kiteframe-otel/Cargo.toml`
- Create: `crates/kiteframe-otel/src/lib.rs`
- Create: `crates/kiteframe-otel/src/capture.rs`
- Test: `crates/kiteframe-otel/tests/capture_policy.rs`
- Create: `tests/conformance/telemetry-vectors.json`

**Interfaces:**
- Consumes: package/lock/binding/resolved digests, `ResolvedContentCaptureRequirement`, trusted deployment capture policy components, and runtime/capability/admission/policy/invocation/audit refs.
- Produces: `TelemetryAttributes`, `ContentCapturePolicy`, `ContentCaptureDecision`, `ContentReference`, and stable attribute constants.

- [ ] **Step 1: Write failing stable-name and default-off tests**

```rust
#[test]
fn stable_v1_attribute_names_match_contract() {
    assert_eq!(ATTR_AGENT_NAME, "kiteframe.agent.name");
    assert_eq!(ATTR_PACKAGE_DIGEST, "kiteframe.agent.package_digest");
    assert_eq!(ATTR_RESOLVED_DIGEST, "kiteframe.agent.resolved_digest");
    assert_eq!(ATTR_LOCK_DIGEST, "kiteframe.lock.digest");
    assert_eq!(ATTR_RUNTIME_ADAPTER, "kiteframe.runtime.adapter");
    assert_eq!(ATTR_CAPABILITY_NAME, "kiteframe.capability.name");
    assert_eq!(ATTR_CAPABILITY_VERSION, "kiteframe.capability.version");
    assert_eq!(ATTR_ADMISSION_ID, "kiteframe.admission.id");
    assert_eq!(ATTR_AUTHORITY_REVISION_DIGEST, "kiteframe.authority_revision.digest");
    assert_eq!(ATTR_TENANT_REF, "kiteframe.principal.tenant_ref");
    assert_eq!(ATTR_HUMAN_REF, "kiteframe.principal.human_ref");
    assert_eq!(ATTR_WORKLOAD_REF, "kiteframe.principal.workload_ref");
    assert_eq!(ATTR_RUN_REF, "kiteframe.run.id");
    assert_eq!(ATTR_INVOCATION_ID, "kiteframe.invocation.id");
    assert_eq!(ATTR_AUDIT_RECORD_ID, "kiteframe.audit.record_id");
}

#[test]
fn content_capture_is_disabled_by_default() {
    let policy = ContentCapturePolicy::default();
    assert_eq!(
        policy.evaluate(&classified_content_fixture()),
        ContentCaptureDecision::Disabled
    );
}
```

- [ ] **Step 2: Run capture tests**

Run: `rtk cargo test -p kiteframe-otel --test capture_policy`

Expected: FAIL because the telemetry contracts do not exist.

- [ ] **Step 3: Add SDK-neutral stable attributes**

```rust
pub struct TelemetryAttributes {
    pub agent_name: AgentName,
    pub package_digest: Sha256Digest,
    pub resolved_digest: Sha256Digest,
    pub lock_digest: Sha256Digest,
    pub runtime_adapter: RuntimeTarget,
    pub capability: Option<CapabilityIdentity>,
    pub admission_id: Option<AdmissionId>,
    pub authority_revision_digest: Option<Sha256Digest>,
    pub tenant_ref: Option<TenantRef>,
    pub human_ref: Option<HumanPrincipalRef>,
    pub workload_ref: Option<WorkloadPrincipalRef>,
    pub run_ref: Option<RunRef>,
    pub invocation_id: Option<InvocationId>,
    pub audit_record_id: Option<AuditRecordId>,
}
```

Keep this module free of OpenTelemetry SDK types; `kiteframe-otel` owns conversion into span attributes. Principal values are opaque verified references, never raw claims, subjects copied from request bodies, bearer tokens, cookies, API keys, or authorization tuples.

- [ ] **Step 4: Implement the five-part capture gate**

```rust
pub fn evaluate_capture(
    policy: &ContentCapturePolicy,
    content: ClassifiedContent<'_>,
) -> Result<ContentCaptureDecision, Diagnostic> {
    if !policy.enabled {
        return Ok(ContentCaptureDecision::Disabled);
    }
    policy.require_declared_classification(&content)?;
    policy.require_deployment_approval()?;
    policy.require_retention_and_access_policy()?;
    let redacted = policy.redactor.redact(content)?;
    let reference = policy.external_store.put_encrypted(redacted)?;
    Ok(ContentCaptureDecision::Reference(reference))
}
```

Construct `ContentCapturePolicy` only by intersecting `ResolvedAgent.content_capture` with the binding's trusted redactor, retention, access, approval, and encrypted-store components. Reject inline raw captured content in span attributes. The only enabled output is an opaque `ContentReference`.

- [ ] **Step 5: Run capture tests**

Run: `rtk cargo test -p kiteframe-otel --test capture_policy`

Expected: PASS for default-off, missing classification, redaction, missing deployment approval, missing retention/access policy, opaque reference, and stable names.

- [ ] **Step 6: Commit telemetry contracts**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-otel tests/conformance/telemetry-vectors.json
rtk git commit -m "feat: define privacy safe telemetry contract"
```

### Task 2: Instrument Rust and Python without coupling telemetry to outcomes

**Files:**
- Create: `crates/kiteframe-otel/src/provider.rs`
- Modify: `crates/kiteframe-provider-http/src/lib.rs`
- Modify: `crates/kiteframe-openfga/src/client.rs`
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/telemetry.py`
- Modify: `python/kiteframe-deepagents/src/kiteframe_deepagents/adapter.py`
- Modify: `python/kiteframe-deepagents/src/kiteframe_deepagents/tools.py`
- Create: `crates/kiteframe-otel/tests/exporter_failure.rs`
- Create: `python/kiteframe-deepagents/tests/test_telemetry.py`

**Interfaces:**
- Consumes: stable telemetry values and W3C trace context.
- Produces: Rust/Python `create_agent`, `invoke_agent`, and `execute_tool` spans plus provider/OpenFGA child spans and metrics.

- [ ] **Step 1: Write failing propagation and exporter-failure tests**

```rust
#[tokio::test]
async fn failing_exporter_does_not_change_allowed_read() {
    let provider = provider_with_exporter(AlwaysFailExporter);
    let outcome = provider.invoke(valid_read_request()).await.unwrap();
    assert!(matches!(outcome, InvocationOutcome::Succeeded { .. }));
}

#[tokio::test]
async fn audit_append_completes_before_blocked_telemetry_export() {
    let events = run_effect_with_blocked_exporter().await;
    assert_eq!(events[0..3], ["authorize", "audit_authorization", "execute"]);
    assert!(!events.contains(&"wait_for_exporter"));
}
```

```python
@pytest.mark.asyncio
async def test_traceparent_reaches_provider_and_openfga(
    instrumented_graph: CompiledStateGraph,
    trace_collector: TraceCollector,
) -> None:
    await instrumented_graph.ainvoke(read_input(), config=trace_config())
    assert trace_collector.path() == [
        "create_agent",
        "invoke_agent",
        "execute_tool",
        "provider.invoke",
        "openfga.check",
    ]
    assert len({span.trace_id for span in trace_collector.spans}) == 1


@pytest.mark.asyncio
async def test_status_request_keeps_trace_after_restart(
    restarted_client: ProviderHttpClient,
    trace_collector: TraceCollector,
) -> None:
    await restarted_client.status(
        build_status_request("inv-1", trace_context("00-status-trace"))
    )
    assert trace_collector.path() == ["provider.status", "invocation_store.status"]
    assert {span.trace_id for span in trace_collector.spans} == {"status-trace"}
```

- [ ] **Step 2: Run telemetry tests**

Run: `rtk cargo test -p kiteframe-otel --test exporter_failure`

Expected: FAIL because instrumentation is not connected.

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_telemetry.py -q`

Expected: FAIL because Python instrumentation does not exist.

- [ ] **Step 3: Add Rust provider and OpenFGA spans**

Create spans around catalog fetch (including typed not-modified), authentication of human/workload principals, admission, invocation validation, point-of-use authorization, audit append, operation execution, traced `StatusRequest` lookup, and OpenFGA calls. Record stable identifiers, opaque tenant/human/workload/run refs, authority-revision digest, and safe status categories; never record raw prompt, arguments, results, evidence, credentials, claims, cookies, API keys, provider ACL rules, legacy fields, or policy tuples.

- [ ] **Step 4: Add Python runtime spans**

```python
@contextmanager
def agent_span(operation: Literal["create_agent", "invoke_agent"], resolved: ResolvedAgent):
    with tracer.start_as_current_span(
        operation,
        attributes={
            "kiteframe.agent.name": resolved.package_name,
            "kiteframe.agent.package_digest": resolved.portable_digest,
            "kiteframe.agent.resolved_digest": resolved.resolved_digest,
            "kiteframe.lock.digest": resolved.lock_digest,
            "kiteframe.runtime.adapter": "deepagents",
        },
    ) as span:
        yield span
```

Use the pinned GenAI operation names in addition to stable attributes. Catch exporter errors at the instrumentation boundary and increment an internal dropped-telemetry counter without altering returned values.

- [ ] **Step 5: Run instrumentation tests**

Run: `rtk cargo test -p kiteframe-otel --test exporter_failure`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_telemetry.py -q`

Expected: PASS for create/invoke/status trace continuity across restart, dual-principal and authority-revision correlation, stable attributes, default content omission, credential leakage scans, exporter outage, and audit-path independence.

- [ ] **Step 6: Commit cross-runtime observability**

```bash
rtk git add crates/kiteframe-otel crates/kiteframe-provider-http crates/kiteframe-openfga python/kiteframe-deepagents
rtk git commit -m "feat: correlate runtime and provider traces"
```

### Task 3: Build the full deterministic end-to-end scenario harness

**Files:**
- Create: `tests/e2e/pyproject.toml`
- Create: `tests/e2e/docker-compose.yml`
- Create: `tests/e2e/conftest.py`
- Create: `tests/e2e/fixtures/packages/`
- Create: `tests/e2e/fixtures/catalogs/`
- Create: `tests/e2e/fixtures/policies/`
- Create: `tests/e2e/fixtures/operations/`
- Create: `tests/e2e/test_read_paths.py`
- Create: `tests/e2e/test_effect_paths.py`
- Create: `tests/e2e/test_suspension_restart.py`
- Create: `tests/e2e/test_delegation.py`
- Create: `tests/e2e/test_tamper_and_outages.py`
- Create: `tests/e2e/test_journeys.py`
- Create: `tests/e2e/test_crankshaft_profile.py`
- Create: `tests/e2e/test_telemetry_audit.py`
- Create: `tests/conformance/journey-vectors.json`
- Create: `tests/conformance/crankshaft-profile-vectors.json`

**Interfaces:**
- Consumes: complete Waves 1–5 system including Wave 3R contracts, explicit synthetic host adapters, and the provider-neutral Crankshaft-shaped fixture from Wave 5.
- Produces: deterministic scenario harness, ten exact journey ownership rows, one named test per design end-to-end requirement, and one named test per Crankshaft profile requirement.

- [ ] **Step 1: Write the scenario matrix before fixture implementation**

```python
SCENARIOS = {
    "allowed_read",
    "admission_denial",
    "point_of_use_denial",
    "stale_policy",
    "confirmation_suspend_resume",
    "idempotent_effect_retry",
    "unknown_outcome_status_resolution",
    "deferred_invocation",
    "subagent_narrowing",
    "process_restart_checkpoint_resume",
    "capability_tampering",
    "lock_tampering",
    "provider_outage_no_fallback",
    "audit_outage_blocks_effect",
}

JOURNEY_ROWS = {
    "J01": {
        "host": {"restore_conversation", "supply_identity_tenant_task_session"},
        "kiteframe": {"construct_runtime", "admit_session", "correlate_status"},
    },
    "J02": {
        "host": {"synthesize_bounded_task", "present_answer"},
        "kiteframe": {"construct_runtime", "authorize_current_capabilities"},
    },
    "J03": {
        "host": {"synthesize_investigation_task", "present_report"},
        "kiteframe": {"compile_declared_child", "narrow_delegated_authority"},
    },
    "J04": {
        "host": {"present_projection"},
        "kiteframe": {"reauthorize_read", "validate_projection_schema"},
    },
    "J05": {
        "host": {"clarify_request", "synthesize_task_after_clarification"},
        "kiteframe": set(),
    },
    "J06": {
        "host": {"render_exact_proposal"},
        "kiteframe": {"digest_proposal", "bind_suspension_to_proposal"},
    },
    "J07": {
        "host": {"collect_and_submit_evidence"},
        "kiteframe": {"revalidate_resume", "authorize_and_audit_effect"},
    },
    "J08": {
        "host": {"stream_ui_status"},
        "kiteframe": {"deferred_status", "restart_status_recovery"},
    },
    "J09": {
        "host": {"present_safe_denial"},
        "kiteframe": {"stable_redacted_denial", "no_ambient_fallback"},
    },
    "J10": {
        "host": {"restore_conversation", "synthesize_new_task"},
        "kiteframe": {"bind_authority_to_task_session", "readmit_without_carryover"},
    },
}


def test_every_design_scenario_has_a_collected_test(pytestconfig: pytest.Config) -> None:
    collected = set(pytestconfig.stash[SCENARIO_MARKS])
    assert collected == SCENARIOS


def test_all_ten_journeys_have_exact_ownership_and_evidence(
    pytestconfig: pytest.Config,
) -> None:
    assert set(JOURNEY_ROWS) == {f"J{number:02d}" for number in range(1, 11)}
    evidence = pytestconfig.stash[JOURNEY_EVIDENCE]
    assert evidence == JOURNEY_ROWS
    portable_surfaces = json.dumps(load_portable_contract_schemas())
    assert "conversation_store" not in portable_surfaces
    assert "workflow" not in portable_surfaces
    assert "progress_stream" not in portable_surfaces
```

- [ ] **Step 2: Run collection and observe missing scenario markers**

Run: `rtk uv run --project tests/e2e pytest --collect-only -q`

Expected: FAIL because the harness and scenario tests do not exist.

- [ ] **Step 3: Create deterministic services and fake model/operations**

Use OpenFGA in Docker, the real Rust provider binary, SQLite invocation state, file audit ledger, an in-memory durable LangGraph checkpointer with restart serialization, a scripted model that makes exact tool calls, and a synthetic host adapter. The synthetic host owns identity/tenant/conversation restoration, clarification, task creation, presentation, and UI status streaming; it is test scaffolding and never enters `agent.yaml`, `ResolvedAgent`, `ResolvedRuntimeInputs`, provider schemas, or runtime feature negotiation. Every test controls time through injected Rust and Python clocks.

- [ ] **Step 4: Implement all named scenarios**

Each technical scenario asserts returned stable outcome/diagnostic, model-visible tool names, provider call order, OpenFGA call count, effect count, invocation status, audit record chain, and trace correlation. Each journey test asserts its exact host and Kiteframe evidence sets from `JOURNEY_ROWS`, including proposal/evidence digest binding, task/session authority non-carryover, and the explicit absence of workflow/planner, hosted conversation, and portable progress-stream contracts. Outage tests additionally assert no filesystem, shell, HTTP, MCP, or undeclared delegation fallback appears.

`test_crankshaft_profile.py` consumes only the checked-in provider-neutral JSON vectors. It proves independently verified human/workload/run correlation, tenant/resource narrowing, stable field projection, canonical authority-revision freshness, revocation after admission, complete audit/outcome linkage, traced status-first restart, and absence of credentials/legacy fields. It must not import a Crankshaft module, read another checkout, or claim provider implementation status.

- [ ] **Step 5: Run the scenario suite twice**

Run: `rtk uv run --project tests/e2e pytest -q`

Expected: PASS.

Run: `rtk uv run --project tests/e2e pytest -q`

Expected: PASS with identical golden outcomes, audit records except timestamps/IDs normalized by the fixture clock, and trace topology.

Run: `rtk uv run --project tests/e2e pytest tests/e2e/test_journeys.py tests/e2e/test_crankshaft_profile.py -q`

Expected: PASS with exactly ten ownership rows, no portable workflow/conversation/progress contract, and all Crankshaft profile dual-auth/projection/revision/audit/restart assertions.

- [ ] **Step 6: Commit the end-to-end harness**

```bash
rtk git add tests/e2e tests/conformance/journey-vectors.json tests/conformance/crankshaft-profile-vectors.json
rtk git commit -m "test: cover kiteframe v1 end to end"
```

### Task 4: Add adversarial property, fuzz, concurrency, and restart gates

**Files:**
- Create: `tests/conformance/diagnostic-vectors.json`
- Create: `tests/conformance/authority-vectors.json`
- Create: `tests/conformance/invocation-vectors.json`
- Modify: `tests/conformance/journey-vectors.json`
- Modify: `tests/conformance/crankshaft-profile-vectors.json`
- Create: `fuzz/fuzz_targets/lock_bytes.rs`
- Create: `fuzz/fuzz_targets/descriptor_bundle.rs`
- Create: `fuzz/fuzz_targets/resolved_ir.rs`
- Create: `crates/kiteframe-provider/tests/concurrency.rs`
- Create: `crates/kiteframe-provider-sqlite/tests/crash_recovery.rs`
- Create: `python/kiteframe-deepagents/tests/test_session_isolation_property.py`

**Interfaces:**
- Consumes: complete Rust/Python contracts, exact journey ownership rows, provider-neutral Crankshaft profile, and traced `StatusRequest`.
- Produces: runtime-neutral conformance vectors plus adversarial authority-revision, dual-principal, credential-leakage, crash/restart, and release gates.

- [ ] **Step 1: Write conformance-vector loaders in Rust and Python**

```rust
#[test]
fn diagnostic_vectors_match_reserved_codes_and_retry_classes() {
    for vector in load_vectors("tests/conformance/diagnostic-vectors.json") {
        let diagnostic = run_vector(&vector);
        assert_eq!(diagnostic.code.as_str(), vector.expected_code);
        assert_eq!(diagnostic.retry.as_str(), vector.expected_retry);
        assert_redacted(&diagnostic);
    }
}
```

```python
@pytest.mark.parametrize("vector", load_authority_vectors())
def test_python_visibility_matches_rust_vector(vector: AuthorityVector) -> None:
    assert visible_names(vector.to_session()) == set(vector.expected_visible)


@pytest.mark.parametrize("vector", load_crankshaft_profile_vectors())
def test_provider_profile_matches_rust_and_python(vector: ProviderProfileVector) -> None:
    assert evaluate_profile(vector).canonical_json() == vector.expected_canonical_json
    assert evaluate_profile(vector).authority_revision_digest == vector.expected_revision_digest
    assert evaluate_profile(vector).authenticated_refs == vector.expected_authenticated_refs
```

- [ ] **Step 2: Run vector tests before checking in the corpus**

Run: `rtk cargo test --workspace conformance_vectors`

Expected: FAIL because the vector corpus is absent.

- [ ] **Step 3: Add adversarial vectors and fuzz targets**

Include duplicate identities, catalog identity/digest mismatch, missing required capabilities, optional denial ordering, selector widening, per-grant expiry boundary, evidence-kind/reference/proposal-digest substitution, locked stable-error/mode mismatch, mixed effect classes, attempted client-supplied authority revisions, stale provider-loaded `AuthorityRevisionSet`, admission/grant-set digest mismatch, human/workload/tenant/run mismatch, unknown status retry, missing or malformed status trace context, malformed canonical JSON, digest collision attempts via concatenation boundaries, field-projection widening, and sensitive diagnostic values.

```rust
fuzz_target!(|bytes: &[u8]| {
    let result = kiteframe_resolver::deserialize_and_validate_resolved_agent(bytes);
    if let Ok(agent) = result {
        assert_eq!(canonical_json(&agent).unwrap(), bytes);
    }
});
```

- [ ] **Step 4: Add concurrent duplicate and crash-window tests**

Exercise 100 concurrent same-key effect requests, process termination after reservation, after authorization audit, during effect response loss, and before outcome audit. Rotate policy/model/configuration revisions and restart with the same traced `StatusRequest`. Assert one effect maximum, correct `outcome_unknown`, recoverable status, unchanged admission/grant/catalog/descriptor/authority-revision/principal/idempotency/audit linkage, and intact audit chain.

Add a recursive leakage scan over canonical contracts, fixture snapshots, captured logs, diagnostics, traces, audit JSONL, SQLite invocation/status rows, and pytest failure output. Seed unique sentinels for human bearer token, workload credential, cookie, raw claims, evidence body, provider field-ACL rule, and legacy field; assert none appears while opaque human/workload/run references remain correlated.

- [ ] **Step 5: Run the hardening suite**

Run: `rtk cargo test --workspace --all-features`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_session_isolation_property.py -q`

Expected: PASS for task/session authority non-carryover, principal isolation, authority-revision refresh, and traced status lookup.

Run: `rtk uv run --project tests/e2e pytest tests/e2e/test_crankshaft_profile.py -q`

Expected: PASS for dual-auth correlation, authority narrowing, stable field projection, revision freshness, audit/outcome linkage, credential leakage scan, and status-first restart.

Run: `rtk cargo fuzz run lock_bytes -- -max_total_time=30`

Expected: no crash.

Run: `rtk cargo fuzz run descriptor_bundle -- -max_total_time=30`

Expected: no crash.

Run: `rtk cargo fuzz run resolved_ir -- -max_total_time=30`

Expected: no crash.

- [ ] **Step 6: Commit conformance vectors and hardening**

```bash
rtk git add tests/conformance fuzz crates/kiteframe-provider/tests crates/kiteframe-provider-sqlite/tests python/kiteframe-deepagents/tests
rtk git commit -m "test: harden authority and recovery invariants"
```

### Task 5: Publish the requirement-to-test matrix and V1 release decision

**Files:**
- Create: `docs/v1alpha1-conformance.md`
- Modify: `README.md`
- Modify: `site/app/page.tsx`
- Modify: `site/app/diagrams.ts`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: all V1 plan exit evidence, exact ten-journey ownership/evidence rows, Wave 3R contract rows, and provider-neutral Crankshaft profile rows.
- Produces: final V1 alpha conformance document and release readiness decision; no new runtime API.

- [ ] **Step 1: Create the traceability test**

```rust
#[test]
fn every_normative_requirement_has_a_test_evidence_id() {
    let matrix = load_conformance_matrix("docs/v1alpha1-conformance.md");
    for requirement in load_normative_requirements([
        FOUNDING_DESIGN_SPEC,
        WAVE_3R_DESIGN_ADDENDUM,
    ]) {
        assert!(
            matrix.has_passing_evidence(&requirement.id),
            "missing passing evidence for {}",
            requirement.id
        );
    }
}

#[test]
fn journeys_and_provider_profiles_have_exact_owned_evidence() {
    let matrix = load_conformance_matrix("docs/v1alpha1-conformance.md");
    assert_eq!(
        matrix.ids_with_prefix("JOURNEY-"),
        (1..=10).map(|number| format!("JOURNEY-{number:02}")).collect::<Vec<_>>()
    );
    for row in matrix.journey_rows() {
        assert!(!row.host_owned.is_empty() || !row.kiteframe_owned.is_empty());
        assert_eq!(row.passing_tests, row.required_tests);
    }
    assert_eq!(
        matrix.ids_with_prefix("PROFILE-CRANKSHAFT-"),
        [
            "PROFILE-CRANKSHAFT-DUAL-AUTH",
            "PROFILE-CRANKSHAFT-AUTHORITY-NARROWING",
            "PROFILE-CRANKSHAFT-FIELD-PROJECTION",
            "PROFILE-CRANKSHAFT-REVISION-FRESHNESS",
            "PROFILE-CRANKSHAFT-AUDIT-OUTCOME",
            "PROFILE-CRANKSHAFT-STATUS-RESTART",
        ]
    );
}
```

- [ ] **Step 2: Run traceability before completing the matrix**

Run: `rtk cargo test -p kiteframe-conformance requirement_traceability`

Expected: FAIL and list every unmapped normative requirement.

- [ ] **Step 3: Fill the matrix with exact evidence**

For each normative requirement in the founding design and Wave 3R addendum, including every package, lock, capability, admission, authentication, invocation, authorization, adapter, delegation, suspension, proposal/evidence binding, traced status, telemetry, audit, reliability, journey, and provider-profile requirement, record:

1. stable requirement ID;
2. normative source file and section;
3. owner (`host`, `kiteframe`, or `provider deployment`);
4. exact contract/type and field names;
5. exact test path and test name;
6. evidence kind (`unit`, `property`, `fuzz`, `integration`, `container`, `restart`, `concurrency`, or `end-to-end`);
7. most recent passing command; and
8. expected security outcome.

The ten `JOURNEY-01` through `JOURNEY-10` rows must reproduce `JOURNEY_ROWS` exactly. Host rows never count as proof of a Kiteframe workflow, conversation store, task synthesizer, UI, or progress stream. Crankshaft profile rows state “provider-neutral fixture; implementation status not asserted” and map dual authentication, authority narrowing, stable field projection, revision freshness, audit/outcome linkage, and status-first restart to exact tests.

Update `site/app/page.tsx` and `site/app/diagrams.ts` in the same review so they
state that the host bounds the request and supplies task/session context,
Kiteframe resolves a published agent package, declared child packages narrow
authority without a separate V1 typed child-I/O contract, and host progress
streaming projects durable status rather than defining a portable Kiteframe
mode. Keep exact proposal presentation host-owned and describe Kiteframe's
portable responsibility as proposal/evidence reference and digest binding.
The conformance test MUST extract these claims and fail when the site reclaims
task synthesis, workflow execution, conversation storage, or portable progress
streaming.

- [ ] **Step 4: Run the complete release candidate suite**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `rtk cargo test --workspace --all-features`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe pytest -q`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe-deepagents pytest -q`

Expected: PASS.

Run: `rtk uv run --project tests/e2e pytest -q`

Expected: PASS.

Run: `rtk uv run --project tests/e2e pytest tests/e2e/test_journeys.py tests/e2e/test_crankshaft_profile.py -q`

Expected: PASS for exactly ten host/Kiteframe ownership rows and all six provider-neutral Crankshaft profile requirements.

Run: `rtk proxy rg -n \"bearer-sentinel|workload-secret-sentinel|cookie-sentinel|raw-claims-sentinel|evidence-body-sentinel|field-acl-rule-sentinel|legacy-field-sentinel\" target tests docs/v1alpha1-conformance.md`

Expected: no matches outside the leakage-test source declarations themselves.

- [ ] **Step 5: Apply the V1 release rule**

Mark V1 ready for release review only when every required V1, Wave 3R, journey-ownership, and provider-profile conformance row passes. Otherwise list the exact failed row IDs and keep V1 unreleased; do not claim host-owned presentation behavior as Kiteframe evidence, claim Crankshaft provider implementation from the neutral profile, defer a failed V1 requirement to V2, or weaken portable semantics to force a pass.

- [ ] **Step 6: Commit the final conformance gate**

```bash
rtk git add docs/v1alpha1-conformance.md README.md site/app/page.tsx site/app/diagrams.ts .github/workflows/ci.yml
rtk git commit -m "docs: publish kiteframe v1 conformance"
```

## Wave 6 Exit Criteria

- Every Wave 3R canonical contract has exact cross-language, schema, runtime, provider, and conformance evidence; no stale shadow grant/status/catalog/suspension type remains.
- Rust, Python, provider, and OpenFGA spans share W3C trace context and stable `kiteframe.*` attributes.
- Human/workload/run refs and `AuthorityRevisionSet` digests correlate across admission, invocation, traced status, audit, outcome, and restart without recording credentials or raw claims.
- Content capture is off by default and can only emit a classified, redacted, approved, policy-bound opaque reference.
- Telemetry exporter failure and backpressure do not alter decisions, effects, statuses, or audit durability.
- All ten site journeys have deterministic host/Kiteframe ownership rows and passing evidence. Host conversation restoration, clarification, task synthesis, UI streaming, and presentation are never described as portable Kiteframe features.
- Kiteframe package/runtime construction, declared delegation narrowing, current reads/effects, safe denial, proposal/evidence-bound suspension, restart/status, audit, and correlation have direct evidence.
- No workflow/planner DSL, hosted conversation store, automatic task synthesis, or portable progress-stream contract appears in V1 schemas, IR, runtime features, or release claims.
- The provider-neutral Crankshaft profile passes dual-auth correlation, authority narrowing, field projection, revision freshness, audit/outcome linkage, status-first restart, and repository-independence tests without asserting provider implementation.
- Adversarial vectors, credential leakage scans, fuzz targets, concurrent duplicates, crash windows, traced status restart, and session isolation pass.
- The V1 release decision follows the full requirement-to-test matrix, not schedule pressure.
- No second-runtime adapter or portability spike is required for V1 completion.
