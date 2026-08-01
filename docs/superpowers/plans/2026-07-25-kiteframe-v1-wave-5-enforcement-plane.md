# Kiteframe V1 Wave 5 Capability Enforcement Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the standardized capability provider on the mandatory Wave 3R contract closure, with authenticated dual-principal admission, monotonic authority, current point-of-use authorization, a real OpenFGA reference backend, schema-validated invocation, durable idempotency/status, and write-ahead audit ordering.

**Architecture:** Wave 5 starts only after Wave 3R lands the canonical locked-capability, effective-grant, authority-revision, status, client catalog-fetch, client provider-authentication, and suspension contracts. `kiteframe-provider` owns the authoritative `CapabilityCatalog` and locked descriptor registry and is a Rust state machine over the applicable shared contracts plus pluggable authorization, operation, invocation-store, and audit interfaces; it does not define shadow grant or descriptor types or depend on runtime assembly state. `kiteframe-provider-http` uses a server-owned `ProviderPrincipalVerifier` to authenticate separately verified human and workload principals before route/application logic. The Wave 3R client-side `ProviderAuthenticator` supplies the credential headers consumed by that verifier, but is not a server interface. `kiteframe-openfga` implements the replaceable reference authorization backend through pinned store/model configuration and current `Check` calls.

**Tech Stack:** Rust 1.97.1, Tokio, axum, reqwest/rustls, jsonschema, async-trait, OpenFGA HTTP API, testcontainers, SQLite for the reference invocation store, append-only JSONL audit fixture.

## Global Constraints

- Wave 3R (`docs/superpowers/plans/2026-07-28-kiteframe-v1-wave-3r-contract-closure.md`) is a hard prerequisite. Do not create the Wave 5 crates until its contract/schema/stub gate passes.
- Consume the applicable exact Wave 3R contracts: `ResolvedCapabilityRequirement` containing exact `LockedCapability`; `EffectiveCapabilityGrant`; `AuthorityRevisionSet`; `StatusRequest`; and the expanded `Suspension`. `ProviderAuthenticator` and `CatalogFetchResult` remain client-side contracts: the former supplies credential headers and the latter interprets catalog HTTP `200`/`304` responses.
- The provider owns the authoritative `CapabilityCatalog` and locked descriptor registry. Admission exact-matches the client's expected catalog identity/digest and resolved requirements against that provider-owned state, then persists the exact provider-validated `LockedCapability` with the admission.
- Do not introduce an internal `EffectiveGrant`, re-fetch a descriptor already persisted with an admission, reconstruct runtime inputs in provider code, or make the provider depend on runtime-specific assembly state.
- Visible authority equals package requirements ∩ deployment policy ∩ actor authority ∩ task/session grants ∩ available locked catalog versions.
- Explicit deny wins, absence is deny, and narrower resource, expiry, evidence, effect, or execution-mode terms win.
- Each `EffectiveCapabilityGrant` preserves exact identity, normalized resources, narrowed execution modes, maximum effect, per-capability expiry, required evidence, freshness, and preconditions.
- Every admission proves the expected catalog identity and digest, resolves every required capability, records optional-denial diagnostics, and emits a canonical `AuthorityRevisionSet` plus digest.
- `AuthorityRevisionSet` is provider-produced state, never client-supplied authority. Invocation binds to a persisted admission by `admission_id` plus the canonical grant-set `grant_digest`; the provider loads the admitted grant/revision snapshot and obtains current revisions from authorization backends before every read or effect.
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
- Every provider route authenticates before route/application logic. Human and workload identities are verified separately; after native request decoding, admission/invocation/status correlate verified tenant/human/workload/run context with every portable actor/agent/task/session/admission reference carried by that request, and any mismatch fails closed.
- Credentials establish transport principals but are never portable authority, grant material, descriptor content, telemetry baggage, diagnostics, status data, or audit payload.
- Provider-internal row/field ACLs may narrow data before serialization. Portable capability descriptors and schemas expose only stable semantic projections, never provider ACL rule names, legacy fields, or presentation/session shapes.
- Suspension and resume evidence is bound to the canonical proposal digest; resume revalidates evidence kind/reference, grant/catalog/descriptor/authority revisions, freshness, preconditions, and point-of-use authorization.

---

## File Structure

```text
crates/kiteframe-provider/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # Provider service facade
    ├── authority.rs                           # Resource/effect/evidence envelope intersection
    ├── admission.rs                           # Grant-set construction and required/optional behavior
    ├── principal.rs                           # Verified transport/portable identity correlation
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
    ├── auth.rs                                # ProviderPrincipalVerifier middleware and credential stripping
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
└── fixtures/
    ├── catalog-policy-effects/                 # Catalog, policy, effects, evidence, failures
    └── crankshaft-profile/                     # Provider-neutral workforce-shaped conformance corpus
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
- Consumes: provider-owned authoritative `CapabilityCatalog` and locked descriptor registry; `AdmissionRequest` carrying expected catalog identity/digest and Wave 3R `ResolvedCapabilityRequirement { locked_capability: LockedCapability, .. }`; canonical `EffectiveCapabilityGrant`, `AuthorityRevisionSet`, and diagnostics.
- Produces: provider-internal `AuthorityTerm`, `intersect_authority(...) -> Result<Option<EffectiveCapabilityGrant>, Vec<Diagnostic>>`, and `AdmissionService::admit(...)`; the only emitted grant value is canonical `EffectiveCapabilityGrant`, and the admission persists each exact provider-validated `LockedCapability`.

- [ ] **Step 1: Write failing deny-precedence and monotonicity tests**

```rust
#[test]
fn explicit_deny_wins_over_allows() {
    let terms = vec![
        AuthorityTerm::allow(grant("cases.read", "tenant:t1/case:*")),
        AuthorityTerm::deny("cases.read"),
        AuthorityTerm::allow(grant("cases.read", "tenant:t1/case:case-1")),
    ];
    assert!(intersect_authority(&resolved_requirement(), &terms).unwrap().is_none());
}

