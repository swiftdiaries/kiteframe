# Kiteframe V1 Wave 3R Contract Closure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the adversarially identified Wave 3 boundary gaps so Waves 4 and 5 consume complete, immutable Rust-owned runtime, capability, admission, invocation, status, catalog, trace, and deployment-authentication contracts without rereading package files or reconstructing canonical JSON in Python.

**Architecture:** Rust remains the authority for locked descriptors, wire values, digests, correlation, narrowing, schema validation, stable errors, execution modes, suspension proposals, catalog cache semantics, and diagnostics. PyO3 exposes frozen typed projections, while the Python HTTP layer contributes only deployment-owned transport, authentication, TLS, and credential-header application. Wave 3R is a mandatory closure wave between merged Wave 3 and Waves 4/5; it does not add orchestration, conversation ownership, or a runtime adapter.

**Tech Stack:** Rust 1.97.1, serde, schemars, RFC 8785 canonical JSON, SHA-256, Python 3.11+, PyO3, pyo3-stub-gen, maturin, httpx 0.28.x, W3C Trace Context, pytest, pytest-asyncio, pyright, ruff.

## Global Constraints

- The approved founding V1 design and
  [`Wave 3R contract-closure addendum`](../specs/2026-07-28-kiteframe-wave-3r-contract-closure-design.md)
  are jointly authoritative; the founding non-goals are unchanged.
- Rust owns parsing, validation, containment, normalization, hashes, locks, resolution, diagnostics, narrowing, and immutable wire values; Python must not reinterpret them.
- The host owns conversations, intent clarification, task synthesis, interaction streaming, and presentation.
- Kiteframe is not a workflow engine, graph DSL, planner language, conversation manager, or presentation layer.
- `ResolvedRuntimeInputs` is the frozen PyO3 compilation boundary and retains the validated `ResolvedAgent`, exact validated `RuntimeBinding`, selected `RuntimeTarget`, and validated target component descriptors through the exact getter `target_components`.
- `ResolvedCapabilityRequirement` retains its name and embeds the exact `LockedCapability`; descriptor and descriptor/input/output/stable-error/safety digests are available without a catalog refetch.
- `EffectiveCapabilityGrant` carries exact identity/resources and only narrower execution modes, maximum effect, per-capability expiry, required evidence, freshness, and preconditions.
- `AuthorityRevisionSet` contains canonical source/revision entries plus its digest; it contains no credentials or provider-native authorization tuples.
- `AuthorityRevisionSet` is persisted with admission/session state and is never client-supplied invocation authority; `InvocationRequest` binds to `admission_id` plus the canonical grant-set `grant_digest`, and the provider obtains current revisions at point of use.
- `StatusRequest` contains the invocation ID and native `TraceContext`; every provider request propagates its own W3C trace context.
- `CatalogFetchResult` is a native typed `modified`/`not_modified` result; HTTP 304 is valid only when the request sent a known catalog digest.
- `ProviderAuthenticator` is a deployment-owned Python protocol returning per-call credential headers through an explicit credential-header allowlist.
- Provider credentials apply to catalog, admission, invocation, and status requests but never enter baggage, canonical JSON, diagnostics, exceptions, or logs.
- Client-certificate material is deployment-only and enters HTTPX through a deployment-built `ssl.SSLContext`; no certificate path, key, or password enters a Rust contract.
- `Suspension` contains a checkpoint reference, evidence kind, protected evidence-request reference, and proposal digest; approval text is never embedded.
- OpenHarness remains a V2 delivery and is absent from V1 implementation tasks and gates.
- Crankshaft is a conformance-driving platform profile only; Kiteframe does not import the Crankshaft repository and portable contracts contain no Crankshaft-specific field.

---

## Review-Derived Requirement Matrix

| Review finding | Required closure | Implemented in | Consumer unblocked |
|---|---|---|---|
| Resolved requirements retain identity/resources but discard the locked semantics | Embed exact `LockedCapability`, add Rust getters and frozen descriptor projections | Tasks 1 and 3 | Waves 4 and 5 |
| `resolve_package` returns only `ResolvedAgent` and forces a runtime adapter to reopen YAML, binding, or target metadata | Return frozen `ResolvedRuntimeInputs`; keep `load_resolved_agent` inspection-only | Task 2 | Wave 4 |
| Python can see outcome/status tags but must parse canonical JSON for result, error, diagnostic, or suspension fields | Add structured native projections and direct getters; eliminate consumer `json.loads(canonical_json())` | Task 3 | Waves 4 and 5 |
| Provider responses are correlated only partially and stable results/errors are not checked against the locked descriptor | Validate invocation ID, descriptor digest, result schema, stable error, and execution mode | Task 3 | Waves 4 and 5 |
| Admission grants do not prove exact request/catalog correlation, required completeness, optional denial, or effective narrowing | Add admission request digest, catalog identity/digest, optional denials, `EffectiveCapabilityGrant`, and `AuthorityRevisionSet` | Task 4 | Wave 5 |
| Suspension cannot bind evidence to an exact proposed effect and status requests have no caller trace | Add `EffectProposal`, exact `Suspension`, `StatusRequest`, and status trace propagation | Task 5 | Waves 4 and 5 |
| Catalog cache freshness and 304 semantics are implicit | Add issued/expiry validation and native `CatalogFetchResult` | Task 6 | Wave 5 |
| Provider authentication and mutual TLS have no deployment seam | Add `ProviderAuthenticator`, explicit credential-header allowlist, deployment SSL context, and redaction gates | Task 7 | Wave 5 |
| Cross-language tests prove byte parity but not consumer sufficiency or a real platform profile | Add no-loss goldens, Wave 4/5 consumer gates, and a self-contained Crankshaft WFM conformance profile | Task 8 | Waves 4 and 5 |

## Scope

Wave 3R changes the V1 alpha contract and regenerates affected schemas, stubs, and goldens in the same commits as their owning types. The work covers only immutable contract closure, PyO3 projection, strict provider transport, and conformance gates. The alpha wire changes are intentional and must fail closed on old or partial payloads.

## Non-Goals

- No Deep Agents construction, middleware, tool wrapper, model invocation, checkpoint implementation, or `CompiledStateGraph`.
- No OpenFGA adapter, policy evaluation, audit storage, idempotency store, or effect execution.
- No conversation state, intent clarification, task synthesis, streaming UI, approval UI, or presentation formatting.
- No generic workflow state, graph execution, arbitrary HTTP/MCP/filesystem/shell capability, or unrestricted fallback.
- No Crankshaft import, repository checkout, shared package, provider-native tuple, or Crankshaft-only wire field.
- No OpenHarness code, fixture, compatibility check, or V1 release gate.

## File Structure

```text
crates/kiteframe-contract/src/
├── capability.rs                              # Complete descriptor getters and validation helpers
├── catalog.rs                                 # Catalog issued/expiry and typed fetch result
├── ir.rs                                      # Locked requirements and catalog correlation in ResolvedAgent
├── lib.rs                                     # Public contract exports
├── lock.rs                                    # LockedCapability read-only getters
└── service.rs                                 # Effective grants, revisions, proposals, status and validation
crates/kiteframe-contract/tests/
├── capability_contract.rs
├── catalog_fetch_contract.rs
├── service_contract.rs
└── wave3r_contract.rs
crates/kiteframe-resolver/src/resolve.rs        # Preserve exact locked selections in IR
crates/kiteframe-resolver/tests/
├── golden_ir.rs
└── resolved_agent.rs
crates/kiteframe-py/src/
├── ir.rs                                      # ResolvedRuntimeInputs and descriptor/component projections
├── lib.rs                                     # Native exports and generated-stub inventory
├── service.rs                                 # Structured service projections and checked loaders
└── validate.rs                                # Resolve once into frozen runtime inputs
crates/kiteframe-py/tests/
├── provider_request_projection.rs
├── resolved_runtime_inputs.rs
└── service_projection.rs
crates/kiteframe-schema/src/main.rs             # All changed/new generated schemas
python/kiteframe/src/kiteframe/
├── __init__.py                                # Public frozen contract exports
├── _native.pyi                                # Generated read-only type stubs
└── provider/
    ├── __init__.py                            # Provider and authenticator exports
    ├── auth.py                                # Deployment-owned authentication protocol
    ├── http.py                                # Typed fetch, descriptor checks, trace, credentials, TLS
    └── protocols.py                           # Adapter-facing typed protocols
python/kiteframe/tests/
├── conformance/test_crankshaft_profile.py
├── fixtures/conformance/crankshaft-wfm-profile.json
├── provider/test_authentication.py
├── provider/test_http_client.py
├── test_native_golden.py
├── test_native_immutability.py
├── test_native_provider_requests.py
├── test_service_round_trip.py
├── test_stub_drift.py
├── test_wave4_consumer_gate.py
└── test_wave5_consumer_gate.py
schemas/v1alpha1/
├── admission-request.schema.json
├── authority-revision-set.schema.json
├── capability-catalog.schema.json
├── capability-grant-set.schema.json
├── catalog-fetch-result.schema.json
├── effect-proposal.schema.json
├── invocation-outcome.schema.json
├── invocation-request.schema.json
├── invocation-status.schema.json
├── resolved-agent.schema.json
└── status-request.schema.json
tests/fixtures/resolved/
└── support-agent.json                         # Updated canonical IR golden
.github/workflows/ci.yml                       # Wave 3R schema/stub/conformance gates
```

