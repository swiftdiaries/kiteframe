# Kiteframe V1 Wave 6 Observability and Conformance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the complete Kiteframe V1 failure model with correlated, privacy-safe observability, adversarial hardening, and traceable release evidence.

**Architecture:** Stable Kiteframe correlation names live in an SDK-neutral Rust contract module; Rust and Python instrumentation project them into OpenTelemetry without feeding telemetry back into authorization. A deterministic end-to-end harness combines the Rust provider, fake semantic operations/model, Deep Agents adapter, OpenFGA, durable checkpointer, invocation store, and audit ledger. Runtime-neutral conformance vectors keep the portable contracts independently testable without adding a second V1 runtime.

**Tech Stack:** Rust 1.97.1, Python 3.11+, OpenTelemetry Rust/Python, Deep Agents 0.6.12, OpenFGA, Docker Compose, pytest, cargo test/fuzz.

## Global Constraints

- Rust and Python emit OpenTelemetry traces and metrics with W3C context propagation through runtime, provider, and OpenFGA.
- GenAI operations are pinned for `create_agent`, `invoke_agent`, and `execute_tool`, but V1 stable `kiteframe.*` attributes never rename or disappear.
- Prompt, message, argument, result, confirmation text, approval evidence, and consent evidence capture is disabled by default.
- Opt-in capture requires declared classification, field-level redaction, deployment policy approval, retention/access policy, and an opaque external encrypted content reference.
- Telemetry exporter failure and backpressure never change authorization or invocation outcomes and never block the write-ahead audit path.
- Audit remains separate, unsampled, append-only, and independently durable.
- Every safety claim in the design has a unit, integration, property, fuzz, concurrency, restart, or end-to-end test.
- Second-runtime adapters and portability spikes are V2 work and MUST NOT enter the V1 release gate.

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
    assert_eq!(ATTR_POLICY_REVISION, "kiteframe.policy.revision");
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
    pub policy_revision: Option<PolicyRevision>,
    pub invocation_id: Option<InvocationId>,
    pub audit_record_id: Option<AuditRecordId>,
}
```

Keep this module free of OpenTelemetry SDK types; `kiteframe-otel` owns conversion into span attributes.

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
```

- [ ] **Step 2: Run telemetry tests**

Run: `rtk cargo test -p kiteframe-otel --test exporter_failure`

Expected: FAIL because instrumentation is not connected.

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_telemetry.py -q`

Expected: FAIL because Python instrumentation does not exist.

- [ ] **Step 3: Add Rust provider and OpenFGA spans**

Create spans around catalog fetch, admission, invocation validation, point-of-use authorization, audit append, operation execution, status lookup, and OpenFGA calls. Record stable identifiers and safe status categories; never record raw prompt, arguments, results, evidence, credentials, or policy tuples.

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

Expected: PASS for trace continuity, stable attributes, default content omission, exporter outage, and audit-path independence.

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
- Create: `tests/e2e/test_telemetry_audit.py`

**Interfaces:**
- Consumes: complete Waves 1–5 system.
- Produces: deterministic scenario harness and one named test per design end-to-end requirement.

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


def test_every_design_scenario_has_a_collected_test(pytestconfig: pytest.Config) -> None:
    collected = set(pytestconfig.stash[SCENARIO_MARKS])
    assert collected == SCENARIOS
```

- [ ] **Step 2: Run collection and observe missing scenario markers**

Run: `rtk uv run --project tests/e2e pytest --collect-only -q`

Expected: FAIL because the harness and scenario tests do not exist.

- [ ] **Step 3: Create deterministic services and fake model/operations**

Use OpenFGA in Docker, the real Rust provider binary, SQLite invocation state, file audit ledger, an in-memory durable LangGraph checkpointer with restart serialization, and a scripted model that makes exact tool calls. Every test controls time through injected Rust and Python clocks.

- [ ] **Step 4: Implement all named scenarios**

Each test asserts returned stable outcome/diagnostic, model-visible tool names, provider call order, OpenFGA call count, effect count, invocation status, audit record chain, and trace correlation. Outage tests additionally assert no filesystem, shell, HTTP, MCP, or undeclared delegation fallback appears.

- [ ] **Step 5: Run the scenario suite twice**

Run: `rtk uv run --project tests/e2e pytest -q`

Expected: PASS.

Run: `rtk uv run --project tests/e2e pytest -q`

Expected: PASS with identical golden outcomes, audit records except timestamps/IDs normalized by the fixture clock, and trace topology.

- [ ] **Step 6: Commit the end-to-end harness**