#[test]
fn narrower_resource_expiry_and_evidence_win() {
    let effective = intersect_authority(&resolved_requirement(), &[
        term("tenant:t1/case:*", HOUR_2, Evidence::Confirmation),
        term("tenant:t1/case:case-7", HOUR_1, Evidence::Approval),
    ]).unwrap().unwrap();
    assert_eq!(effective.resources(), ["tenant:t1/case:case-7"]);
    assert_eq!(effective.expires_at(), HOUR_1);
    assert_eq!(effective.required_evidence(), Evidence::Approval);
    assert_eq!(effective.maximum_effect(), EffectClassification::ReadOnly);
}

proptest! {
    #[test]
    fn adding_a_restriction_never_increases_envelope(
        base in authority_term_strategy(),
        restriction in narrower_term_strategy(),
    ) {
        let requirement = resolved_requirement();
        let before = intersect_authority(&requirement, std::slice::from_ref(&base))
            .unwrap()
            .unwrap();
        let after = intersect_authority(&requirement, &[base, restriction])
            .unwrap()
            .unwrap();
        prop_assert!(after.is_subset_of(&before));
    }
}
```

- [ ] **Step 2: Run authority tests**

Run: `rtk cargo test -p kiteframe-provider --test authority`

Expected: FAIL because the provider crate does not exist.

- [ ] **Step 3: Implement selector and envelope partial ordering**

```rust
pub fn intersect_authority(
    requirement: &ResolvedCapabilityRequirement,
    terms: &[AuthorityTerm],
) -> Result<Option<EffectiveCapabilityGrant>, Vec<Diagnostic>> {
    if terms.is_empty() || terms.iter().any(AuthorityTerm::is_explicit_deny) {
        return Ok(None);
    }
    EffectiveCapabilityGrant::intersect(
        requirement.locked_capability(),
        terms.iter().map(AuthorityTerm::allow_value),
    ).map(Some)
}
```

For V1 selectors, normalize `/`-separated resource segments with literals and `*`; resolve `${context.*}` before admission; define subset as literal ≤ matching wildcard and exact equality otherwise. Reject unresolved placeholders and wildcard widening.

- [ ] **Step 4: Implement catalog-bound required/optional admission**

`AdmissionService` first exact-matches the request's expected catalog identity and digest to the provider-owned authoritative `CapabilityCatalog`. It then walks every `ResolvedCapabilityRequirement`, exact-matches its embedded `LockedCapability` against the provider-owned locked descriptor registry, persists that exact provider-validated lock with the admission, intersects package/deployment/human/workload/task/session terms, and emits one `EffectiveCapabilityGrant` per admitted capability. A client-resolved requirement is evidence of what the caller selected, never provider catalog authority. Missing required terms produce `KF-AUTH-001`; optional misses append a stable safe diagnostic and do not create a grant. Canonically sort source/revision entries into `AuthorityRevisionSet`, compute its digest, and include it plus per-capability expiry in the grant set.

```rust
#[tokio::test]
async fn admission_proves_catalog_and_all_required_capabilities() {
    let service = service();
    let result = service.admit(admission_request()).await.unwrap();
    assert_eq!(result.catalog_identity(), expected_catalog_identity());
    assert_eq!(result.catalog_digest(), expected_catalog_digest());
    assert_eq!(result.grants().len(), 2);
    assert_eq!(result.optional_denials()[0].code.as_str(), "KF-AUTH-001");
    assert_eq!(
        result.authority_revisions().entries(),
        [
            revision("deployment-policy", "deploy-7"),
            revision("openfga-model", "model-3"),
            revision("tenant-policy", "tenant-42"),
        ]
    );
    assert_eq!(
        result.authority_revision_digest(),
        result.authority_revisions().digest()
    );
    let persisted = service
        .load_admission(result.admission_id(), result.grant_digest())
        .await
        .unwrap();
    assert_eq!(
        persisted.locked_capability(&capability_identity("cases.read", "1.0.0")),
        authoritative_registry().locked_capability("cases.read", "1.0.0"),
    );
}

