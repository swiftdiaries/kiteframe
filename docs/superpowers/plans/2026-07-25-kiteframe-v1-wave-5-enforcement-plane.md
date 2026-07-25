# Kiteframe V1 Wave 5 Capability Enforcement Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the standardized capability provider with monotonic admission, current point-of-use authorization, a real OpenFGA reference backend, schema-validated invocation, durable idempotency/status, and write-ahead audit ordering.

**Architecture:** `kiteframe-provider` is a Rust state machine over pluggable authorization, operation, invocation-store, and audit interfaces. Admission computes a time-bounded visibility envelope but never authorizes execution. `kiteframe-provider-http` exposes the four V1 routes, while `kiteframe-openfga` implements the replaceable reference authorization backend through pinned store/model configuration and current `Check` calls.

**Tech Stack:** Rust 1.97.1, Tokio, axum, reqwest/rustls, jsonschema, async-trait, OpenFGA HTTP API, testcontainers, SQLite for the reference invocation store, append-only JSONL audit fixture.

## Global Constraints

- Visible authority equals package requirements ∩ deployment policy ∩ actor authority ∩ task/session grants ∩ available locked catalog versions.
- Explicit deny wins, absence is deny, and narrower resource, expiry, evidence, effect, or execution-mode terms win.
- Admission filters visibility but never replaces point-of-use authorization.
- Every read and effectful invocation requires a current authorization decision; cached admission never overrides revocation.
- Required admission denial stops session construction with `KF-AUTH-001`; optional denial removes the grant with a stable diagnostic.
- Policy that is older than the descriptor/admission freshness requirement fails with `KF-AUTH-004`.
- OpenFGA `ListObjects` filters candidates during admission; OpenFGA `Check` runs immediately before every operation.
- OpenFGA store ID and authorization model ID are pinned in deployment configuration and recorded in grants and audit.
- Every effectful operation has a caller-generated idempotency key and retained status for at least the descriptor retention window.
- Unknown effect outcome requires status resolution before retry with the same key; a new key is forbidden until terminal resolution or explicit authorized abandonment.
- A durable write-ahead authorization record and receipt precede every effect. Audit append failure blocks the effect with `KF-AUDIT-001`.
- Provider result validation occurs before the result reaches the adapter or model.
- Provider, OpenFGA, audit, operation, or storage failure never exposes an unrestricted fallback.

---

## File Structure

```text
crates/kiteframe-provider/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # Provider service facade
    ├── authority.rs                           # Resource/effect/evidence envelope intersection
    ├── admission.rs                           # Grant-set construction and required/optional behavior
    ├── authorization.rs                       # Replaceable backend trait and current decision
    ├── operation.rs                           # Trusted semantic operation registry
    ├── invocation.rs                          # Validation and execution state machine
    ├── status.rs                              # Durable idempotency and invocation state
    └── audit.rs                               # AuditSink trait and record contracts
crates/kiteframe-openfga/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # OpenFgaAuthorizationBackend
    ├── client.rs                              # ListObjects/Check HTTP requests
    ├── mapping.rs                             # Kiteframe refs to OpenFGA objects/contextual tuples
    └── freshness.rs                           # Model/revision and outage rules
crates/kiteframe-audit/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # In-memory and file ledger implementations
    └── chain.rs                               # Partition sequence and hash chaining
crates/kiteframe-provider-http/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # axum router factory
    ├── routes.rs                              # Four V1 handlers
    ├── response.rs                            # Stable body and transport status mapping
    ├── trace.rs                               # W3C propagation and baggage filtering
    └── main.rs                                # TLS server configuration
crates/kiteframe-provider-sqlite/
├── Cargo.toml
├── migrations/0001_invocations.sql
└── src/lib.rs                                 # Durable invocation/idempotency reference store
openfga/
├── authorization-model.fga
└── test-tuples.yaml
tests/provider/
├── docker-compose.yml
└── fixtures/                                  # Catalog, policy, effects, evidence, failures
```

### Task 1: Implement monotonic authority envelopes and admission behavior

**Files:**
- Create: `crates/kiteframe-provider/Cargo.toml`
- Create: `crates/kiteframe-provider/src/lib.rs`
- Create: `crates/kiteframe-provider/src/authority.rs`
- Create: `crates/kiteframe-provider/src/admission.rs`
- Test: `crates/kiteframe-provider/tests/authority.rs`
- Test: `crates/kiteframe-provider/tests/admission.rs`