### Task 1: Preserve Exact Locked Capability Semantics in IR

**Files:**
- Modify: `crates/kiteframe-contract/src/capability.rs`
- Modify: `crates/kiteframe-contract/src/lock.rs`
- Modify: `crates/kiteframe-contract/src/ir.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Modify: `crates/kiteframe-resolver/src/resolve.rs`
- Modify: `crates/kiteframe-contract/tests/capability_contract.rs`
- Modify: `crates/kiteframe-resolver/tests/resolved_agent.rs`
- Modify: `crates/kiteframe-resolver/tests/golden_ir.rs`
- Modify: `schemas/v1alpha1/resolved-agent.schema.json`
- Modify: `tests/fixtures/resolved/support-agent.json`
- Modify: `tests/fixtures/resolved/support-agent.digests.json`

**Interfaces:**
- Consumes: exact `LockedCapability`, catalog identity/revision, and catalog digest values already verified by `verify_lock`.
- Produces: `ResolvedAgent::catalog_identity()`, `catalog_digest()`, `ResolvedCapabilityRequirement::locked_capability()`, `descriptor()`, `descriptor_digest()`, `input_schema_digest()`, `output_schema_digest()`, `stable_error_set_digest()`, and `safety_metadata_digest()`.

- [ ] **Step 1: Write failing locked-requirement tests**

```rust
#[test]
fn resolved_requirement_retains_the_exact_verified_lock_entry() {
    let input = resolution_fixture();
    let expected = input.lock.capabilities[0].clone();
    let expected_catalog_digest = input.lock.catalog_digest;
    let resolved = resolve_agent(input).unwrap();
    let requirement = &resolved.capability_requirements()[0];

    assert_eq!(resolved.catalog_identity().name, "support");
    assert_eq!(resolved.catalog_identity().revision, "v1");
    assert_eq!(resolved.catalog_digest(), &expected_catalog_digest);
    assert_eq!(requirement.locked_capability(), &expected);
    assert_eq!(requirement.descriptor(), &expected.descriptor);
    assert_eq!(requirement.descriptor_digest(), &expected.descriptor_digest);
    assert_eq!(requirement.input_schema_digest(), &expected.input_schema_digest);
    assert_eq!(requirement.output_schema_digest(), &expected.output_schema_digest);
    assert_eq!(
        requirement.stable_error_set_digest(),
        &expected.stable_error_set_digest
    );
    assert_eq!(
        requirement.safety_metadata_digest(),
        &expected.safety_metadata_digest
    );
}

#[test]
fn descriptor_exposes_every_runtime_consumed_semantic() {
    let descriptor = full_descriptor();
    assert_eq!(descriptor.summary(), "Comment on a case");
    assert!(!descriptor.stable_errors().is_empty());
    assert!(descriptor.execution_modes().as_set().contains(&ExecutionMode::Immediate));
    assert_eq!(descriptor.effect(), EffectClassification::ReversibleWrite);
    assert!(!descriptor.preconditions().is_empty());
    assert!(matches!(descriptor.confirmation(), ConfirmationRequirement::Required { .. }));
}
```

- [ ] **Step 2: Run the focused Rust tests and observe the missing APIs**

Run: `rtk cargo test -p kiteframe-contract --test capability_contract`

Expected: FAIL because complete descriptor getters do not exist.

Run: `rtk cargo test -p kiteframe-resolver --test resolved_agent`

Expected: FAIL because `ResolvedCapabilityRequirement` does not retain `LockedCapability`.

- [ ] **Step 3: Replace the lossy resolved requirement with the exact locked value**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCapabilityRequirement {
    locked_capability: LockedCapability,
    required: bool,
    resources: Vec<String>,
}

impl ResolvedCapabilityRequirement {
    pub fn try_new(
        locked_capability: LockedCapability,
        required: bool,
        mut resources: Vec<String>,
    ) -> Result<Self, String> {
        resources.sort();
        resources.dedup();
        if resources.is_empty() {
            return Err("resolved capability requires at least one resource selector".to_owned());
        }
        Ok(Self { locked_capability, required, resources })
    }

    pub fn identity(&self) -> &CapabilityIdentity {
        &self.locked_capability.identity
    }
    pub fn locked_capability(&self) -> &LockedCapability { &self.locked_capability }
    pub fn descriptor(&self) -> &CapabilityDescriptor { &self.locked_capability.descriptor }
    pub fn descriptor_digest(&self) -> &Sha256Digest {
        &self.locked_capability.descriptor_digest
    }
    pub fn input_schema_digest(&self) -> &Sha256Digest {
        &self.locked_capability.input_schema_digest
    }
    pub fn output_schema_digest(&self) -> &Sha256Digest {
        &self.locked_capability.output_schema_digest
    }
    pub fn stable_error_set_digest(&self) -> &Sha256Digest {
        &self.locked_capability.stable_error_set_digest
    }
    pub fn safety_metadata_digest(&self) -> &Sha256Digest {
        &self.locked_capability.safety_metadata_digest
    }
    pub fn required(&self) -> bool { self.required }
    pub fn resources(&self) -> &[String] { &self.resources }
}
```

Change `resolve_capabilities` to pass `capability.clone()` into `try_new`; do not reconstruct a descriptor or digest from catalog state.

Add `catalog_identity: CatalogIdentity` and `catalog_digest: Sha256Digest` to `ResolvedAgentParts` and private `ResolvedAgent` fields. Construct the identity exactly from `CapabilityLock.catalog_identity` plus `catalog_revision`, carry the verified `catalog_digest`, include both in the resolved digest, and expose read-only getters. This is the source for Task 4 admission correlation after package files and the lock are unavailable.

- [ ] **Step 4: Add complete read-only descriptor and lock getters**

Add getters for `CapabilityDescriptor` fields `summary`, `stable_errors`, `execution_modes`, `resource_selector_schema`, `effect`, `freshness`, `preconditions`, `confirmation`, `approval`, and `consent`. Add getters for all `LockedCapability` fields. Keep fields private where they are newly touched so later code cannot replace one digest independently.

```rust
pub fn stable_errors(&self) -> &[CapabilityErrorDescriptor] { &self.stable_errors }
pub fn execution_modes(&self) -> &NonEmptySet<ExecutionMode> { &self.execution_modes }
pub fn effect(&self) -> EffectClassification { self.effect }
pub fn freshness(&self) -> &FreshnessRequirement { &self.freshness }
pub fn preconditions(&self) -> &[PreconditionDescriptor] { &self.preconditions }
pub fn confirmation(&self) -> &ConfirmationRequirement { &self.confirmation }
pub fn consent(&self) -> &ConsentRequirement { &self.consent }
```

- [ ] **Step 5: Regenerate the changed IR schema and golden**

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Expected: `resolved-agent.schema.json` requires `lockedCapability` and its five exact digests.

Run: `rtk cargo run -p kiteframe-cli -- compile tests/fixtures/packages/support-agent --binding tests/fixtures/packages/support-agent/bindings/deepagents.yaml --target tests/fixtures/components/deepagents-test.json --locked --json --output tests/fixtures/resolved/support-agent.json`