#[tokio::test]
async fn client_lock_drift_from_provider_registry_fails_closed() {
    let request = admission_request_with_tampered_locked_descriptor();
    let error = service().admit(request).await.unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-001");
}
```

- [ ] **Step 5: Run authority and admission tests**

Run: `rtk cargo test -p kiteframe-provider --test authority --test admission`

Expected: PASS for catalog identity/digest mismatch, deny precedence, absence, exact locked-version/resource/effect/mode/per-capability-expiry/evidence/freshness/precondition intersection, required denial, optional diagnostics, canonical authority-revision ordering/digest, and canonical grant digest.

- [ ] **Step 6: Commit monotonic admission**

```bash
rtk git add crates/kiteframe-provider
rtk git commit -m "feat: admit monotonic capability grants"
```

### Task 2: Correlate authenticated principals and define provider extension interfaces

**Files:**
- Create: `crates/kiteframe-provider/src/principal.rs`
- Create: `crates/kiteframe-provider/src/authorization.rs`
- Create: `crates/kiteframe-provider/src/operation.rs`
- Modify: `crates/kiteframe-provider/src/lib.rs`
- Test: `crates/kiteframe-provider/tests/principal_boundary.rs`
- Test: `crates/kiteframe-provider/tests/interfaces.rs`

**Interfaces:**
- Consumes: server-produced `VerifiedProviderPrincipals` containing separately verified human/workload principals; portable actor/agent/task/session/admission refs; exact capability identity; selected resource; trace context; and provider-loaded persisted admission/grant/revision state.
- Produces: provider-internal `AuthenticatedInvocationContext`, `correlate_principals(...)`, `AuthorizationBackend`, `AuthorizationDecision`, `CapabilityOperation`, `OperationRegistry`, and `InvocationContext`.

- [ ] **Step 1: Write failing dual-principal, mismatch, registry, and current-check tests**

```rust
#[test]
fn independently_verified_human_and_workload_are_both_required() {
    let context = correlate_principals(
        verified_human("tenant-1", "human-7"),
        verified_workload("tenant-1", "harness-2", "run-9"),
        portable_refs("actor-7", "agent-2", "task-4", "session-3", "admission-5"),
    ).unwrap();
    assert_eq!(context.tenant_ref().as_str(), "tenant-1");
    assert_eq!(context.human_ref().as_str(), "human-7");
    assert_eq!(context.workload_ref().as_str(), "harness-2");
    assert_eq!(context.run_ref().as_str(), "run-9");
}

#[test]
fn verified_tenant_or_subject_mismatch_fails_closed() {
    let error = correlate_principals(
        verified_human("tenant-1", "human-7"),
        verified_workload("tenant-2", "harness-2", "run-9"),
        portable_refs("actor-other", "agent-2", "task-4", "session-3", "admission-5"),
    ).unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-003");
}

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

Run: `rtk cargo test -p kiteframe-provider --test principal_boundary --test interfaces`

Expected: FAIL because principal correlation, backend, and operation interfaces do not exist.

- [ ] **Step 3: Add fail-closed principal correlation and the authorization interface**

`AuthenticatedInvocationContext` is created only from separately verified human and workload principal values returned by the server-side `ProviderPrincipalVerifier` as `VerifiedProviderPrincipals`. Correlation exact-matches verified tenant, human-to-portable-actor mapping, workload-to-portable-agent mapping, run/task/session/admission bindings, and expiries. It stores opaque principal references, never bearer tokens, cookies, API keys, raw claims, or caller-supplied tenant/user fields. The human principal remains the business actor; the workload principal is a separately constrained calling identity and provenance term, never substitute authority.

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

    async fn revisions(&self) -> Result<AuthorityRevisionSet, Diagnostic>;
}
```

`AuthorizationDecision::Allow` contains a decision reference, canonical `AuthorityRevisionSet`, decided timestamp, and narrowed conditions. `Deny` contains only a safe reason category and decision reference. Every request to this interface includes the correlated authenticated principal references and portable actor/agent/task/session/admission refs.

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

Provider operations may apply deployment-internal row and field ACLs before producing a result. The result returned to Kiteframe must validate against the embedded locked output schema and contain only the stable semantic projection. `FieldDecision`, legacy DTO fields, UI field IDs, and policy rule names never cross the provider boundary.

- [ ] **Step 5: Run interface tests**

Run: `rtk cargo test -p kiteframe-provider --test principal_boundary --test interfaces`

Expected: PASS for dual-principal verification, tenant/human/workload/run correlation, portable-ref mismatch denial, credential non-retention, current authorization, exact operation registration, and stable projection output.

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
- Test: `crates/kiteframe-openfga/tests/openfga_container.rs`

**Interfaces:**
- Consumes: `AuthorizationBackend`, `AuthorityRevisionSet`, authenticated human/workload correlation, and deployment `OpenFgaConfig`.
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

#[tokio::test]
async fn revision_set_records_model_store_and_tenant_policy_sources() {
    let revisions = backend_for(&fake_openfga()).revisions().await.unwrap();
    assert_eq!(
        revisions.entries(),
        [
            revision("openfga-model", "model-1"),
            revision("openfga-store", "store-1"),
            revision("tenant-policy", "tenant-policy-7"),
        ]
    );
    assert_eq!(revisions.digest(), canonical_authority_revision_digest());
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

type workload
  relations
    define actor: [actor]

type task
  relations
    define actor: [actor]

type agent
  relations
    define assigned_task: [task]
    define actor: actor from assigned_task

type session
  relations
    define task: [task]
    define actor: actor from task

type capability
  relations
    define allowed_actor: [actor]
    define allowed_task_actor: [task#actor, session#actor, agent#actor]
    define allowed_workload_actor: [workload#actor]
    define can_invoke: allowed_actor and allowed_task_actor and allowed_workload_actor

type resource
  relations
    define capability: [capability]
    define can_invoke: can_invoke from capability
```