```bash
rtk git add tests/e2e
rtk git commit -m "test: cover kiteframe v1 end to end"
```

### Task 4: Add adversarial property, fuzz, concurrency, and restart gates

**Files:**
- Create: `tests/conformance/diagnostic-vectors.json`
- Create: `tests/conformance/authority-vectors.json`
- Create: `tests/conformance/invocation-vectors.json`
- Create: `fuzz/fuzz_targets/lock_bytes.rs`
- Create: `fuzz/fuzz_targets/descriptor_bundle.rs`
- Create: `fuzz/fuzz_targets/resolved_ir.rs`
- Create: `crates/kiteframe-provider/tests/concurrency.rs`
- Create: `crates/kiteframe-provider-sqlite/tests/crash_recovery.rs`
- Create: `python/kiteframe-deepagents/tests/test_session_isolation_property.py`

**Interfaces:**
- Consumes: complete Rust/Python contracts.
- Produces: runtime-neutral conformance vectors and adversarial release gates.

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
```

- [ ] **Step 2: Run vector tests before checking in the corpus**

Run: `rtk cargo test --workspace conformance_vectors`

Expected: FAIL because the vector corpus is absent.

- [ ] **Step 3: Add adversarial vectors and fuzz targets**

Include duplicate identities, selector widening, expiry boundary, evidence substitution, mixed effect classes, unknown status retry, malformed canonical JSON, digest collision attempts via concatenation boundaries, and sensitive diagnostic values.

```rust
fuzz_target!(|bytes: &[u8]| {
    let result = kiteframe_resolver::deserialize_and_validate_resolved_agent(bytes);
    if let Ok(agent) = result {
        assert_eq!(canonical_json(&agent).unwrap(), bytes);
    }
});
```

- [ ] **Step 4: Add concurrent duplicate and crash-window tests**

Exercise 100 concurrent same-key effect requests, process termination after reservation, after authorization audit, during effect response loss, and before outcome audit. Assert one effect maximum, correct `outcome_unknown`, recoverable status, and intact audit chain.

- [ ] **Step 5: Run the hardening suite**

Run: `rtk cargo test --workspace --all-features`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_session_isolation_property.py -q`

Expected: PASS.

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
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: all V1 plan exit evidence.
- Produces: final V1 alpha conformance document and release readiness decision; no new runtime API.

- [ ] **Step 1: Create the traceability test**

```rust
#[test]
fn every_normative_requirement_has_a_test_evidence_id() {
    let matrix = load_conformance_matrix("docs/v1alpha1-conformance.md");
    for requirement in load_normative_requirements(DESIGN_SPEC) {
        assert!(
            matrix.has_passing_evidence(&requirement.id),
            "missing passing evidence for {}",
            requirement.id
        );
    }
}
```

- [ ] **Step 2: Run traceability before completing the matrix**

Run: `rtk cargo test -p kiteframe-conformance requirement_traceability`

Expected: FAIL and list every unmapped normative requirement.

- [ ] **Step 3: Fill the matrix with exact evidence**

For each package, lock, capability, admission, invocation, authorization, adapter, delegation, suspension, telemetry, audit, and reliability requirement, record the owning test path/name and most recent passing command. Distinguish unit, property, fuzz, integration, container, restart, concurrency, and end-to-end evidence.

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

- [ ] **Step 5: Apply the V1 release rule**

Mark V1 ready for release review only when every required V1 conformance row passes. Otherwise list the exact failed rows and keep V1 unreleased; do not defer a failed V1 requirement to V2 or weaken portable semantics to force a pass.

- [ ] **Step 6: Commit the final conformance gate**

```bash
rtk git add docs/v1alpha1-conformance.md README.md .github/workflows/ci.yml
rtk git commit -m "docs: publish kiteframe v1 conformance"
```

## Wave 6 Exit Criteria

- Rust, Python, provider, and OpenFGA spans share W3C trace context and stable `kiteframe.*` attributes.
- Content capture is off by default and can only emit a classified, redacted, approved, policy-bound opaque reference.
- Telemetry exporter failure and backpressure do not alter decisions, effects, statuses, or audit durability.
- Every end-to-end scenario named in the design passes with exact diagnostic, visibility, effect-count, status, audit, and trace assertions.
- Adversarial vectors, fuzz targets, concurrent duplicates, crash windows, restart, and session isolation pass.
- The V1 release decision follows the full requirement-to-test matrix, not schedule pressure.
- No second-runtime adapter or portability spike is required for V1 completion.