Expected: atomically replaces the canonical resolved-agent fixture with exact RFC 8785 bytes and no newline.

Run: `rtk jq -cS -j '{bindingDigest,lockDigest,portableDigest,resolvedDigest}' tests/fixtures/resolved/support-agent.json > tests/fixtures/resolved/support-agent.digests.json`

Expected: rewrites the four-digest record in sorted compact form with no newline.

Run: `rtk cargo test -p kiteframe-contract --test capability_contract`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-resolver --test resolved_agent --test golden_ir`

Expected: PASS and byte-for-byte deterministic IR serialization.

- [ ] **Step 6: Commit the exact locked requirement**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-resolver schemas/v1alpha1/resolved-agent.schema.json tests/fixtures/resolved/support-agent.json tests/fixtures/resolved/support-agent.digests.json
rtk git commit -m "fix: preserve locked capability semantics in resolved IR"
```

### Task 2: Return Frozen `ResolvedRuntimeInputs` From Package Resolution

**Files:**
- Modify: `crates/kiteframe-contract/src/ir.rs`
- Modify: `crates/kiteframe-py/src/ir.rs`
- Modify: `crates/kiteframe-py/src/validate.rs`
- Modify: `crates/kiteframe-py/src/lib.rs`
- Create: `crates/kiteframe-py/tests/resolved_runtime_inputs.rs`
- Modify: `python/kiteframe/src/kiteframe/__init__.py`
- Modify: `python/kiteframe/src/kiteframe/_native.pyi`
- Modify: `python/kiteframe/tests/test_native_immutability.py`
- Create: `python/kiteframe/tests/test_wave4_consumer_gate.py`

**Interfaces:**
- Consumes: successful `resolve_agent`, validated `RuntimeBinding`, `RuntimeTargetDescriptor.target`, and validated `ComponentMetadataCatalog.components`.
- Produces: frozen non-constructible `ResolvedRuntimeInputs` with `resolved_agent`, `runtime_binding`, `runtime_target`, and `target_components`; `resolve_package(...) -> ResolvedRuntimeInputs`. `load_resolved_agent(bytes) -> ResolvedAgent` remains inspection-only.

- [ ] **Step 1: Write failing resolution-boundary tests**

```python
def test_resolve_package_returns_all_compilation_inputs(package_paths) -> None:
    inputs = resolve_package(
        package_paths.package,
        package_paths.binding,
        package_paths.target,
    )
    assert isinstance(inputs, ResolvedRuntimeInputs)
    assert inputs.resolved_agent.package_name == "support-agent"
    assert inputs.resolved_agent.catalog_name == "support"
    assert inputs.resolved_agent.catalog_revision == "v1"
    assert len(inputs.resolved_agent.catalog_digest) == 64
    assert inputs.runtime_target == "deepagents"
    assert inputs.runtime_binding.runtime == "deepagents"
    assert {component.symbol for component in inputs.target_components} >= {
        "models.primary",
        "capability-providers.primary",
        "audit-sinks.ledger",
    }


def test_runtime_inputs_are_not_constructible_or_mutable() -> None:
    with pytest.raises(TypeError):
        ResolvedRuntimeInputs()  # type: ignore[call-arg]
    inputs = resolved_runtime_inputs()
    with pytest.raises(AttributeError):
        inputs.runtime_target = "forged"  # type: ignore[misc]
```

- [ ] **Step 2: Run the native boundary tests**

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_immutability.py tests/test_wave4_consumer_gate.py -q`

Expected: FAIL because `ResolvedRuntimeInputs` is undefined and `resolve_package` returns `ResolvedAgent`.

- [ ] **Step 3: Add frozen binding and component descriptor projections**

Add Rust `ResolvedAgent` getters for prompts, skills, required/optional features, and content-capture/report values already stored in the IR. Create private `Arc`-backed `PyRuntimeBinding`, `PyComponentDescriptor`, `PyResolvedTextAsset`, and `PyResolvedModelRequirement` classes with no `#[new]`. `PyRuntimeBinding` exposes runtime, sorted model-role/symbol pairs, middleware symbols, backend, checkpointer, capability-provider symbol, and audit-sink symbol. `PyComponentDescriptor` exposes symbol, kind, sorted features, durable, and optional model metadata fields. `PyResolvedAgent` exposes ordered prompt assets, skill assets, model requirements, required/optional features, content-capture values, plus direct `catalog_name`, `catalog_revision`, and `catalog_digest` getters so compilation/admission never reopens package or lock inputs.

```rust
#[pyclass(frozen, immutable_type, module = "kiteframe._native", name = "ResolvedRuntimeInputs")]
pub struct PyResolvedRuntimeInputs {
    resolved_agent: Arc<ResolvedAgent>,
    runtime_binding: Arc<RuntimeBinding>,
    runtime_target: Arc<RuntimeTarget>,
    target_components: Arc<BTreeMap<RegistrySymbol, ComponentMetadata>>,
}
```

Return fresh frozen child projections from getters; never return a mutable Rust-owned map or a Python dictionary that can be written back.

- [ ] **Step 4: Preserve the already-validated inputs before resolver ownership consumes them**

```rust
struct ResolvedRuntimeInputs {
    resolved_agent: ResolvedAgent,
    runtime_binding: RuntimeBinding,
    runtime_target: RuntimeTarget,
    target_components: BTreeMap<RegistrySymbol, ComponentMetadata>,
}

fn resolve_package_inner(
    package_root: &Path,
    binding_path: &Path,
    target_path: &Path,
) -> Result<ResolvedRuntimeInputs, Vec<Diagnostic>> {
    let package = load_package(package_root, PackageLimits::V1)?;
    let lock = read_lock(default_lock_path(&package))?;
    verify_lock(&package, &lock, None)?;
    let binding_path = package_relative_path(&package, binding_path)?;
    let binding = load_runtime_binding(package.root().as_path(), &binding_path, PackageLimits::V1)?;
    let (target, components) = load_runtime_target_catalog(target_path)?;
    let child_locks = read_child_locks(&package)?;
    let resolved_agent = resolve_agent(ResolutionInput {
        package,
        lock,
        child_locks,
        binding: binding.clone(),
        target: target.clone(),
        components: components.clone(),
    })?;
    Ok(ResolvedRuntimeInputs {
        resolved_agent,
        runtime_binding: binding,
        runtime_target: target.target,
        target_components: components.components,
    })
}
```

The returned value contains no file paths and no loader callback. Register all new classes in `_native` and update public exports and generated stubs.

- [ ] **Step 5: Prove the Wave 4 consumer needs no filesystem reread**

In `test_wave4_consumer_gate.py`, resolve from a copied temporary fixture, remove that temporary fixture through pytest’s cleanup, then read the system prompt, locked capability descriptor, runtime binding symbols, selected target, and component metadata exclusively from the retained native inputs. Assert `load_resolved_agent` returns `ResolvedAgent`, has no runtime binding getter, and is documented by its stub as inspection-only.

Run from `python/kiteframe`: `rtk uv run --project . maturin develop`