The checked OpenFGA user is the correlated business actor. Stored, pinned policy tuples grant the capability/resource and its actor, task/session/agent-actor, and workload-actor relationships; removing any such tuple revokes access. Contextual tuples bind only separately verified workload/run, actor/task/session/agent/admission provenance, and ephemeral conditions—they never create capability, resource, or policy-grant edges. Human, workload, and task checks remain distinct intersections; a workload allow never substitutes for missing human authority.

- [ ] **Step 4: Implement `ListObjects` and `Check` requests**

Configure reqwest with rustls, redirects disabled, fixed base origin, bounded bodies, and timeouts. Always include `authorization_model_id`, condition context, contextual tuples, and current timestamp. Return canonical source/revision entries for pinned store, authorization model, tenant policy, and any deployment policy source in `AuthorityRevisionSet`; every allow decision records its digest.

- [ ] **Step 5: Run mock and container-backed tests**

Run: `rtk cargo test -p kiteframe-openfga --test openfga_contract`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-openfga --features container-tests --test openfga_container`

Expected: PASS for human/workload/run/task/agent/session/admission/resource relations, `ListObjects`, point-of-use `Check`, canonical authority revisions, contextual tuples, expiry conditions, revocation after admission, model migration, unavailable service, and stale policy.

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
- Consumes: `InvocationRequest` carrying admission ID plus canonical grant-set `grant_digest`; the provider-owned admission record containing each exact provider-validated `LockedCapability`, persisted `EffectiveCapabilityGrant`, and admission `AuthorityRevisionSet`; expanded `Suspension`; authenticated principal context; `AuthorizationBackend`; and `CapabilityOperation`.
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

#[tokio::test]
async fn evidence_for_another_proposal_cannot_resume_effect() {
    let service = provider_with_valid_approval();
    let error = service
        .resume(effect_resume_request("proposal-digest-other"))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!service.events().contains(&"execute"));
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
    let admission = self
        .admissions
        .load(request.admission_id(), request.grant_digest())
        .await?;
    let locked = admission.locked_capability(request.capability())?;
    let descriptor = locked.descriptor();
    locked.validate_complete_semantics(
        descriptor.stable_errors(),
        descriptor.execution_modes(),
        descriptor.effect(),
        descriptor.evidence_requirements(),
        descriptor.freshness(),
        descriptor.preconditions(),
    )?;
    validate_input_schema(descriptor, &request.arguments)?;
    let grant = admission.effective_grant(request.capability())?;
    grant.validate_against_locked_capability(locked)?;
    self.validate_authenticated_context(&request, grant)?;
    let current_revisions = self.authorization.revisions().await?;
    self.validate_freshness(
        grant,
        admission.authority_revisions(),
        &current_revisions,
        descriptor,
    ).await?;
    self.validate_resource(grant, descriptor, &request.selected_resource)?;
    self.validate_evidence(
        descriptor,
        &request.evidence_refs,
        request.proposal_digest(),
    )?;
    let operation = self.operations.resolve(&request.capability)?;
    operation.validate_preconditions(&self.context(&request), &request.preconditions).await?;
    let decision = self.authorization.check(&request.into_authorization()).await?;
    self.continue_after_authorization(request, descriptor, operation, decision).await
}
```

Denial maps to `KF-AUTH-003`; stale or unprovable current policy maps to `KF-AUTH-004`; missing/stale preconditions map to `KF-CAP-001`.

The request never supplies `AuthorityRevisionSet` or revision entries. The provider resolves `admission_id` plus the canonical grant-set `grant_digest` through its persisted admission store, exact-matches actor/agent/task/session and authenticated principal context, and asks each authorization backend for a fresh canonical revision set. Current revision freshness and point-of-use authorization are independent checks; matching the admitted revision digest never authorizes execution.

Invocation never consults client runtime assembly state and never re-resolves a capability from a mutable catalog. The exact `LockedCapability` persisted after provider-side admission validation is the descriptor and version authority for every invocation and resume.

- [ ] **Step 4: Add confirmation, approval, and consent distinction**

Validate evidence type, subject/approver identity, action/resource binding, issue/expiry times, protected reference form, and canonical proposal digest independently. Return `Suspended` only for descriptors whose embedded `LockedCapability` permits `suspendable` mode; never treat prompt text as evidence. Construct the Wave 3R `Suspension` with checkpoint reference, evidence kind, protected evidence-request reference, and proposal digest. Resume exact-matches all four and then revalidates authenticated principals, grant/catalog/descriptor/authority-revision digests, per-capability expiry, evidence, freshness, resource, preconditions, and point-of-use authorization before execution.