**Interfaces:**
- Consumes: Wave 3 `AdmissionRequest`, `CapabilityGrantSet`, locked descriptors, resolved requirements, diagnostics.
- Produces: `AuthorityTerm`, `EffectiveEnvelope`, `intersect_authority(...)`, `AdmissionService::admit(...)`.

- [ ] **Step 1: Write failing deny-precedence and monotonicity tests**

```rust
#[test]
fn explicit_deny_wins_over_allows() {
    let terms = vec![
        AuthorityTerm::allow(grant("cases.read", "tenant:t1/case:*")),
        AuthorityTerm::deny("cases.read"),
        AuthorityTerm::allow(grant("cases.read", "tenant:t1/case:case-1")),
    ];
    assert!(intersect_authority(&terms).unwrap().is_empty());
}

#[test]
fn narrower_resource_expiry_and_evidence_win() {
    let effective = intersect_authority(&[
        term("tenant:t1/case:*", HOUR_2, Evidence::Confirmation),
        term("tenant:t1/case:case-7", HOUR_1, Evidence::Approval),
    ]).unwrap();
    assert_eq!(effective.resources(), ["tenant:t1/case:case-7"]);
    assert_eq!(effective.expires_at(), HOUR_1);
    assert_eq!(effective.evidence(), Evidence::Approval);
}

proptest! {
    #[test]
    fn adding_a_restriction_never_increases_envelope(
        base in authority_term_strategy(),
        restriction in narrower_term_strategy(),
    ) {
        let before = intersect_authority(std::slice::from_ref(&base)).unwrap();
        let after = intersect_authority(&[base, restriction]).unwrap();
        prop_assert!(after.is_subset_of(&before));
    }
}
```

- [ ] **Step 2: Run authority tests**

Run: `rtk cargo test -p kiteframe-provider --test authority`

Expected: FAIL because the provider crate does not exist.

- [ ] **Step 3: Implement selector and envelope partial ordering**

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveGrant {
    pub identity: CapabilityIdentity,
    pub resources: NonEmptySet<NormalizedResourceSelector>,
    pub execution_modes: NonEmptySet<ExecutionMode>,
    pub maximum_effect: EffectClassification,
    pub expires_at: Timestamp,
    pub required_evidence: EvidenceRequirementSet,
    pub freshness: FreshnessRequirement,
}

pub fn intersect_authority(
    terms: &[AuthorityTerm],
) -> Result<EffectiveEnvelope, Vec<Diagnostic>> {
    if terms.is_empty() || terms.iter().any(AuthorityTerm::is_explicit_deny) {
        return Ok(EffectiveEnvelope::empty());
    }
    terms.iter().map(AuthorityTerm::allow_value).try_fold(
        EffectiveEnvelope::unbounded_for(terms[0].identity()),
        EffectiveEnvelope::intersect,
    )
}
```

For V1 selectors, normalize `/`-separated resource segments with literals and `*`; resolve `${context.*}` before admission; define subset as literal ≤ matching wildcard and exact equality otherwise. Reject unresolved placeholders and wildcard widening.

- [ ] **Step 4: Implement required/optional admission**

`AdmissionService` obtains available exact locked versions, requests policy candidates, intersects package/deployment/actor/task/session terms, and creates a canonical expiring `CapabilityGrantSet`. Missing `required_for_session_start` terms produce `KF-AUTH-001`; optional misses append a stable safe diagnostic and do not create a grant.

- [ ] **Step 5: Run authority and admission tests**

Run: `rtk cargo test -p kiteframe-provider --test authority --test admission`

Expected: PASS for deny precedence, absence, version/resource/effect/mode/expiry/evidence intersection, required denial, optional omission, and canonical grant digest.

- [ ] **Step 6: Commit monotonic admission**

```bash
rtk git add crates/kiteframe-provider
rtk git commit -m "feat: admit monotonic capability grants"
```

### Task 2: Define replaceable authorization and semantic operation interfaces

**Files:**
- Create: `crates/kiteframe-provider/src/authorization.rs`
- Create: `crates/kiteframe-provider/src/operation.rs`
- Modify: `crates/kiteframe-provider/src/lib.rs`
- Test: `crates/kiteframe-provider/tests/interfaces.rs`

**Interfaces:**
- Consumes: actor/agent/task/session refs, exact capability identity, selected resource, trace and policy context.
- Produces: `AuthorizationBackend`, `AuthorizationDecision`, `CapabilityOperation`, `OperationRegistry`, and `InvocationContext`.

- [ ] **Step 1: Write failing registry and current-check tests**

```rust
#[tokio::test]
async fn duplicate_operation_registration_is_rejected() {
    let mut registry = OperationRegistry::new();
    registry.register(read_operation()).unwrap();
    let error = registry.register(read_operation()).unwrap_err();
    assert_eq!(error.code.as_str(), "KF-RUNTIME-001");
}