Expected: builds the updated extension.

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_immutability.py tests/test_wave4_consumer_gate.py -q`

Expected: PASS after the source fixture is gone.

Run: `rtk cargo test -p kiteframe-py --test resolved_runtime_inputs`

Expected: PASS for exact binding/target/component retention and frozen projections.

- [ ] **Step 6: Generate stubs and commit the runtime input boundary**

Run: `rtk cargo run -p kiteframe-schema -- --python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Run: `rtk cargo run -p kiteframe-schema -- --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: PASS; `resolve_package` returns `ResolvedRuntimeInputs`, while `load_resolved_agent` still returns `ResolvedAgent`.

```bash
rtk git add crates/kiteframe-py python/kiteframe/src/kiteframe python/kiteframe/tests/test_native_immutability.py python/kiteframe/tests/test_wave4_consumer_gate.py
rtk git commit -m "fix: retain validated runtime compilation inputs"
```

### Task 3: Add Structured Projections and Descriptor-Aware Provider Validation

**Files:**
- Modify: `crates/kiteframe-contract/src/capability.rs`
- Modify: `crates/kiteframe-contract/src/service.rs`
- Modify: `crates/kiteframe-contract/tests/service_contract.rs`
- Modify: `crates/kiteframe-py/src/ir.rs`
- Modify: `crates/kiteframe-py/src/service.rs`
- Modify: `crates/kiteframe-py/src/lib.rs`
- Modify: `crates/kiteframe-py/tests/service_projection.rs`
- Modify: `python/kiteframe/src/kiteframe/__init__.py`
- Modify: `python/kiteframe/src/kiteframe/_native.pyi`
- Modify: `python/kiteframe/src/kiteframe/provider/http.py`
- Modify: `python/kiteframe/tests/provider/test_http_client.py`
- Modify: `python/kiteframe/tests/test_native_provider_requests.py`

**Interfaces:**
- Consumes: the embedded locked descriptor selected by exact capability identity from `ResolvedRuntimeInputs.resolved_agent`.
- Produces: frozen `CapabilityDescriptor`, `StableCapabilityError`, `Diagnostic`, and `Suspension` projections; direct structured outcome/status getters; descriptor-aware `InvocationOutcome::validate_against(request, descriptor)` and `InvocationStatus::validate_against(request, descriptor)`.

- [ ] **Step 1: Write failing descriptor-validation tests**

```rust
#[test]
fn succeeded_result_must_match_the_locked_output_schema() {
    let descriptor = immediate_descriptor();
    let request = valid_invocation_request(&descriptor);
    let outcome = InvocationOutcome::Succeeded {
        invocation_id: request.invocation_id().clone(),
        result: json!({"unexpected": true}),
    };
    let diagnostic = outcome.validate_against(&request, &descriptor).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "KF-CAP-002");
}

#[test]
fn failed_error_must_be_declared_by_the_locked_descriptor() {
    let descriptor = immediate_descriptor();
    let outcome = failed_with_code("PROVIDER_NATIVE_500");
    let diagnostic = outcome.validate_against(&valid_invocation_request(&descriptor), &descriptor)
        .unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "KF-CAP-002");
}

#[test]
fn deferred_and_suspended_variants_require_their_locked_execution_modes() {
    assert_result_invalid(deferred_outcome(), immediate_descriptor());
    assert_result_invalid(suspended_outcome(), immediate_descriptor());
}
```

- [ ] **Step 2: Write failing structured Python projection tests**

```python
def test_outcome_exposes_validated_result_without_json_reparse(valid_outcome) -> None:
    assert valid_outcome.result == {"caseId": "case-1", "accepted": True}
    assert valid_outcome.error is None
    assert valid_outcome.diagnostic is None


def test_failure_and_denial_are_structured(valid_failure, valid_denial) -> None:
    assert valid_failure.error.code == "CASE_CONFLICT"
    assert valid_failure.error.retry == "after_refresh"
    assert valid_denial.diagnostic.code == "KF-AUTH-003"
    assert valid_denial.diagnostic.details == ()
```

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_provider_requests.py tests/provider/test_http_client.py -q`

Expected: FAIL because only status, invocation ID, and canonical bytes are projected.

- [ ] **Step 3: Implement locked descriptor validation for every response variant**

Add `CapabilityDescriptor::validate_input`, `validate_output`, `validate_stable_error`, and `supports_execution_mode`. Stable error validation exact-matches code, category, retry class, and safe public message against one locked `CapabilityErrorDescriptor`.

```rust
pub fn validate_against(
    &self,
    request: &InvocationRequest,
    descriptor: &CapabilityDescriptor,
) -> Result<(), Diagnostic> {
    validate_response_invocation_id(self.invocation_id(), request.invocation_id())?;
    request.validate_against(descriptor).map_err(|_| result_invalid(
        DiagnosticStage::Invoke,
        "invocation request no longer matches its locked descriptor",
    ))?;
    match self {
        Self::Succeeded { result, .. } => descriptor.validate_output(result),
        Self::Failed { error, .. } => descriptor.validate_stable_error(error),
        Self::Deferred { .. } => descriptor.require_mode(ExecutionMode::Deferred),
        Self::Suspended { .. } => descriptor.require_mode(ExecutionMode::Suspendable),
        Self::Denied { diagnostic, .. } => validate_denial_diagnostic(diagnostic),
        Self::OutcomeUnknown { diagnostic, .. } => validate_status_first(diagnostic.diagnostic())
            .map_err(|_| result_invalid(DiagnosticStage::Invoke, "invalid status-first error")),
    }
}
```

Apply the same result, stable-error, suspension-mode, denial, and invocation-ID rules to terminal `InvocationStatus`.

- [ ] **Step 4: Add frozen direct projections**

Expose `ResolvedCapabilityRequirement.descriptor`, all five digest properties, and descriptor fields directly. Add frozen `PyStableCapabilityError`, `PyDiagnostic`, and `PySuspension`. Add optional `result`, `error`, `diagnostic`, and `suspension` getters to `PyInvocationOutcome` and `PyInvocationStatus`. Add direct native `baggage`, arguments, preconditions, and evidence-reference projections to request classes. Rewrite `_request_headers` to consume `request.traceparent`, `request.tracestate`, and `request.baggage` directly; remove `json.loads(request.canonical_json())`.

Convert `serde_json::Value` recursively inside Rust to detached Python scalars, tuples, and dictionaries. The detached value is presentation data only and cannot mutate the backing Rust value. Do not call Python’s `json.loads`, and do not require a consumer to call `canonical_json()`.

```rust
#[getter]
fn result(&self, py: Python<'_>) -> PyResult<Option<PyObject>> {
    match self.inner.as_ref() {
        ContractInvocationOutcome::Succeeded { result, .. } => {
            json_value_to_python(py, result).map(Some)
        }
        _ => Ok(None),
    }
}
```

- [ ] **Step 5: Make the HTTP client validate against the retained locked descriptor**

Require `resolved_runtime_inputs: ResolvedRuntimeInputs` in `ProviderHttpClient.__init__`. Build an immutable identity-to-requirement index once. `invoke` finds the exact requirement for `request.capability_name` and version, and calls the native loader with that requirement. Unknown or duplicate identity fails before transport. Because `StatusRequest` intentionally contains only invocation ID plus trace context, Task 5 requires its caller to pass the exact `ResolvedCapabilityRequirement`; the client verifies that requirement belongs to its frozen index before status transport and response validation.

```python
return _load_native_response(
    load_invocation_outcome_for_request,
    await self._request(...),
    request,
    requirement,
)
```

Change the native loader to accept `PyResolvedCapabilityRequirement` and pass `requirement.descriptor()` to the Rust validator.

- [ ] **Step 6: Run descriptor and structured-projection checks**

Run: `rtk cargo test -p kiteframe-contract --test service_contract`

Expected: PASS for valid result/error/mode cases and `KF-CAP-002` for every mismatch.

Run: `rtk cargo test -p kiteframe-py --test service_projection`

Expected: PASS for structured result, error, diagnostic, suspension, and descriptor getters.

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_provider_requests.py tests/provider/test_http_client.py -q`

Expected: PASS with no `json.loads(request.canonical_json())` or outcome/status canonical-JSON consumer path.

- [ ] **Step 7: Generate stubs and commit structured validation**

Run: `rtk cargo run -p kiteframe-schema -- --python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: direct read-only properties appear for descriptors, results, stable errors, diagnostics, and suspensions.

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-py python/kiteframe/src/kiteframe python/kiteframe/tests
rtk git commit -m "fix: validate provider results against locked descriptors"
```

### Task 4: Close Admission Correlation and Effective Authority Contracts

**Files:**
- Modify: `crates/kiteframe-contract/src/service.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Modify: `crates/kiteframe-contract/tests/service_contract.rs`
- Create: `crates/kiteframe-contract/tests/wave3r_contract.rs`
- Modify: `crates/kiteframe-py/src/service.rs`
- Modify: `crates/kiteframe-py/src/lib.rs`
- Modify: `crates/kiteframe-py/tests/service_projection.rs`
- Modify: `crates/kiteframe-schema/src/main.rs`
- Modify: `schemas/v1alpha1/admission-request.schema.json`
- Modify: `schemas/v1alpha1/capability-grant-set.schema.json`
- Create: `schemas/v1alpha1/authority-revision-set.schema.json`
- Modify: `schemas/v1alpha1/invocation-request.schema.json`
- Modify: `python/kiteframe/src/kiteframe/__init__.py`
- Modify: `python/kiteframe/src/kiteframe/_native.pyi`
- Modify: `python/kiteframe/tests/test_service_round_trip.py`