- [ ] **Step 5: Run validation tests**

Run: `rtk cargo test -p kiteframe-provider --test invocation_validation`

Expected: PASS for embedded descriptor stable errors/modes/effect/evidence/freshness/preconditions, request/result schemas, per-capability expiry, principal correlation, selector, proposal-bound evidence, authority-revision change, resume revalidation, denial, suspension, and invalid stable-projection output.

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
- Consumes: validated invocation, authenticated principal context, embedded descriptor idempotency contract, `StatusRequest { invocation_id, trace_context }`, grant/catalog/descriptor/authority-revision digests, and proposal/evidence references.
- Produces: `InvocationStore`, `InvocationReservation`, `reserve_or_get`, `transition`, and `status(StatusRequest)`.

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
    let request = StatusRequest::try_new("inv-1", trace_context("00-restart")).unwrap();
    assert_eq!(reopened.status(&request).await.unwrap().state, StatusState::OutcomeUnknown);
    assert_eq!(reopened.last_traceparent(), "00-restart");
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
        request: &StatusRequest,
    ) -> Result<InvocationStatus, Diagnostic>;
}
```

The unique database key is `(actor, capability_name, capability_version, normalized_resource, semantic_operation, idempotency_key)`. State transitions use compare-and-swap transactions.

- [ ] **Step 4: Implement SQLite restart and retention behavior**

Persist request digest, current state, safe terminal result/error, admission ID, canonical grant-set `grant_digest`, catalog identity/digest, descriptor digest, authority-revision-set digest, authenticated human/workload/run references, task/session refs, invocation/status identifiers, idempotency scope/key, proposal digest, protected evidence references, audit authorization/outcome record IDs, created/updated time, and retention deadline. Never persist raw credentials, tokens, cookies, claims, evidence bodies, provider ACL rules, or legacy fields. Status authorization exact-matches the authenticated principal and portable context stored for the invocation before returning data.

- [ ] **Step 5: Run idempotency and restart tests**

Run: `rtk cargo test -p kiteframe-provider --test idempotency`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-provider-sqlite --test restart`

Expected: PASS for deduplication, concurrent duplicate requests, traced `StatusRequest`, principal/context mismatch denial, digest preservation, restart, retention, status-first retry, and explicit abandonment authorization.

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
- Consumes: current allow decision, authenticated principal context, invocation reservation, operation outcome, package/lock/binding/resolved, admission, effective-grant, catalog, descriptor, and authority-revision digests, trace IDs.
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
    pub tenant_ref: TenantRef,
    pub human_principal_ref: HumanPrincipalRef,
    pub workload_principal_ref: WorkloadPrincipalRef,
    pub run_ref: RunRef,
    pub actor: ActorRef,
    pub agent: AgentRef,
    pub task: TaskRef,
    pub session: SessionRef,
    pub capability: CapabilityIdentity,
    pub resource: NormalizedResourceSelector,
    pub admission_id: AdmissionId,
    pub grant_digest: Sha256Digest,
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
    pub descriptor_digest: Sha256Digest,
    pub authority_revision_digest: Sha256Digest,
    pub decision_reference: DecisionRef,
    pub invocation_id: InvocationId,
    pub idempotency_key: IdempotencyKey,
    pub precondition_refs: Vec<PreconditionRef>,
    pub evidence_refs: EvidenceReferences,
    pub proposal_digest: Sha256Digest,
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

Outcome records are `completion`, `failure`, `suspension`, or `outcome_unknown` and contain the write-ahead record ID plus the same admission, grant-set, catalog, descriptor, authority-revision, authenticated-principal, invocation/status state, idempotency, proposal, and trace correlation. Authorization and outcome records never contain credentials, raw claims, evidence bodies, provider ACL rules, legacy DTO fields, arguments, or results outside the approved safe-result contract.

- [ ] **Step 4: Implement append-only partition chains**

Each partition append locks the partition, reads the last `(sequence, hash)`, computes `record_hash = SHA256("kiteframe:audit:v1\0" || partition || sequence || previous_hash || canonical_record)`, writes one JSON line, flushes, calls `sync_data`, and only then returns `DurableAuditReceipt`.

- [ ] **Step 5: Run audit ordering and integrity tests**

Run: `rtk cargo test -p kiteframe-provider --test audit_ordering`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-audit --test integrity`

Expected: PASS for sequence, hash chain, concurrent partitions, tamper detection, complete authorization/outcome linkage, digest and dual-principal correlation, credential absence, trace correlation, and restart.

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
- Create: `crates/kiteframe-provider-http/src/auth.rs`
- Create: `crates/kiteframe-provider-http/src/trace.rs`
- Create: `crates/kiteframe-provider-http/src/main.rs`
- Create: `crates/kiteframe-provider-http/tests/http_profile.rs`
- Create: `tests/provider/docker-compose.yml`
- Create: `tests/provider/fixtures/`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: server-owned `ProviderPrincipalVerifier`, Wave 3R `StatusRequest`, and provider-owned catalog, admission, invocation, and status services.
- Produces: `VerifiedProviderPrincipals`, `provider_router(...)`, and the `kiteframe-provider` TLS server binary.

- [ ] **Step 1: Write failing route and transport tests**

```rust
#[tokio::test]
async fn exact_v1_routes_return_stable_contract_bodies() {
    let app = provider_router(test_services(), allowing_principal_verifier());
    assert_contract(app.clone(), Method::GET, "/v1/capability-catalog").await;
    assert_contract(app.clone(), Method::POST, "/v1/capability-admissions").await;
    assert_contract(app.clone(), Method::POST, "/v1/capability-invocations/cases.read").await;
    assert_contract(app, Method::GET, "/v1/capability-invocations/inv-1").await;
}