#[tokio::test]
async fn invocation_uses_current_check_not_admission_decision() {
    let backend = FakeAuthorizationBackend::new()
        .with_admission_allow("cases.read")
        .with_invocation_deny("cases.read");
    let service = provider_with_backend(backend);
    let error = service.invoke(valid_read_request()).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-003");
}
```

- [ ] **Step 2: Run interface tests**

Run: `rtk cargo test -p kiteframe-provider --test interfaces`

Expected: FAIL because the backend and operation interfaces do not exist.

- [ ] **Step 3: Add the authorization interface**

```rust
#[async_trait::async_trait]
pub trait AuthorizationBackend: Send + Sync {
    async fn list_admissible(
        &self,
        request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, Diagnostic>;

    async fn check(
        &self,
        request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, Diagnostic>;

    async fn revision(&self) -> Result<PolicyRevision, Diagnostic>;
}
```

`AuthorizationDecision::Allow` contains a decision reference, model ID, policy revision, decided timestamp, and narrowed conditions. `Deny` contains only a safe reason category and decision reference.

- [ ] **Step 4: Add trusted exact-version operation registration**

```rust
#[async_trait::async_trait]
pub trait CapabilityOperation: Send + Sync {
    fn identity(&self) -> &CapabilityIdentity;
    async fn validate_preconditions(
        &self,
        context: &InvocationContext,
        preconditions: &[Precondition],
    ) -> Result<(), Diagnostic>;
    async fn execute(
        &self,
        context: &InvocationContext,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, OperationFailure>;
}
```

`OperationRegistry` is deployment-built, rejects duplicate exact identities, and freezes before service startup. It never loads a module named by a manifest, lock, or request.

- [ ] **Step 5: Run interface tests**

Run: `rtk cargo test -p kiteframe-provider --test interfaces`

Expected: PASS.

- [ ] **Step 6: Commit provider extension interfaces**

```bash
rtk git add crates/kiteframe-provider/src crates/kiteframe-provider/tests/interfaces.rs
rtk git commit -m "feat: define provider authorization seams"
```

### Task 3: Build the real OpenFGA reference backend

**Files:**
- Create: `crates/kiteframe-openfga/Cargo.toml`
- Create: `crates/kiteframe-openfga/src/lib.rs`
- Create: `crates/kiteframe-openfga/src/client.rs`
- Create: `crates/kiteframe-openfga/src/mapping.rs`
- Create: `crates/kiteframe-openfga/src/freshness.rs`
- Create: `openfga/authorization-model.fga`
- Create: `openfga/test-tuples.yaml`
- Test: `crates/kiteframe-openfga/tests/openfga_contract.rs`

**Interfaces:**
- Consumes: `AuthorizationBackend` and deployment `OpenFgaConfig`.
- Produces: `OpenFgaAuthorizationBackend`.

- [ ] **Step 1: Write failing request-mapping and outage tests**

```rust
#[tokio::test]
async fn admission_uses_list_objects_with_pinned_model() {
    let server = fake_openfga();
    let backend = backend_for(&server);
    backend.list_admissible(&admission_auth_request()).await.unwrap();
    let request = server.last_request();
    assert_eq!(request.path(), "/stores/store-1/list-objects");
    assert_eq!(request.json()["authorization_model_id"], "model-1");
}

#[tokio::test]
async fn invocation_uses_higher_consistency_check() {
    let server = fake_openfga();
    let backend = backend_for(&server);
    backend.check(&invocation_auth_request()).await.unwrap();
    let request = server.last_request();
    assert_eq!(request.path(), "/stores/store-1/check");
    assert_eq!(request.json()["consistency"], "HIGHER_CONSISTENCY");
}

#[tokio::test]
async fn outage_or_stale_revision_fails_closed() {
    let backend = unavailable_backend();
    let error = backend.check(&invocation_auth_request()).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-004");
}
```

- [ ] **Step 2: Run OpenFGA tests**

Run: `rtk cargo test -p kiteframe-openfga --test openfga_contract`

Expected: FAIL because the reference backend does not exist.

- [ ] **Step 3: Add the agent/task/session authorization model**

```fga
model
  schema 1.1

type actor

type task
  relations
    define actor: [actor]

type agent
  relations
    define assigned_task: [task]

type session
  relations
    define task: [task]

type capability
  relations
    define allowed_actor: [actor]
    define allowed_task: [task, session#task, agent#assigned_task]
    define can_invoke: allowed_actor and allowed_task

type resource
  relations
    define capability: [capability]
    define can_invoke: can_invoke from capability
```

Use contextual tuples to bind exact capability/resource, calling agent, task, session, and ephemeral conditions. Actor and task checks remain distinct intersections.

- [ ] **Step 4: Implement `ListObjects` and `Check` requests**

Configure reqwest with rustls, redirects disabled, fixed base origin, bounded bodies, and timeouts. Always include `authorization_model_id`, condition context, contextual tuples, and current timestamp. Record the pinned model ID and a deployment policy revision in every allow decision.

- [ ] **Step 5: Run mock and container-backed tests**

Run: `rtk cargo test -p kiteframe-openfga --test openfga_contract`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-openfga --features container-tests --test openfga_container`

Expected: PASS for actor/task/agent/session/resource relations, `ListObjects`, point-of-use `Check`, contextual tuples, expiry conditions, revocation after admission, model migration, unavailable service, and stale policy.

- [ ] **Step 6: Commit the OpenFGA reference backend**

```bash
rtk git add crates/kiteframe-openfga openfga
rtk git commit -m "feat: add openfga authorization backend"
```

### Task 4: Validate invocation schemas, grants, freshness, evidence, and preconditions

**Files:**
- Create: `crates/kiteframe-provider/src/invocation.rs`
- Modify: `crates/kiteframe-provider/src/lib.rs`
- Test: `crates/kiteframe-provider/tests/invocation_validation.rs`

**Interfaces:**
- Consumes: locked descriptor, `InvocationRequest`, `CapabilityGrantSet`, `AuthorizationBackend`, `CapabilityOperation`.
- Produces: `InvocationService::invoke` validation stages through the point immediately before effect execution.

- [ ] **Step 1: Write failing ordered-validation tests**

```rust
#[tokio::test]
async fn expired_grant_fails_before_authorization_or_operation() {
    let service = instrumented_provider();
    let error = service.invoke(request_with_expired_grant()).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-002");
    assert_eq!(service.events(), ["validate_request", "validate_grant"]);
}

#[tokio::test]
async fn stale_policy_fails_before_precondition_or_effect() {
    let service = instrumented_provider_with_stale_policy();
    let error = service.invoke(valid_effect_request()).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-004");
    assert!(!service.events().contains(&"execute"));
}

#[tokio::test]
async fn invalid_result_is_never_returned() {
    let service = provider_with_operation_result(json!({"unexpected": true}));
    let error = service.invoke(valid_read_request()).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-002");
}
```

- [ ] **Step 2: Run invocation validation tests**

Run: `rtk cargo test -p kiteframe-provider --test invocation_validation`

Expected: FAIL because the invocation state machine is undefined.

- [ ] **Step 3: Implement the pre-execution validation order**

```rust
pub async fn invoke(
    &self,
    request: InvocationRequest,
) -> Result<InvocationOutcome, Diagnostic> {
    let descriptor = self.locked_descriptor(&request.capability)?;
    validate_input_schema(descriptor, &request.arguments)?;
    let grant = self.validate_grant(&request)?;
    self.validate_freshness(grant, descriptor).await?;
    self.validate_resource(grant, descriptor, &request.selected_resource)?;
    self.validate_evidence(descriptor, &request.evidence)?;
    let operation = self.operations.resolve(&request.capability)?;
    operation.validate_preconditions(&self.context(&request), &request.preconditions).await?;
    let decision = self.authorization.check(&request.into_authorization()).await?;
    self.continue_after_authorization(request, descriptor, operation, decision).await
}
```

Denial maps to `KF-AUTH-003`; stale or unprovable current policy maps to `KF-AUTH-004`; missing/stale preconditions map to `KF-CAP-001`.

- [ ] **Step 4: Add confirmation, approval, and consent distinction**

Validate evidence type, subject/approver identity, action/resource binding, issue/expiry times, and protected reference form independently. Return `Suspended` only for descriptors that permit `suspendable` mode; never treat prompt text as evidence.

- [ ] **Step 5: Run validation tests**

Run: `rtk cargo test -p kiteframe-provider --test invocation_validation`

Expected: PASS for request/result schemas, expiry, freshness, selector, evidence, preconditions, denial, suspension, and invalid output.

- [ ] **Step 6: Commit invocation validation**

```bash
rtk git add crates/kiteframe-provider/src/invocation.rs crates/kiteframe-provider/tests/invocation_validation.rs
rtk git commit -m "feat: validate capability invocations"
```

### Task 5: Persist idempotency reservations and status before executing effects

**Files:**
- Create: `crates/kiteframe-provider/src/status.rs`
- Create: `crates/kiteframe-provider-sqlite/Cargo.toml`
- Create: `crates/kiteframe-provider-sqlite/migrations/0001_invocations.sql`
- Create: `crates/kiteframe-provider-sqlite/src/lib.rs`
- Test: `crates/kiteframe-provider/tests/idempotency.rs`
- Test: `crates/kiteframe-provider-sqlite/tests/restart.rs`

**Interfaces:**
- Consumes: validated invocation and descriptor idempotency contract.
- Produces: `InvocationStore`, `InvocationReservation`, `reserve_or_get`, `transition`, `status`.

- [ ] **Step 1: Write failing duplicate and restart tests**

```rust
#[tokio::test]
async fn same_scope_and_key_deduplicates_effect() {
    let service = provider_with_counting_effect();
    let first = service.invoke(effect_request("key-1")).await.unwrap();
    let second = service.invoke(effect_request("key-1")).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(service.effect_count(), 1);
}

#[tokio::test]
async fn unknown_outcome_rejects_new_key_until_resolved() {
    let service = provider_with_unknown_effect();
    service.invoke(effect_request("key-1")).await.unwrap();
    let error = service.invoke(effect_request("key-2")).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-003");
    assert_eq!(error.retry, RetryClass::StatusFirst);
}

#[tokio::test]
async fn status_survives_store_restart() {
    let path = temporary_database();
    write_unknown_status(&path, "inv-1").await;
    let reopened = SqliteInvocationStore::open(&path).await.unwrap();
    assert_eq!(reopened.status("inv-1").await.unwrap().state, StatusState::OutcomeUnknown);
}
```

- [ ] **Step 2: Run idempotency tests**

Run: `rtk cargo test -p kiteframe-provider --test idempotency`

Expected: FAIL because no invocation store exists.

- [ ] **Step 3: Define durable reservation and transition semantics**

```rust
#[async_trait::async_trait]
pub trait InvocationStore: Send + Sync {
    async fn reserve_or_get(
        &self,
        scope: IdempotencyScopeValue,
        key: IdempotencyKey,
        retention_until: Timestamp,
    ) -> Result<InvocationReservation, Diagnostic>;

    async fn transition(
        &self,
        invocation_id: &InvocationId,
        expected: InvocationState,
        next: InvocationState,
    ) -> Result<(), Diagnostic>;

    async fn status(
        &self,
        invocation_id: &InvocationId,
    ) -> Result<InvocationStatus, Diagnostic>;
}
```

The unique database key is `(actor, capability_name, capability_version, normalized_resource, semantic_operation, idempotency_key)`. State transitions use compare-and-swap transactions.

- [ ] **Step 4: Implement SQLite restart and retention behavior**

Persist request digest, current state, safe terminal result/error, audit record IDs, policy revision, created/updated time, and retention deadline. Never persist raw credentials or evidence bodies.

- [ ] **Step 5: Run idempotency and restart tests**

Run: `rtk cargo test -p kiteframe-provider --test idempotency`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-provider-sqlite --test restart`

Expected: PASS for deduplication, concurrent duplicate requests, restart, retention, status-first retry, and explicit abandonment authorization.

- [ ] **Step 6: Commit durable invocation status**

```bash
rtk git add crates/kiteframe-provider/src/status.rs crates/kiteframe-provider-sqlite
rtk git commit -m "feat: persist capability invocation status"
```

### Task 6: Enforce write-ahead audit receipts and hash-chained outcomes

**Files:**
- Create: `crates/kiteframe-provider/src/audit.rs`
- Create: `crates/kiteframe-audit/Cargo.toml`
- Create: `crates/kiteframe-audit/src/lib.rs`
- Create: `crates/kiteframe-audit/src/chain.rs`
- Modify: `crates/kiteframe-provider/src/invocation.rs`
- Test: `crates/kiteframe-provider/tests/audit_ordering.rs`
- Test: `crates/kiteframe-audit/tests/integrity.rs`

**Interfaces:**
- Consumes: current allow decision, invocation reservation, operation outcome, package/lock/binding/resolved digests, trace IDs.
- Produces: `AuditSink`, `AuditRecord`, `DurableAuditReceipt`, `FileAuditLedger`.

- [ ] **Step 1: Write failing effect-order and outage tests**

```rust
#[tokio::test]
async fn write_ahead_receipt_precedes_effect() {
    let service = instrumented_effect_provider();
    service.invoke(valid_effect_request()).await.unwrap();
    assert_eq!(
        service.events(),
        ["authorize", "reserve", "audit_authorization", "execute", "audit_outcome", "terminal_status"]
    );
}

#[tokio::test]
async fn audit_outage_blocks_effect() {
    let service = provider_with_failing_audit();
    let error = service.invoke(valid_effect_request()).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUDIT-001");
    assert_eq!(service.effect_count(), 0);
}

#[tokio::test]
async fn outcome_append_failure_marks_status_unknown() {
    let service = provider_with_second_audit_append_failure();
    let outcome = service.invoke(valid_effect_request()).await.unwrap();
    assert!(matches!(outcome, InvocationOutcome::OutcomeUnknown { .. }));
    assert_eq!(service.effect_count(), 1);
}
```

- [ ] **Step 2: Run audit tests**

Run: `rtk cargo test -p kiteframe-provider --test audit_ordering`

Expected: FAIL because audit interfaces and ordering are undefined.

- [ ] **Step 3: Define complete authorization and outcome records**

```rust
pub struct AuthorizationAuditRecord {
    pub actor: ActorRef,
    pub agent: AgentRef,
    pub task: TaskRef,
    pub session: SessionRef,
    pub capability: CapabilityIdentity,
    pub resource: NormalizedResourceSelector,
    pub admission_id: AdmissionId,
    pub grant_digest: Sha256Digest,
    pub policy_revision: PolicyRevision,
    pub decision_reference: DecisionRef,
    pub idempotency_key: IdempotencyKey,
    pub precondition_refs: Vec<PreconditionRef>,
    pub evidence_refs: EvidenceReferences,
    pub portable_digest: Sha256Digest,
    pub lock_digest: Sha256Digest,
    pub binding_digest: Sha256Digest,
    pub resolved_digest: Sha256Digest,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub intended_effect: EffectClassification,
    pub timestamp: Timestamp,
}
```

Outcome records are `completion`, `failure`, `suspension`, or `outcome_unknown` and contain the write-ahead record ID.

- [ ] **Step 4: Implement append-only partition chains**

Each partition append locks the partition, reads the last `(sequence, hash)`, computes `record_hash = SHA256("kiteframe:audit:v1\0" || partition || sequence || previous_hash || canonical_record)`, writes one JSON line, flushes, calls `sync_data`, and only then returns `DurableAuditReceipt`.

- [ ] **Step 5: Run audit ordering and integrity tests**

Run: `rtk cargo test -p kiteframe-provider --test audit_ordering`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-audit --test integrity`

Expected: PASS for sequence, hash chain, concurrent partitions, tamper detection, trace correlation, and restart.

- [ ] **Step 6: Commit mandatory audit ordering**

```bash
rtk git add crates/kiteframe-provider crates/kiteframe-audit
rtk git commit -m "feat: audit effects before execution"
```

### Task 7: Expose the four TLS HTTP routes with stable bodies and trace propagation

**Files:**
- Create: `crates/kiteframe-provider-http/Cargo.toml`
- Create: `crates/kiteframe-provider-http/src/lib.rs`
- Create: `crates/kiteframe-provider-http/src/routes.rs`
- Create: `crates/kiteframe-provider-http/src/response.rs`
- Create: `crates/kiteframe-provider-http/src/trace.rs`
- Create: `crates/kiteframe-provider-http/src/main.rs`
- Create: `crates/kiteframe-provider-http/tests/http_profile.rs`
- Create: `tests/provider/docker-compose.yml`
- Create: `tests/provider/fixtures/`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: catalog, admission, invocation, and status services.
- Produces: `provider_router(...)` and `kiteframe-provider` TLS server binary.

- [ ] **Step 1: Write failing route and transport tests**

```rust
#[tokio::test]
async fn exact_v1_routes_return_stable_contract_bodies() {
    let app = provider_router(test_services());
    assert_contract(app.clone(), Method::GET, "/v1/capability-catalog").await;
    assert_contract(app.clone(), Method::POST, "/v1/capability-admissions").await;
    assert_contract(app.clone(), Method::POST, "/v1/capability-invocations/cases.read").await;
    assert_contract(app, Method::GET, "/v1/capability-invocations/inv-1").await;
}

#[tokio::test]
async fn catalog_etag_revalidation_returns_304() {
    let app = provider_router(test_services());
    let first = request(&app, "/v1/capability-catalog").await;
    let etag = first.headers()["etag"].clone();
    let second = request_with_header(&app, "/v1/capability-catalog", "if-none-match", etag).await;
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
}
```

- [ ] **Step 2: Run HTTP profile tests**

Run: `rtk cargo test -p kiteframe-provider-http --test http_profile`

Expected: FAIL because the HTTP crate does not exist.

- [ ] **Step 3: Implement exact routes and stable error envelopes**

```rust
pub fn provider_router(state: ProviderHttpState) -> Router {
    Router::new()
        .route("/v1/capability-catalog", get(catalog))
        .route("/v1/capability-admissions", post(admit))
        .route("/v1/capability-invocations/{name}", post(invoke))
        .route("/v1/capability-invocations/{invocation_id}", get(status))
        .layer(DefaultBodyLimit::max(1_048_576))
        .with_state(state)
}
```

Transport status codes distinguish malformed request, not found, conflict, timeout, and service failure, but every non-304 response body is a native success value or structured Kiteframe diagnostic envelope.

- [ ] **Step 4: Add TLS, trace, baggage, and origin rules**

The binary requires certificate/key paths and refuses plaintext non-loopback startup. A test-only `--insecure-loopback` flag binds only `127.0.0.1`. Parse and forward `traceparent`/`tracestate`; keep only deployment-allowlisted baggage keys and reject sensitive names.

- [ ] **Step 5: Run full enforcement-plane verification**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `rtk cargo test --workspace --all-features`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-openfga --features container-tests`

Expected: PASS with Docker available.

- [ ] **Step 6: Commit the HTTP profile and Wave 5 gate**

```bash
rtk git add crates/kiteframe-provider-http tests/provider .github/workflows/ci.yml
rtk git commit -m "feat: serve capability provider profile"
```

## Wave 5 Exit Criteria

- Authority intersection is monotonic across capability version, selector, effect, execution mode, expiry, freshness, and evidence.
- Admission and point-of-use authorization are separate observable calls.
- The OpenFGA backend uses `ListObjects` only for admission filtering and current higher-consistency `Check` for every invocation.
- Revocation, model migration, stale policy, and OpenFGA outage fail closed.
- Input and output schemas, grant expiry, resource selection, evidence, freshness, and preconditions are validated in a fixed order.
- Idempotency reservations and status survive process restart and deduplicate concurrent duplicate effects.
- Effect execution occurs only after a durable authorization audit receipt; audit outage blocks the effect.
- Outcome append failure becomes `outcome_unknown`, and retry remains status-first with the same key.
- The four standardized TLS routes return stable native bodies and propagate filtered W3C context.