**Interfaces:**
- Consumes: exact requested capabilities and embedded locked descriptors from `AdmissionRequest`.
- Produces: `AdmissionRequest::request_digest()`, catalog identity/digest correlation, `CapabilityDenial`, `EffectiveCapabilityGrant`, `RequiredEvidence`, `AuthorityRevision`, `AuthorityRevisionSet`, and `InvocationRequest::grant_digest()`.

- [ ] **Step 1: Write failing admission completeness and narrowing tests**

```rust
#[test]
fn every_required_capability_must_have_an_effective_grant() {
    let request = admission_request_with_required_and_optional();
    let response = grant_set_without_required(request.request_digest());
    let diagnostic = response.validate_against(&request).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "KF-CAP-002");
}

#[test]
fn every_optional_capability_has_exactly_one_grant_or_denial() {
    let request = admission_request_with_required_and_optional();
    assert_result_invalid(grant_set_missing_optional_disposition(), &request);
    assert_result_invalid(grant_and_denial_for_same_optional(), &request);
}

#[test]
fn effective_grant_cannot_widen_any_locked_dimension() {
    let request = narrow_admission_request();
    for response in [
        grant_with_extra_resource(),
        grant_with_extra_execution_mode(),
        grant_with_later_expiry(),
        grant_with_weaker_evidence(),
        grant_with_staler_freshness(),
        grant_missing_required_precondition(),
    ] {
        assert_eq!(
            response.validate_against(&request).unwrap_err().code.as_str(),
            "KF-CAP-002"
        );
    }
}
```

- [ ] **Step 2: Run service tests**

Run: `rtk cargo test -p kiteframe-contract --test service_contract --test wave3r_contract`

Expected: FAIL because the response has neither exact admission correlation nor effective grant dimensions.

- [ ] **Step 3: Add exact request and authority correlation**