#[tokio::test]
async fn catalog_etag_revalidation_returns_304() {
    let app = provider_router(test_services(), allowing_principal_verifier());
    let first = request(&app, "/v1/capability-catalog").await;
    let etag = first.headers()["etag"].clone();
    let second = request_with_header(&app, "/v1/capability-catalog", "if-none-match", etag).await;
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert_body_empty(second).await;
    assert_eq!(catalog_events(), ["catalog_200", "catalog_304"]);
}

#[tokio::test]
async fn every_route_authenticates_and_traces_before_service_logic() {
    let app = provider_router(test_services(), recording_principal_verifier());
    for request in all_four_route_requests("00-route-trace") {
        request(&app, request).await;
    }
    assert_eq!(
        app.events(),
        [
            "trace", "authenticate_human", "authenticate_workload", "catalog",
            "trace", "authenticate_human", "authenticate_workload", "admit",
            "trace", "authenticate_human", "authenticate_workload", "invoke",
            "trace", "authenticate_human", "authenticate_workload", "status",
        ]
    );
}
```

- [ ] **Step 2: Run HTTP profile tests**

Run: `rtk cargo test -p kiteframe-provider-http --test http_profile`

Expected: FAIL because the HTTP crate does not exist.

- [ ] **Step 3: Implement exact routes and stable error envelopes**

```rust
#[async_trait::async_trait]
pub trait ProviderPrincipalVerifier: Send + Sync {
    async fn verify(
        &self,
        headers: &HeaderMap,
    ) -> Result<VerifiedProviderPrincipals, Diagnostic>;
}

pub fn provider_router(
    state: ProviderHttpState,
    principal_verifier: Arc<dyn ProviderPrincipalVerifier>,
) -> Router {
    Router::new()
        .route("/v1/capability-catalog", get(catalog))
        .route("/v1/capability-admissions", post(admit))
        .route("/v1/capability-invocations/{name}", post(invoke))
        .route("/v1/capability-invocations/{invocation_id}", get(status))
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(from_fn_with_state(
            ProviderAuthState::new(principal_verifier),
            authenticate_provider_request,
        ))
        .layer(from_fn(extract_trace_context))
        .with_state(state)
}
```

`authenticate_provider_request` passes credential-bearing headers to the deployment's server-side `ProviderPrincipalVerifier`, which verifies human and workload credentials independently and returns `VerifiedProviderPrincipals`. The middleware then strips credential-bearing headers and exposes only opaque authenticated principal refs before calling any route logic. Catalog, admission, invocation, and status all use this boundary. The Wave 3R client-side `ProviderAuthenticator` only supplies the credential headers consumed by this verifier; the client hook and server verifier share neither an interface nor authority.

After native request decoding, admission/invocation/status correlate the authenticated tenant/human/workload/run refs with all portable actor/agent/task/session/admission refs present before calling application services; status constructs native `StatusRequest` with the incoming trace context. Transport status codes distinguish malformed request, authentication failure, identity mismatch, not found, conflict, timeout, and service failure, but every non-304 response body is a native success value or structured Kiteframe diagnostic envelope. A matching `If-None-Match` causes the server to return a bodyless HTTP `304`; the server does not import or construct `CatalogFetchResult`. The Wave 3R native client alone interprets HTTP `200`/`304` as the corresponding typed catalog fetch result.

- [ ] **Step 4: Add TLS, trace, baggage, and origin rules**

The binary requires certificate/key paths and refuses plaintext non-loopback startup. A test-only `--insecure-loopback` flag binds only `127.0.0.1`. Parse and forward `traceparent`/`tracestate` on all four routes, including status and typed catalog revalidation; keep only deployment-allowlisted baggage keys and reject sensitive names. Scan request extensions, logs, diagnostics, audit records, invocation rows, and trace attributes to prove bearer tokens, cookies, API keys, raw claims, prompts, arguments, results, and evidence bodies are absent.

- [ ] **Step 5: Run full enforcement-plane verification**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `rtk cargo test --workspace --all-features`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-openfga --features container-tests`

Expected: PASS with Docker available.

Run: `rtk cargo test -p kiteframe-provider-http --test http_profile`

Expected: PASS for authentication-before-service ordering, independently verified human/workload identities, mismatch denial, traced status, typed catalog `304`, credential leakage scans, bounded bodies, redirect denial, and stable native responses.

- [ ] **Step 6: Commit the authenticated HTTP profile**

```bash
rtk git add crates/kiteframe-provider-http tests/provider .github/workflows/ci.yml
rtk git commit -m "feat: serve capability provider profile"
```