Extend `AdmissionRequestParts` with `catalog_identity: CatalogIdentity` and `catalog_digest: Sha256Digest`. Compute `request_digest` from all request wire fields except the digest itself using domain `b"kiteframe:admission-request:v1\0"`, serialize it, and verify it during deserialization.

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityRevisionSet {
    entries: Vec<AuthorityRevision>,
    authority_revision_digest: Sha256Digest,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityRevision {
    source: String,
    revision: String,
}
```

`AuthorityRevisionSet::try_new` trims neither value, rejects blank values, sorts by `(source, revision)`, rejects duplicate sources, and computes a domain-separated canonical digest. Strict fields make credentials, tokens, tuple objects, and provider-native authorization structures unrepresentable.

Freeze the authority ownership rule in the wire contract: `AuthorityRevisionSet` appears on `CapabilityGrantSet`, not on `InvocationRequest`. Add the admitted effective `grant_digest: Sha256Digest` beside `admission_id` on `InvocationRequest`. The invocation constructor requires both, serialization includes both, and deserialization rejects either when absent. A client never supplies authority revisions; the provider resolves admission state by `admission_id`, exact-matches `grant_digest`, obtains a fresh current `AuthorityRevisionSet` at point of use, and later audit work records both admitted and current revision digests.

- [ ] **Step 4: Replace wire grants with complete `EffectiveCapabilityGrant` values**

```rust
pub struct EffectiveCapabilityGrantParts {
    pub capability: CapabilityIdentity,
    pub resources: Vec<NormalizedResourceSelector>,
    pub execution_modes: NonEmptySet<ExecutionMode>,
    pub maximum_effect: EffectClassification,
    pub expires_at: Timestamp,
    pub required_evidence: RequiredEvidence,
    pub freshness: FreshnessRequirement,
    pub preconditions: Vec<PreconditionDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequiredEvidence {
    confirmation: ConfirmationRequirement,
    approval: ApprovalRequirement,
    consent: ConsentRequirement,
}
```

Use the V1 effect ceiling order `read_only < reversible_write < irreversible_write < external_side_effect`. An effective grant must use a subset of locked execution modes, a maximum effect no broader than the locked effect, expiry no later than the grant-set expiry, evidence no weaker than the descriptor, freshness maxima no larger than the descriptor, `policy_revision_required` never weakened from true to false, and all locked required preconditions retained.

- [ ] **Step 5: Add optional denials and exact grant-set validation**

`CapabilityGrantSet` carries `admission_request_digest`, `catalog_identity`, `catalog_digest`, `authority_revisions`, `grants: Vec<EffectiveCapabilityGrant>`, and `optional_denials: Vec<CapabilityDenial>`. `CapabilityDenial` accepts only optional requested identities and a warning or error diagnostic with code `KF-AUTH-001`; its diagnostic remains redacted.

Validation must prove:

1. actor, agent, task, and session exact-match;
2. response request digest exact-matches;
3. catalog identity and digest exact-match;
4. each required request has exactly one valid grant;
5. each optional request has exactly one grant or denial;
6. no unrequested capability appears;
7. every grant narrows its exact embedded locked descriptor;
8. each per-capability expiry is no later than the set expiry.

Add an invocation contract test that changes only `grantDigest` and proves correlation fails before result validation. Add a schema assertion that `invocation-request.schema.json` requires `admissionId` and `grantDigest` and has no `authorityRevisions` property.

- [ ] **Step 6: Add frozen effective-grant, denial, and revision projections**

Expose every dimension as a direct property. Tuples preserve canonical ordering; `AuthorityRevisionSet` exposes entries and digest but no mapping or generic details field. Rename the Python grant projection to `EffectiveCapabilityGrant` and remove the old alpha `CapabilityGrant` export.

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Expected: admission, grant-set, and authority-revision schemas contain all exact correlation and narrowing fields.

Run: `rtk cargo test -p kiteframe-contract --test service_contract --test wave3r_contract`

Expected: PASS.

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_service_round_trip.py -q`

Expected: PASS for exact canonical bytes, immutable projections, optional denial, and revision digest.

- [ ] **Step 7: Commit the admission authority closure**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-py crates/kiteframe-schema schemas/v1alpha1 python/kiteframe/src/kiteframe python/kiteframe/tests/test_service_round_trip.py
rtk git commit -m "fix: close admission and effective grant contracts"
```

### Task 5: Bind Suspension to Exact Proposals and Trace Every Status Request

**Files:**
- Modify: `crates/kiteframe-contract/src/service.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Modify: `crates/kiteframe-contract/tests/service_contract.rs`
- Modify: `crates/kiteframe-py/src/service.rs`
- Modify: `crates/kiteframe-py/src/lib.rs`
- Modify: `crates/kiteframe-py/tests/provider_request_projection.rs`
- Modify: `crates/kiteframe-schema/src/main.rs`
- Create: `schemas/v1alpha1/effect-proposal.schema.json`
- Create: `schemas/v1alpha1/status-request.schema.json`
- Modify: `schemas/v1alpha1/invocation-outcome.schema.json`
- Modify: `schemas/v1alpha1/invocation-status.schema.json`
- Modify: `python/kiteframe/src/kiteframe/__init__.py`
- Modify: `python/kiteframe/src/kiteframe/_native.pyi`
- Modify: `python/kiteframe/src/kiteframe/provider/protocols.py`
- Modify: `python/kiteframe/src/kiteframe/provider/http.py`
- Modify: `python/kiteframe/tests/provider/test_http_client.py`
- Modify: `python/kiteframe/tests/test_native_provider_requests.py`

**Interfaces:**
- Consumes: exact invocation identity, admission ID, effective grant digest, resource, arguments, preconditions, idempotency key, effect class, and native trace context.
- Produces: `EffectProposal`, `EvidenceKind`, `ProtectedEvidenceRequestRef`, exact `Suspension`, and `StatusRequest`; `CapabilityInvoker.status(request: StatusRequest)`.

- [ ] **Step 1: Write failing proposal, suspension, and status tests**

```rust
#[test]
fn proposal_digest_changes_for_every_effect_semantic() {
    let baseline = effect_proposal();
    for changed in [
        with_other_capability(&baseline),
        with_other_admission_id(&baseline),
        with_other_grant_digest(&baseline),
        with_other_resource(&baseline),
        with_other_arguments_digest(&baseline),
        with_other_preconditions_digest(&baseline),
        with_other_idempotency_key(&baseline),
        with_other_effect(&baseline),
    ] {
        assert_ne!(baseline.proposal_digest(), changed.proposal_digest());
    }
}

#[test]
fn suspension_contains_only_protected_refs_and_proposal_binding() {
    let suspension = Suspension::try_new(
        CheckpointRef::new("checkpoint:opaque:1").unwrap(),
        EvidenceKind::Approval,
        ProtectedEvidenceRequestRef::new("evidence-request:opaque:1").unwrap(),
        effect_proposal().proposal_digest().to_owned(),
    ).unwrap();
    let wire = serde_json::to_value(suspension).unwrap();
    assert!(wire.get("proposalDigest").is_some());
    assert!(wire.get("approvalText").is_none());
    assert!(wire.get("evidence").is_none());
}

#[test]
fn status_request_requires_native_trace_context() {
    let request = StatusRequest::new(invocation_id("inv-1"), trace_context());
    assert_eq!(request.trace_context().traceparent(), VALID_TRACEPARENT);
}
```

- [ ] **Step 2: Run the focused tests**

Run: `rtk cargo test -p kiteframe-contract --test service_contract`

Expected: FAIL because suspension has only `checkpointRef` and status accepts a bare string.

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/provider/test_http_client.py -q`

Expected: FAIL because `status` has no trace context.

- [ ] **Step 3: Add canonical effect proposal and exact suspension contracts**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectProposal {
    invocation_id: InvocationId,
    admission_id: AdmissionId,
    grant_digest: Sha256Digest,
    capability: CapabilityIdentity,
    selected_resource: NormalizedResourceSelector,
    arguments_digest: Sha256Digest,
    preconditions_digest: Sha256Digest,
    idempotency_key: Option<IdempotencyKey>,
    effect: EffectClassification,
    proposal_digest: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind { Confirmation, Approval, Consent }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Suspension {
    checkpoint_ref: CheckpointRef,
    evidence_kind: EvidenceKind,
    evidence_request_ref: ProtectedEvidenceRequestRef,
    proposal_digest: Sha256Digest,
}
```

Compute `arguments_digest`, `preconditions_digest`, and `proposal_digest` in Rust from canonical values with distinct domains. Include `admission_id`, the admitted `grant_digest`, and the presence or absence of the idempotency key in the proposal digest so resumed approval cannot be moved to a different admission envelope. Require a key exactly when the locked descriptor's idempotency contract requires one; preserve `None` for read-only or consent-gated capabilities whose descriptor forbids a key. Deserialization recomputes and verifies the proposal digest. No field accepts evidence payload, authority revisions, or approval text.

- [ ] **Step 4: Add `StatusRequest` and native projections**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusRequest {
    invocation_id: InvocationId,
    trace_context: TraceContext,
}
```

Add canonical-only `load_status_request`, a frozen `StatusRequest` projection, `StatusRequest.invocation_id`, `traceparent`, `tracestate`, baggage getters, and structured proposal/suspension getters. Change status response correlation to accept `&StatusRequest`.

- [ ] **Step 5: Propagate status trace context and remove the bare-ID protocol**

```python
class CapabilityInvoker(Protocol):
    async def invoke(self, request: InvocationRequest) -> InvocationOutcome: ...
    async def status(
        self,
        request: StatusRequest,
        requirement: ResolvedCapabilityRequirement,
    ) -> InvocationStatus: ...
```

`ProviderHttpClient.status` accepts native `StatusRequest` plus the exact native `ResolvedCapabilityRequirement`, verifies that requirement identity and all five digests exact-match one entry in its frozen `ResolvedRuntimeInputs`, calls `_request_headers(request, ...)`, encodes only `request.invocation_id` into the route, and validates the response against the request plus that locked descriptor. The requirement is local validation context and is not serialized into the status request. Never obtain trace state from ambient context.

- [ ] **Step 6: Generate schemas/stubs and run trace tests**

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Run: `rtk cargo run -p kiteframe-schema -- --python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: new proposal/status schemas and exact suspension fields; no `approvalText`.

Run: `rtk cargo test -p kiteframe-contract --test service_contract`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-py --test provider_request_projection`

Expected: PASS for canonical status request loading and correlation.

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_provider_requests.py tests/provider/test_http_client.py -q`

Expected: PASS and the captured status request contains exact `traceparent`, optional `tracestate`, and only allowlisted baggage.

- [ ] **Step 7: Commit proposal-bound suspension and status trace**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-py crates/kiteframe-schema schemas/v1alpha1 python/kiteframe/src/kiteframe python/kiteframe/tests
rtk git commit -m "fix: bind suspension and trace status requests"
```

### Task 6: Make Catalog Freshness and HTTP 304 Native and Typed

**Files:**
- Modify: `crates/kiteframe-contract/src/catalog.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Create: `crates/kiteframe-contract/tests/catalog_fetch_contract.rs`
- Modify: `crates/kiteframe-py/src/service.rs`
- Modify: `crates/kiteframe-py/src/lib.rs`
- Modify: `crates/kiteframe-py/tests/service_projection.rs`
- Modify: `crates/kiteframe-schema/src/main.rs`
- Modify: `schemas/v1alpha1/capability-catalog.schema.json`
- Create: `schemas/v1alpha1/catalog-fetch-result.schema.json`
- Modify: `python/kiteframe/src/kiteframe/__init__.py`
- Modify: `python/kiteframe/src/kiteframe/_native.pyi`
- Modify: `python/kiteframe/src/kiteframe/provider/protocols.py`
- Modify: `python/kiteframe/src/kiteframe/provider/http.py`
- Modify: `python/kiteframe/tests/provider/test_http_client.py`

**Interfaces:**
- Consumes: `CatalogRequest.known_catalog_digest` and the provider’s catalog body/ETag.
- Produces: issued/expiry-valid `CapabilityCatalog` and native `CatalogFetchResult::{Modified, NotModified}`.

- [ ] **Step 1: Write failing catalog contract tests**

```rust
#[test]
fn catalog_expiry_must_follow_issue_time() {
    let error = CapabilityCatalog::try_new(
        catalog_identity(),
        Timestamp::new(200),
        Some(Timestamp::new(200)),
        descriptors(),
    ).unwrap_err();
    assert_eq!(error, "catalog expiry must be after its issue time");
}

#[test]
fn modified_and_not_modified_are_disjoint_typed_results() {
    let modified = CatalogFetchResult::Modified { catalog: catalog() };
    let not_modified = CatalogFetchResult::NotModified {
        catalog_digest: digest(),
    };
    assert_ne!(canonical_json(&modified), canonical_json(&not_modified));
}
```

- [ ] **Step 2: Run catalog tests**

Run: `rtk cargo test -p kiteframe-contract --test catalog_fetch_contract`

Expected: FAIL because catalogs have no issued/expiry time and no typed fetch result.

- [ ] **Step 3: Add catalog time bounds and typed fetch result**

```rust
pub struct CapabilityCatalog {
    identity: CatalogIdentity,
    issued_at: Timestamp,
    expires_at: Option<Timestamp>,
    descriptors: Vec<CapabilityDescriptor>,
    catalog_digest: Sha256Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "status", deny_unknown_fields)]
pub enum CatalogFetchResult {
    Modified { catalog: CapabilityCatalog },
    NotModified {
        catalog_digest: Sha256Digest,
    },
}
```

Include `issuedAt` and `expiresAt` in the catalog digest. Reject `expiresAt <= issuedAt`. Add frozen `CatalogFetchResult` projection with `status`, optional `catalog`, and digest getters. The modified variant exposes catalog identity through its typed catalog; the not-modified variant carries only the exact digest already known by the requester.

- [ ] **Step 4: Implement exact 200/304 HTTP rules**

Change `CatalogProvider.catalog` and `ProviderHttpClient.catalog` to return `CatalogFetchResult`.

- HTTP 200 requires canonical catalog JSON, a valid typed catalog, and an ETag exactly equal to the returned catalog digest.
- HTTP 304 requires `request.known_catalog_digest is not None`, an empty body, and an ETag exactly equal to that known digest.
- HTTP 304 without a known digest is `ProviderTransportError("provider returned unsolicited not-modified")`.
- HTTP 304 with a mismatched or absent ETag is `ProviderTransportError("provider not-modified digest mismatch")`.
- Every other 3xx remains forbidden.

Construct `NotModified` through a native factory that accepts the request’s already-validated known digest; Python does not synthesize canonical bytes.

- [ ] **Step 5: Run catalog transport and schema checks**

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Run: `rtk cargo test -p kiteframe-contract --test catalog_fetch_contract`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-py --test service_projection`

Expected: PASS for both typed variants and immutable time fields.

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/provider/test_http_client.py -q`

Expected: PASS for 200, valid 304, unsolicited 304, mismatched ETag, expired catalog representation, body limits, and redirects.

- [ ] **Step 6: Commit typed catalog caching**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-py crates/kiteframe-schema schemas/v1alpha1 python/kiteframe/src/kiteframe python/kiteframe/tests/provider/test_http_client.py
rtk git commit -m "fix: type catalog freshness and revalidation"
```

### Task 7: Add Deployment-Owned Provider Authentication and Credential Redaction

**Files:**
- Create: `python/kiteframe/src/kiteframe/provider/auth.py`
- Modify: `python/kiteframe/src/kiteframe/provider/__init__.py`
- Modify: `python/kiteframe/src/kiteframe/provider/http.py`
- Create: `python/kiteframe/tests/provider/test_authentication.py`
- Modify: `python/kiteframe/tests/provider/test_http_client.py`
- Modify: `python/kiteframe/README.md`

**Interfaces:**
- Consumes: deployment-owned `ProviderAuthenticator`, explicit `credential_header_allowlist`, and optional deployment-built `ssl.SSLContext`.
- Produces: per-call credentials for all four provider operations without adding credentials to a Rust value or public diagnostic.

- [ ] **Step 1: Write failing all-route authentication and redaction tests**

```python
@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["catalog", "admit", "invoke", "status"])
async def test_authenticator_is_called_per_request(operation, client_fixture) -> None:
    authenticator = RecordingAuthenticator({"x-provider-token": "secret-value"})
    client, captured = client_fixture(
        authenticator=authenticator,
        credential_header_allowlist=frozenset({"x-provider-token"}),
    )
    await call_operation(client, operation)
    assert authenticator.operations == [operation]
    assert captured.headers["x-provider-token"] == "secret-value"


@pytest.mark.asyncio
async def test_credentials_never_enter_body_baggage_or_failure_text(client_fixture) -> None:
    secret = "credential-that-must-not-escape"
    client, captured = client_fixture(
        authenticator=StaticAuthenticator({"authorization": f"Bearer {secret}"}),
        credential_header_allowlist=frozenset({"authorization"}),
        response=httpx.Response(503, content=b"invalid"),
    )
    with pytest.raises(Exception) as error:
        await client.invoke(valid_invocation_request())
    assert secret not in captured.content.decode(errors="ignore")
    assert secret not in captured.headers.get("baggage", "")
    assert secret not in str(error.value)
    assert secret not in repr(error.value)
```

- [ ] **Step 2: Run authentication tests**

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/provider/test_authentication.py -q`

Expected: FAIL because no authentication protocol or client seam exists.

- [ ] **Step 3: Define the deployment-owned protocol and safe request metadata**

```python
ProviderOperation = Literal["catalog", "admit", "invoke", "status"]

@dataclass(frozen=True, slots=True)
class ProviderAuthRequest:
    operation: ProviderOperation
    method: Literal["GET", "POST"]
    origin: str
    route: str


@runtime_checkable
class ProviderAuthenticator(Protocol):
    async def credential_headers(
        self,
        request: ProviderAuthRequest,
    ) -> Mapping[str, str]: ...
```

`ProviderAuthRequest` contains no canonical body, arguments, result, evidence, actor, policy tuple, or trace baggage. It is a Python deployment value and is not exported from `_native` or schema generation.

- [ ] **Step 4: Enforce the explicit credential-header allowlist**

Add constructor parameters:

```python
def __init__(
    self,
    base_url: str,
    *,
    resolved_runtime_inputs: ResolvedRuntimeInputs,
    authenticator: ProviderAuthenticator | None = None,
    credential_header_allowlist: frozenset[str] = frozenset(),
    tls_context: ssl.SSLContext | None = None,
    transport: httpx.MockTransport | None = None,
    baggage_allowlist: frozenset[str] = frozenset(),
) -> None:
```

Reject an authenticator with an empty allowlist. Normalize allowlist and returned names to lowercase ASCII. Reject duplicate names, CR/LF/NUL in values, and headers outside the explicit allowlist. Always forbid credential control over `host`, `content-length`, `content-type`, `accept`, `accept-encoding`, `traceparent`, `tracestate`, `baggage`, `cookie`, `set-cookie`, and proxy headers even when listed.

Call the authenticator immediately before each request, merge only validated credential headers after trace/body headers, and replace the local credential mapping with an empty mapping after the request finishes. Never include headers in exception messages or diagnostic details.

- [ ] **Step 5: Add deployment-only client-certificate configuration**

HTTPX 0.28 accepts a custom `ssl.SSLContext` through `AsyncClient(verify=...)`. Use `verify=tls_context if tls_context is not None else True`; retain `trust_env=False`. Deployment code loads its certificate chain before construction:

```python
tls_context = ssl.create_default_context()
tls_context.load_cert_chain(
    certfile=deployment_client_certificate_path,
    keyfile=deployment_client_key_path,
)
client = ProviderHttpClient(
    provider_origin,
    resolved_runtime_inputs=inputs,
    tls_context=tls_context,
)
```

Kiteframe stores only the `SSLContext` reference in the client. It does not serialize, log, inspect, or place certificate paths/passwords in diagnostics. Mock transports remain test-only and cannot prove mutual TLS.

- [ ] **Step 6: Run authentication, typing, and redaction checks**

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/provider/test_authentication.py tests/provider/test_http_client.py -q`

Expected: PASS for all four operations, per-call refresh, allowlist rejection, header injection rejection, failure redaction, and TLS-context construction.

Run from `python/kiteframe`: `rtk uv run --project . ruff check src tests`

Expected: PASS.

Run from `python/kiteframe`: `rtk uv run --project . pyright`

Expected: PASS for `ProviderAuthenticator`, optional authenticator, and `ssl.SSLContext`.

- [ ] **Step 7: Commit the deployment authentication seam**

```bash
rtk git add python/kiteframe/src/kiteframe/provider python/kiteframe/tests/provider python/kiteframe/README.md
rtk git commit -m "feat: add deployment-owned provider authentication"
```

### Task 8: Gate Cross-Language No-Loss Contracts and Wave 4/5 Consumers

**Files:**
- Modify: `crates/kiteframe-contract/tests/schema_contract.rs`
- Modify: `crates/kiteframe-py/tests/provider_request_projection.rs`
- Modify: `crates/kiteframe-py/tests/service_projection.rs`
- Modify: `python/kiteframe/tests/test_native_golden.py`
- Modify: `python/kiteframe/tests/test_stub_drift.py`
- Modify: `python/kiteframe/tests/test_service_round_trip.py`
- Modify: `python/kiteframe/tests/test_wave4_consumer_gate.py`
- Create: `python/kiteframe/tests/test_wave5_consumer_gate.py`
- Create: `python/kiteframe/tests/fixtures/conformance/crankshaft-wfm-profile.json`
- Create: `python/kiteframe/tests/conformance/test_crankshaft_profile.py`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: every Wave 3R Rust type, schema, native projection, protocol, and provider-client boundary.
- Produces: byte-for-byte no-loss gates plus executable Wave 4 and Wave 5 prerequisites; no new public interface.

- [ ] **Step 1: Add failing cross-language no-loss assertions**

```python
def test_locked_requirement_is_lossless_across_rust_and_python() -> None:
    resolved = load_resolved_agent(RESOLVED_GOLDEN.read_bytes())
    requirement = resolved.capability_requirements[0]
    expected = LOCK_GOLDEN["capabilities"][0]
    assert requirement.descriptor_digest == expected["descriptorDigest"]
    assert requirement.input_schema_digest == expected["inputSchemaDigest"]
    assert requirement.output_schema_digest == expected["outputSchemaDigest"]
    assert requirement.stable_error_set_digest == expected["stableErrorSetDigest"]
    assert requirement.safety_metadata_digest == expected["safetyMetadataDigest"]
    assert requirement.descriptor.canonical_json() == canonical_bytes(expected["descriptor"])


def test_service_variants_round_trip_without_field_loss(service_goldens) -> None:
    for loader, payload in service_goldens:
        value = loader(payload)
        assert value.canonical_json() == payload
        assert not hasattr(value, "__dict__")
```

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_golden.py tests/test_service_round_trip.py -q`

Expected: FAIL until every new type and getter is wired through schemas, stubs, and PyO3.

- [ ] **Step 2: Add the Wave 4 consumer gate**

The test must construct a minimal fake adapter from `ResolvedRuntimeInputs` after its package, binding, lock, and target fixture paths are unavailable. It reads:

- system prompt and skill assets from `ResolvedAgent`;
- exact model and component symbols from `RuntimeBinding`;
- selected `RuntimeTarget`;
- validated component kinds/features/model metadata;
- locked descriptor summary/input schema/output schema/stable errors/execution modes/safety fields;
- required and optional negotiated features.

It fails if the adapter calls `open`, `Path.read_*`, `yaml`, `json.loads`, `load_resolved_agent`, or any catalog provider. The resulting fake runtime construction record is assembled solely from frozen getters.

- [ ] **Step 3: Add the Wave 5 consumer gate**

The test creates a native admission request and proves:

1. a required omission fails;
2. an optional denial is directly projected;
3. a broadened resource/mode/effect/expiry/evidence/freshness/precondition fails;
4. tampering with the persisted admission `AuthorityRevisionSet` digest fails, while `InvocationRequest` contains only `admissionId` plus the admitted effective `grantDigest`;
5. invocation result/error/mode mismatch fails against the embedded lock;
6. a suspended outcome exposes evidence kind, protected request ref, and exact proposal digest;
7. status propagates the request’s native trace context;
8. catalog 304 without a sent known digest fails;
9. credentials appear only in the explicitly allowed HTTP header.

Use only mock provider transport and native fixtures; do not implement OpenFGA, audit storage, or effect execution.

- [ ] **Step 4: Add the self-contained Crankshaft WFM conformance profile**

`crankshaft-wfm-profile.json` contains only standard Kiteframe V1 alpha values: package/lock/resolved digests, actor/agent/task/session refs, an exact WFM capability identity, resource selector, effective execution modes/effect/expiry/evidence/freshness/preconditions, authority source/revision, invocation/status trace context, and proposal/suspension refs.

```python
def test_crankshaft_profile_uses_only_portable_kiteframe_contracts() -> None:
    profile = load_fixture("crankshaft-wfm-profile.json")
    request = load_admission_request(canonical_bytes(profile["admissionRequest"]))
    grant = load_capability_grant_set_for_request(
        canonical_bytes(profile["grantSet"]),
        request,
    )
    assert grant.grants[0].name == "workforce.shift.change"
    assert grant.authority_revisions.entries[0].source == "wfm-policy"
    assert "crankshaft" not in resolved_agent_schema_property_names()
```

The fixture name and test description identify the platform profile. Contract field names and enum values remain generic. The test imports only `kiteframe`, Python standard-library modules, and local fixtures; CI must not clone, install, or address a Crankshaft repository.

- [ ] **Step 5: Regenerate and check all schemas and stubs**

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Run: `rtk cargo run -p kiteframe-schema -- --check schemas/v1alpha1`

Run: `rtk cargo run -p kiteframe-schema -- --python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Run: `rtk cargo run -p kiteframe-schema -- --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: all commands PASS with no generated drift.

- [ ] **Step 6: Run the complete Rust and Python verification matrix**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `rtk cargo test --workspace --all-features`

Expected: PASS, including adversarial contract, PyO3 projection, schema, resolver, and golden tests.

Run from `python/kiteframe`: `rtk uv run --project . pytest -q`

Expected: PASS, including Wave 4, Wave 5, authentication, HTTP, redaction, and Crankshaft-profile conformance.

Run from `python/kiteframe`: `rtk uv run --project . ruff check src tests`

Expected: PASS.

Run from `python/kiteframe`: `rtk uv run --project . pyright`

Expected: PASS.

Run: `rtk git diff --check`

Expected: PASS.

- [ ] **Step 7: Commit the Wave 3R consumer gates**

```bash
rtk git add .github/workflows/ci.yml crates/kiteframe-contract/tests crates/kiteframe-py/tests python/kiteframe/tests
rtk git commit -m "test: gate wave 3r cross-language contracts"
```

## Wave 3R Exit Criteria

- `ResolvedCapabilityRequirement` embeds the exact verified `LockedCapability`; Rust and Python expose the descriptor plus descriptor, input, output, stable-error, and safety digests without a catalog call.
- `resolve_package` returns frozen `ResolvedRuntimeInputs` retaining `ResolvedAgent`, exact validated `RuntimeBinding`, selected `RuntimeTarget`, and validated component descriptors through `target_components`.
- `load_resolved_agent` remains available for canonical IR inspection and is not accepted as a substitute for runtime compilation inputs.
- A Wave 4-style consumer succeeds after all YAML, lock, binding, and target paths are unavailable and performs no file or catalog reread.
- Outcomes and statuses expose structured result, stable error, diagnostic, and suspension projections; consumer code does not parse `canonical_json()`.
- Result schemas, stable error declarations, execution modes, descriptor identity/digest, invocation ID, and status request are validated against the exact locked descriptor before returning to Python.
- Admission responses exact-match actor/agent/task/session, admission request digest, catalog identity/digest, and authority revisions.
- Every required capability has one effective grant; every optional capability has exactly one grant or redacted denial diagnostic.
- `EffectiveCapabilityGrant` cannot broaden resources, execution modes, maximum effect, expiry, evidence, freshness, or preconditions.
- `AuthorityRevisionSet` is canonical, digest-verified, persisted with admission/session state, and cannot represent credentials or provider-native authorization tuples; invocation binds only `admission_id + grant_digest`, and current revisions are provider-derived at point of use.
- `EffectProposal` binds the exact effect semantics; `Suspension` contains only checkpoint ref, evidence kind, protected evidence-request ref, and proposal digest.
- `StatusRequest` carries invocation ID plus native W3C trace context, and catalog/admit/invoke/status all propagate their request context.
- Catalog issued/expiry times are validated and hashed; `CatalogFetchResult` is native and HTTP 304 is rejected unless the request sent the exact known digest.
- `ProviderAuthenticator` is invoked per call for all four provider operations, and only explicitly allowlisted credential headers are applied.
- Credentials and client-certificate configuration remain deployment-only and never enter baggage, canonical JSON, diagnostics, exceptions, logs, schemas, or stubs.
- Rust schemas, PyO3 projections, Python stubs, canonical goldens, and service variants pass byte-for-byte no-loss tests.
- The self-contained Crankshaft WFM conformance profile passes without a Crankshaft dependency or Crankshaft-specific portable field.
- Formatting, warning-denied Clippy, all Rust tests, all Python tests, ruff, pyright, schema drift, stub drift, and `git diff --check` pass.

## Wave 4 Consumer Gate

Wave 4 may begin only after its adapter-spike test consumes `ResolvedRuntimeInputs` and demonstrates:

1. no package YAML, capability lock, runtime binding, runtime target, or catalog reread;
2. exact locked schemas/errors/modes/safety available through frozen getters;
3. exact binding symbols and validated component descriptors available without reconstruction;
4. no `json.loads(canonical_json())` path for IR or provider response consumption;
5. no host conversation, intent clarification, task synthesis, streaming, or presentation responsibility moved into Kiteframe.

## Wave 5 Consumer Gate

Wave 5 may begin only after its enforcement-spike test consumes the Wave 3R values and demonstrates:

1. exact admission request/catalog/authority correlation and required/optional completeness;
2. monotonic `EffectiveCapabilityGrant` narrowing across every authority dimension;
3. descriptor-aware invocation outcome and status validation;
4. proposal-bound suspension without embedded approval text;
5. native trace propagation for catalog, admission, invocation, and status;
6. typed catalog freshness/304 behavior;
7. per-call deployment authentication, explicit credential-header allowlisting, mutual-TLS configuration isolation, and credential redaction;
8. no OpenFGA tuple, credential, endpoint, or provider-native policy field added to portable contracts.