### Task 8: Prove a Crankshaft-shaped provider profile without coupling repositories

**Files:**
- Create: `tests/provider/fixtures/crankshaft-profile/catalog.json`
- Create: `tests/provider/fixtures/crankshaft-profile/admission.json`
- Create: `tests/provider/fixtures/crankshaft-profile/policy-revisions.json`
- Create: `tests/provider/fixtures/crankshaft-profile/read-result.json`
- Create: `tests/provider/fixtures/crankshaft-profile/effect-outcomes.json`
- Create: `crates/kiteframe-provider/tests/crankshaft_profile.rs`
- Create: `crates/kiteframe-provider-http/tests/crankshaft_profile.rs`

**Interfaces:**
- Consumes: provider-neutral JSON fixtures shaped like a workforce-management provider with separately verified human/workload principals, tenant-scoped resources, provider-internal field ACL projection, revision changes, and durable effect status.
- Produces: cross-domain conformance evidence only; no dependency on Crankshaft crates and no claim that a Crankshaft Kiteframe provider is implemented.

- [ ] **Step 1: Write the failing profile tests**

```rust
#[tokio::test]
async fn workforce_profile_preserves_dual_principals_and_projection_scope() {
    let profile = ProviderProfile::load("tests/provider/fixtures/crankshaft-profile").unwrap();
    let admitted = profile
        .admit(
            verified_human("tenant-1", "employee-7"),
            verified_workload("tenant-1", "harness-2", "run-9"),
            requirement("workforce.absence.read@1", "tenant:tenant-1/employee:employee-7"),
        )
        .await
        .unwrap();
    let result = profile.invoke(admitted, read_request()).await.unwrap();
    assert_eq!(
        result.canonical_json(),
        br#"{"employeeId":"employee-7","status":"approved"}"#
    );
    assert!(!result.canonical_json().windows(6).any(|bytes| bytes == b"salary"));
    assert!(!result.canonical_json().windows(8).any(|bytes| bytes == b"coworker"));
}

#[tokio::test]
async fn revision_change_revokes_then_restart_status_recovers_effect() {
    let profile = restarted_profile_after_revision_change();
    let denied = profile.invoke(stale_admission_request()).await.unwrap_err();
    assert_eq!(denied.code.as_str(), "KF-AUTH-004");
    let status = profile
        .status(StatusRequest::try_new("inv-absence-1", trace_context("00-status")).unwrap())
        .await
        .unwrap();
    assert!(matches!(status, InvocationStatus::OutcomeUnknown { .. }));
}
```

- [ ] **Step 2: Run the profile tests**

Run: `rtk cargo test -p kiteframe-provider --test crankshaft_profile`

Expected: FAIL because the provider-neutral workforce fixture is absent.

- [ ] **Step 3: Add the exact provider-neutral fixture corpus**

The catalog declares `workforce.absence.read@1` and `workforce.absence.propose@1` with complete stable errors, modes, read/effect classes, idempotency, evidence, freshness, preconditions, and stable projection schemas. The admission fixture contains verified tenant/human/workload/run correlation, one employee-scoped resource, per-capability expiry, exact descriptor/catalog/grant digests, and canonical deployment/tenant/model `AuthorityRevisionSet`. The result fixture contains only the declared projection fields. The effect corpus covers allow, revocation, proposal-bound suspension, policy-revision change, unknown outcome, traced status-first lookup, and restart recovery.

Fixtures use only Kiteframe JSON contracts and neutral workforce names. Tests must not add a Cargo/path/Git dependency on Crankshaft, import Crankshaft types, inspect a Crankshaft checkout, or state that Crankshaft already implements the profile.

- [ ] **Step 4: Run provider and HTTP profile tests**

Run: `rtk cargo test -p kiteframe-provider --test crankshaft_profile`

Expected: PASS for dual-principal correlation, scoped resources, complete descriptors, field projection, authority revision change, revocation, proposal/evidence binding, restart, and status-first recovery.

Run: `rtk cargo test -p kiteframe-provider-http --test crankshaft_profile`

Expected: PASS for all-route authentication/trace propagation, bodyless catalog HTTP `304` for client-side typed interpretation, stable schemas/errors, and credential-free responses/audit/status records.

- [ ] **Step 5: Verify repository independence**

Run: `rtk cargo tree -p kiteframe-provider --edges normal`

Expected: PASS and contain no `crankshaft-*` package or external Crankshaft path/Git dependency.

- [ ] **Step 6: Commit the provider-neutral profile**

```bash
rtk git add tests/provider/fixtures/crankshaft-profile crates/kiteframe-provider/tests/crankshaft_profile.rs crates/kiteframe-provider-http/tests/crankshaft_profile.rs
rtk git commit -m "test: add workforce provider conformance profile"
```

## Wave 5 Exit Criteria

- Wave 3R is merged, and Wave 5 consumes its applicable exact embedded `LockedCapability`, `EffectiveCapabilityGrant`, `AuthorityRevisionSet`, `StatusRequest`, and expanded `Suspension` contracts without shadow types or a runtime-assembly dependency.
- Admission exact-matches expected catalog identity/digest and every resolved requirement against the provider-owned authoritative `CapabilityCatalog` and locked descriptor registry, then persists each exact provider-validated `LockedCapability`.
- Authority intersection is monotonic across capability version, selector, effect, execution mode, expiry, freshness, and evidence.
- Admission proves catalog identity/digest, complete required-capability coverage, optional-denial diagnostics, per-capability expiry, exact grant envelopes, and canonical authority-revision entries/digest.
- Every route uses the server-owned `ProviderPrincipalVerifier` to verify human and workload identities separately before route/service logic; admission/invocation/status fail closed on tenant/human/workload/run versus actor/agent/task/session/admission mismatch.
- The client-side `ProviderAuthenticator` supplies credential headers only; it is never imported, implemented, or treated as authority by the provider server.
- Credentials establish transport identity only and are absent from portable authority, persistence, audit, diagnostics, telemetry, and responses.
- Admission and point-of-use authorization are separate observable calls.
- The OpenFGA backend uses `ListObjects` only for admission filtering and current higher-consistency `Check` for every invocation.
- Revocation, model migration, stale policy, and OpenFGA outage fail closed.
- Input and output schemas plus stable errors/modes come from the embedded locked descriptor; grant expiry, resource selection, proposal-bound evidence, freshness, authority revisions, and preconditions are validated in a fixed order and again on resume.
- Idempotency reservations and traced `StatusRequest` lookup survive process restart, preserve complete digest/principal/audit correlation, and deduplicate concurrent duplicate effects.
- Effect execution occurs only after a durable authorization audit receipt; audit outage blocks the effect.
- Outcome append failure becomes `outcome_unknown`, and retry remains status-first with the same key and trace context.
- The four standardized TLS routes authenticate and propagate filtered W3C context; matching catalog ETags produce bodyless HTTP `304`, which only the native client interprets as its typed not-modified result.
- Provider-internal row/field ACL behavior is visible only through stable capability projection schemas and validated results.
- The Crankshaft-shaped conformance profile passes dual-principal, scoped-resource, revision-freshness, stable-projection, revocation, restart/status, audit/outcome-linkage, and repository-independence gates without claiming a Crankshaft provider implementation.

---

## Post-Wave 5 Improvement Waves

The following work is deliberately **not** a Wave 5 exit criterion. Wave 5
uses SQLite as its reference transactional invocation store. It is valid for
local and single-node deployments, but it is not the coordination authority
for a replicated Kubernetes provider deployment.

### Improvement Wave 1: PostgreSQL transactional invocation store for distributed Kiteframe

**Goal:** Add a production PostgreSQL implementation of `InvocationStore` so
every provider replica shares one transactional idempotency, status, and
suspension authority. SQLite remains the local/reference implementation and
must preserve the same portable contracts.

**Files (anticipated):**

- Create: `crates/kiteframe-provider-postgres/Cargo.toml`
- Create: `crates/kiteframe-provider-postgres/migrations/`
- Create: `crates/kiteframe-provider-postgres/src/lib.rs`
- Create: `crates/kiteframe-provider-postgres/tests/distributed_recovery.rs`
- Modify: deployment/server configuration and provider integration tests

- [ ] **Step 1: Define production-store selection and migration ownership**

Add explicit deployment configuration selecting `sqlite` only for
local/single-node reference deployments and `postgres` for replicated
deployments. Define one PostgreSQL migration history owned by the new crate;
do not share a writable SQLite volume between Kubernetes replicas or infer the
backend from environment topology.

- [ ] **Step 2: Implement atomic shared reservations and durable leases**

Use PostgreSQL transactions and unique constraints/row locks to atomically
reserve the effect scope `(tenant, actor, capability, normalized resource,
operation)` across all replicas. Persist `Reserved`, `Pending`, `Suspended`,
and `OutcomeUnknown` states, proposal/checkpoint binding, audit receipts, and
an execution lease. Recovery must conservatively turn an expired or
crash-interrupted execution into `OutcomeUnknown`; a new key remains denied
until status resolution or authorized abandonment is recorded transactionally.

- [ ] **Step 3: Preserve status and audit correlation across replicas**

Implement `StatusRequest` lookup and safe terminal projection from the shared
store, with exact principal/admission/grant/catalog/descriptor/revision/audit
correlation. A request admitted or invoked by one replica must be resumable
and status-addressable through another replica without leaking credentials or
unprojected provider data.

- [ ] **Step 4: Prove distributed behavior with real PostgreSQL integration tests**

Run two independently constructed provider service handles against the same
PostgreSQL database and prove concurrent duplicate effects execute once,
lease/crash recovery becomes status-first `OutcomeUnknown`, suspension resumes
on a different replica, and authorized abandonment is the only route that
releases the scope for a new key. Include migration-from-empty and
upgrade/rollback safety evidence.

- [ ] **Step 5: Publish Kubernetes deployment guidance**

Document connection/TLS/credential-secret configuration, readiness checks that
exercise the real shared store, migration execution/locking, backup and
restore expectations, and the rule that SQLite is unsupported for a
multi-replica provider deployment.
