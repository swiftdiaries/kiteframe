# Kiteframe V1 Wave 2 Lock Resolve and CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve semantic capabilities into an immutable lock, verify every locked byte and safety field, negotiate runtime features, produce canonical `ResolvedAgent` IR, and expose the shared Rust pipeline through the four V1 CLI commands.

**Architecture:** `kiteframe-resolver` consumes only a validated `AgentPackage`, canonical catalog bytes, a selected `RuntimeBinding`, and trusted runtime/component metadata. Capability selection and lock verification are separate operations: only `lock` selects versions, while locked resolution never substitutes. `kiteframe-cli` delegates to the library and renders either human output or the same structured diagnostics.

**Tech Stack:** Rust 1.97.1, Cargo edition 2024, semver, jsonschema, clap, tempfile, serde, schemars, SHA-256, RFC 8785 canonical JSON, proptest.

## Global Constraints

- Capability names are stable semantic operations and versions follow SemVer.
- A breaking change to inputs, outputs, stable error meaning, resource selection, effect classification, idempotency, freshness, preconditions, confirmation, approval, or consent requires a major version change.
- Capability schemas use JSON Schema 2020-12; remote `$ref` is forbidden and all referenced schemas are bundled in canonical descriptor bytes.
- Descriptors MUST NOT contain endpoints, credentials, bearer grants, OpenFGA tuples, transport configuration, deployment topology, or runtime code symbols.
- `capability.lock` is generated, is never hand-edited, selects exact versions, and is updated only by `kiteframe lock` through an atomic replace after complete validation.
- `--locked` never contacts a provider and never substitutes a compatible version.
- Required unsupported features fail with `KF-FEAT-001`; optional unsupported features are omitted with a stable warning in `CompilationReport`.
- V1 effect classes are `read_only`, `reversible_write`, `irreversible_write`, and `external_side_effect`.
- V1 execution modes are `immediate`, `deferred`, and `suspendable`; streaming is not a portable V1 capability mode.
- Every non-read-only descriptor defines an idempotency scope and retention window.
- Rust remains authoritative for lock verification, feature negotiation, diagnostic ordering, and `ResolvedAgent` construction.

---

## File Structure

```text
crates/kiteframe-contract/src/
├── capability.rs                              # Descriptor, schemas, effects, evidence, errors
├── catalog.rs                                 # Canonical catalog identity and descriptor bundles
├── lock.rs                                    # Exact selections and component digests
├── feature.rs                                 # Versioned features and negotiation report entries
├── component.rs                               # Trusted runtime/component metadata without objects
└── ir.rs                                      # Immutable serializable ResolvedAgent
crates/kiteframe-resolver/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # lock_package, verify_lock, resolve_agent
    ├── descriptor.rs                          # Descriptor semantic validation and digest parts
    ├── catalog.rs                             # Canonical catalog validation and SemVer selection
    ├── lock.rs                                # Lock build, verify, and atomic write
    ├── feature.rs                             # Exact feature-set negotiation
    ├── model.rs                               # Model-role constraint satisfaction
    └── resolve.rs                             # IR assembly and resolved digest
crates/kiteframe-cli/
├── Cargo.toml
└── src/
    ├── main.rs                                # Clap entrypoint and exit mapping
    ├── command.rs                             # Check, lock, explain, compile argument types
    └── render.rs                              # Human and structured projections
schemas/v1alpha1/
├── capability-descriptor.schema.json
├── capability-catalog.schema.json
├── capability-lock.schema.json
└── resolved-agent.schema.json
tests/fixtures/
├── catalogs/support-v1.json
├── components/deepagents-test.json
├── locks/support-agent.lock
├── resolved/support-agent.json
└── resolution/                                # stale, tampered, incompatible, optional fixtures
```

### Task 1: Define capability, catalog, lock, feature, component, and IR contracts

**Files:**
- Create: `crates/kiteframe-contract/src/capability.rs`
- Create: `crates/kiteframe-contract/src/catalog.rs`
- Create: `crates/kiteframe-contract/src/lock.rs`
- Create: `crates/kiteframe-contract/src/feature.rs`
- Create: `crates/kiteframe-contract/src/component.rs`
- Create: `crates/kiteframe-contract/src/ir.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Modify: `crates/kiteframe-schema/src/main.rs`
- Create: `schemas/v1alpha1/capability-descriptor.schema.json`
- Create: `schemas/v1alpha1/capability-catalog.schema.json`
- Create: `schemas/v1alpha1/capability-lock.schema.json`
- Create: `schemas/v1alpha1/resolved-agent.schema.json`
- Test: `crates/kiteframe-contract/tests/capability_contract.rs`

**Interfaces:**
- Consumes: Wave 1 `Sha256Digest`, diagnostics, manifest requirement types, portable content-capture requirements, and schema constants.
- Produces: `CapabilityDescriptor`, `CapabilityCatalog`, `CapabilityLock`, `LockedCapability`, `FeatureId`, `RuntimeTargetDescriptor`, `ComponentMetadataCatalog`, `ResolvedAgent`, `ResolvedCapabilityRequirement`, `ResolvedContentCaptureRequirement`, and `CompilationReport`.

- [ ] **Step 1: Write failing safety-contract tests**

```rust
#[test]
fn effectful_descriptor_requires_idempotency() {
    let mut parts = descriptor_parts("cases.comment", "1.0.0");
    parts.effect = EffectClassification::ExternalSideEffect;
    parts.idempotency = IdempotencyRequirement::None;
    let errors = CapabilityDescriptor::try_new(parts).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("idempotency"));
}

#[test]
fn remote_schema_reference_is_rejected() {
    let mut parts = descriptor_parts("cases.read", "1.2.0");
    parts.input_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "https://example.invalid/case.json"
    });
    assert!(CapabilityDescriptor::try_new(parts).is_err());
}

#[test]
fn resolved_agent_schema_contains_no_credentials_or_runtime_objects() {
    let schema = serde_json::to_string(&schemars::schema_for!(ResolvedAgent)).unwrap();
    assert!(!schema.contains("credential"));
    assert!(!schema.contains("endpoint"));
    assert!(!schema.contains("runtimeObject"));
}
```

- [ ] **Step 2: Run contract tests**

Run: `rtk cargo test -p kiteframe-contract --test capability_contract`

Expected: FAIL because the Wave 2 contract types do not exist.

- [ ] **Step 3: Add exact capability safety types**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum IdempotencyRequirement {
    None,
    Required {
        scope: IdempotencyScope,
        retention_seconds: NonZeroU64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityDescriptor {
    identity: CapabilityIdentity,
    summary: String,
    input_schema: JsonSchema2020_12,
    output_schema: JsonSchema2020_12,
    stable_errors: Vec<CapabilityErrorDescriptor>,
    execution_modes: NonEmptySet<ExecutionMode>,
    resource_selector_schema: ResourceSelectorSchema,
    effect: EffectClassification,
    idempotency: IdempotencyRequirement,
    freshness: FreshnessRequirement,
    preconditions: Vec<PreconditionDescriptor>,
    confirmation: ConfirmationRequirement,
    approval: ApprovalRequirement,
    consent: ConsentRequirement,
    descriptor_digest: Sha256Digest,
}
```

Keep validated aggregate fields private, expose read-only getters, and construct them through validating `try_new`/deserialization entrypoints backed by internal raw wire structs. Define evidence requirements independently, make all collections deterministic (`BTreeMap`, sorted unique vectors, or `NonEmptySet`), and validate that descriptor digest fields are absent while computing canonical bytes and equal after construction.

- [ ] **Step 4: Add immutable IR and report types**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedAgent {
    schema_version: IrSchemaVersion,
    package_identity: PackageIdentity,
    portable_digest: Sha256Digest,
    lock_digest: Sha256Digest,
    binding_digest: Sha256Digest,
    resolved_digest: Sha256Digest,
    prompts: BTreeMap<PackagePath, ValidatedTextAsset>,
    skills: BTreeMap<PackagePath, ValidatedTextAsset>,
    models: BTreeMap<ModelRole, ResolvedModelRequirement>,
    capability_requirements: Vec<ResolvedCapabilityRequirement>,
    subagents: Vec<ResolvedSubagent>,
    required_features: FeatureSet,
    optional_features: FeatureSet,
    content_capture: ResolvedContentCaptureRequirement,
    compilation_report: CompilationReport,
}
```

The only constructor is `ResolvedAgent::try_new(ResolvedAgentParts)`. The `resolved_digest` field is excluded while hashing the IR payload, then inserted into the immutable final value; callers receive read-only getters and cannot use a public struct literal.

- [ ] **Step 5: Generate and verify all Wave 2 schemas**

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Expected: writes the four new schema files.

Run: `rtk cargo test -p kiteframe-contract --test capability_contract`

Expected: PASS.

- [ ] **Step 6: Commit capability and IR contracts**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-schema schemas/v1alpha1
rtk git commit -m "feat: define capability lock and resolved ir contracts"
```

### Task 2: Validate catalogs and select the highest compatible exact version

**Files:**
- Create: `crates/kiteframe-resolver/Cargo.toml`
- Create: `crates/kiteframe-resolver/src/lib.rs`
- Create: `crates/kiteframe-resolver/src/descriptor.rs`
- Create: `crates/kiteframe-resolver/src/catalog.rs`
- Create: `tests/fixtures/catalogs/support-v1.json`
- Test: `crates/kiteframe-resolver/tests/catalog_resolution.rs`

**Interfaces:**
- Consumes: `CapabilityRequirement`, `CapabilityDescriptor`, `CapabilityCatalog`.
- Produces: `ValidatedCatalog`, `validate_catalog(bytes)`, and `select_capabilities(requirements, catalog, policy_filter) -> Result<Vec<SelectedCapability>, Vec<Diagnostic>>`.

- [ ] **Step 1: Write failing deterministic selection tests**

```rust
#[test]
fn selects_highest_compatible_version() {
    let catalog = catalog_with_versions("cases.read", ["1.2.0", "1.9.3", "2.0.0"]);
    let selected = select_capabilities(
        &[requirement("cases.read", "^1.2", true)],
        &catalog,
        CandidatePolicy::AllowAll,
    ).unwrap();
    assert_eq!(selected[0].descriptor.identity.version.to_string(), "1.9.3");
}

#[test]
fn policy_can_only_remove_candidates() {
    let catalog = catalog_with_versions("cases.read", ["1.2.0", "1.9.3"]);
    let selected = select_capabilities(
        &[requirement("cases.read", "^1.2", true)],
        &catalog,
        CandidatePolicy::exact(["cases.read@1.2.0"]),
    ).unwrap();
    assert_eq!(selected[0].descriptor.identity.version.to_string(), "1.2.0");
}

#[test]
fn reordered_catalog_bytes_select_identically_after_canonical_validation() {
    let a = validate_catalog(include_bytes!("fixtures/catalog-a.json")).unwrap();
    let b = validate_catalog(include_bytes!("fixtures/catalog-b.json")).unwrap();
    assert_eq!(a.catalog_digest(), b.catalog_digest());
}
```

- [ ] **Step 2: Run resolver tests**

Run: `rtk cargo test -p kiteframe-resolver --test catalog_resolution`

Expected: FAIL because the resolver crate does not exist.

- [ ] **Step 3: Implement descriptor and catalog validation**

```rust
pub fn validate_catalog(bytes: &[u8]) -> Result<ValidatedCatalog, Vec<Diagnostic>> {
    let catalog: CapabilityCatalog = serde_json::from_slice(bytes)
        .map_err(catalog_parse_diagnostics)?;
    let mut descriptors = catalog.descriptors;
    descriptors.sort_by(|a, b| {
        (a.identity().name(), a.identity().version())
            .cmp(&(b.identity().name(), b.identity().version()))
    });
    reject_duplicate_identities(&descriptors)?;
    for descriptor in &descriptors {
        descriptor.validate()?;
        verify_descriptor_digest(descriptor)?;
    }
    ValidatedCatalog::new(catalog.metadata, descriptors)
}
```

Compile every input/output schema with Draft 2020-12, walk `$ref` values to reject absolute/remote URIs, require all local references to exist in the bundle, and hash stable errors plus safety metadata independently.

- [ ] **Step 4: Implement stable SemVer selection**

Sort candidates by parsed `semver::Version`, apply the manifest requirement and policy filter as intersections, select `.max()`, and return `KF-CAT-001` for a required capability with no candidate. Preserve an optional miss as a stable compilation warning for later IR assembly.

- [ ] **Step 5: Run catalog tests and properties**

Run: `rtk cargo test -p kiteframe-resolver --test catalog_resolution`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-resolver catalog_order_never_changes_selection`

Expected: PASS for randomized descriptor order.

- [ ] **Step 6: Commit catalog validation and selection**

```bash
rtk git add crates/kiteframe-resolver tests/fixtures/catalogs
rtk git commit -m "feat: resolve deterministic capability versions"
```

### Task 3: Generate, atomically write, and exhaustively verify capability locks

**Files:**
- Create: `crates/kiteframe-resolver/src/lock.rs`
- Modify: `crates/kiteframe-resolver/src/lib.rs`
- Create: `tests/fixtures/locks/support-agent.lock`
- Create: `tests/fixtures/resolution/stale-package/`
- Create: `tests/fixtures/resolution/tampered-descriptor/`
- Test: `crates/kiteframe-resolver/tests/lock_contract.rs`

**Interfaces:**
- Consumes: validated package, validated catalog, selected capabilities.
- Produces: `lock_package(package, catalog, policy) -> CapabilityLock`, `verify_lock(package, lock, catalog: Option<&ValidatedCatalog>)`, and `write_lock_atomic(path, lock)`.

- [ ] **Step 1: Write failing stale and tamper tests**

```rust
#[test]
fn locked_compile_never_substitutes_another_compatible_version() {
    let package = support_package();
    let lock = lock_selecting("cases.read", "1.2.0");
    let catalog = catalog_with_versions("cases.read", ["1.3.0"]);
    let errors = verify_lock(&package, &lock, Some(&catalog)).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-LOCK-001");
}

#[test]
fn changed_safety_metadata_is_tampering() {
    let package = support_package();
    let mut catalog = support_catalog();
    catalog.descriptor_mut("cases.comment", "1.0.0").approval =
        ApprovalRequirement::None;
    let errors = verify_lock(&package, &support_lock(), Some(&catalog)).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-LOCK-002");
}
```

- [ ] **Step 2: Run lock tests**

Run: `rtk cargo test -p kiteframe-resolver --test lock_contract`

Expected: FAIL because lock build and verification are undefined.

- [ ] **Step 3: Implement exact lock construction**

```rust
pub struct LockedCapability {
    identity: CapabilityIdentity,
    descriptor: CapabilityDescriptor,
    descriptor_digest: Sha256Digest,
    input_schema_digest: Sha256Digest,
    output_schema_digest: Sha256Digest,
    stable_error_set_digest: Sha256Digest,
    safety_metadata_digest: Sha256Digest,
}
```

`descriptor` is the canonical locked descriptor bundle used by offline `--locked` verification; its nested local schema definitions travel with the lock. `CapabilityLock` also records lock schema version, package portable digest, catalog identity/digest/revision, resolver version, and exact resolved feature set. Sort locked capabilities by `(name, version)` before hashing.

- [ ] **Step 4: Implement no-provider locked verification and atomic output**

```rust
pub fn write_lock_atomic(path: &Path, lock: &CapabilityLock) -> Result<(), Diagnostic> {
    let parent = path.parent().ok_or_else(lock_parent_diagnostic)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(lock_io_diagnostic)?;
    std::io::Write::write_all(&mut temporary, &canonical_json(lock)?)
        .map_err(lock_io_diagnostic)?;
    temporary.as_file().sync_all().map_err(lock_io_diagnostic)?;
    temporary.persist(path).map_err(|error| lock_io_diagnostic(error.error))?;
    sync_parent_directory(parent)?;
    Ok(())
}
```

When no catalog is supplied, verify package digest, lock self-digest, schema version, resolver compatibility, and every embedded canonical descriptor bundle/digest. When a catalog is supplied, also verify exact presence, catalog revision, and catalog digest.

- [ ] **Step 5: Run lock contract tests**

Run: `rtk cargo test -p kiteframe-resolver --test lock_contract`

Expected: PASS for stale package, absent version, tampered schema, tampered stable error, tampered safety metadata, unsupported resolver, atomic replacement, and unchanged output after a failed lock attempt.

- [ ] **Step 6: Commit lock generation and verification**

```bash
rtk git add crates/kiteframe-resolver tests/fixtures/locks tests/fixtures/resolution
rtk git commit -m "feat: generate and verify capability locks"
```

### Task 4: Negotiate features and resolve model, binding, subagent, and digest state

**Files:**
- Create: `crates/kiteframe-resolver/src/feature.rs`
- Create: `crates/kiteframe-resolver/src/model.rs`
- Create: `crates/kiteframe-resolver/src/resolve.rs`
- Modify: `crates/kiteframe-resolver/src/lib.rs`
- Create: `tests/fixtures/components/deepagents-test.json`
- Test: `crates/kiteframe-resolver/tests/resolved_agent.rs`
- Test: `crates/kiteframe-resolver/tests/authority_monotonicity.rs`

**Interfaces:**
- Consumes: verified `AgentPackage`, `CapabilityLock`, `RuntimeBinding`, `RuntimeTargetDescriptor`, and `ComponentMetadataCatalog`.
- Produces: `ResolutionInput`, `resolve_agent(input) -> Result<ResolvedAgent, Vec<Diagnostic>>`, exact `FeatureSet`, and `CompilationReport`.

- [ ] **Step 1: Write failing required/optional feature and model tests**

```rust
#[test]
fn unsupported_required_feature_stops_resolution() {
    let mut input = resolution_fixture();
    input.package.manifest.spec.features.required =
        features(["kiteframe.capability.deferred@1"]);
    input.target.supported_features = features(["kiteframe.capability.point-of-use-auth@1"]);
    let errors = resolve_agent(input).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-FEAT-001");
}

#[test]
fn optional_model_falls_back_only_when_primary_satisfies_every_constraint() {
    let mut input = resolution_fixture();
    input.components.remove("models.anthropic.haiku");
    let resolved = resolve_agent(input).unwrap();
    assert_eq!(resolved.models()["fast"].symbol().as_str(), "models.anthropic.sonnet");
    assert!(resolved.compilation_report().entries().iter().any(|entry| {
        entry.machine_code == "KF-MODEL-OPTIONAL-FALLBACK"
    }));
}

#[test]
fn binding_cannot_add_capability_or_delegation() {
    let input = resolution_fixture_with_binding_authority_injection();
    assert_eq!(
        resolve_agent(input).unwrap_err()[0].code.as_str(),
        "KF-PKG-001"
    );
}

#[test]
fn binding_cannot_enable_or_broaden_content_capture() {
    let mut input = resolution_fixture();
    input.package.manifest.spec.observability.content_capture.allowed = false;
    input.binding.spec.content_capture = Some(binding_capture(["confidential"]));
    let errors = resolve_agent(input).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}
```

- [ ] **Step 2: Run resolved-agent tests**

Run: `rtk cargo test -p kiteframe-resolver --test resolved_agent`

Expected: FAIL because feature/model negotiation and IR assembly are undefined.

- [ ] **Step 3: Implement exact feature negotiation**

```rust
pub fn negotiate_features(
    required: &FeatureSet,
    optional: &FeatureSet,
    supported: &FeatureSet,
) -> Result<FeatureNegotiation, Vec<Diagnostic>> {
    let missing_required = required.difference_compatible(supported);
    if !missing_required.is_empty() {
        return Err(missing_required.into_iter()
            .map(required_feature_diagnostic)
            .collect());
    }
    let enabled_optional = optional.intersection_compatible(supported);
    let omitted_optional = optional.difference_compatible(supported);
    Ok(FeatureNegotiation { enabled_optional, omitted_optional })
}
```

Feature compatibility compares exact feature name plus supported major version; no runtime-version inference is permitted.

- [ ] **Step 4: Resolve models, subagents, and final digest**

Resolve each symbol against `ComponentMetadataCatalog` by expected `ComponentKind`. Check every model modality, tool-calling, structured-output, context, residency, and latency constraint. Recursively resolve declared child packages, reject duplicate package identities, and carry the parent delegation declaration into `ResolvedSubagent` without expanding it. Resolve content capture as the intersection of the package declaration and trusted deployment binding; absence or `allowed: false` yields a disabled requirement, and classifications can only be removed.

Compute `binding_digest` from canonical typed binding bytes and `resolved_digest` from domain-separated portable, lock, binding, negotiated-feature, model-resolution, capability-resolution, and child resolved digests.

- [ ] **Step 5: Run resolution and monotonicity tests**

Run: `rtk cargo test -p kiteframe-resolver --test resolved_agent`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-resolver --test authority_monotonicity`

Expected: PASS for properties that removing a capability candidate, narrowing a resource selector, shortening expiry metadata, or narrowing a delegation set never increases the resolved envelope.

- [ ] **Step 6: Commit immutable resolution**

```bash
rtk git add crates/kiteframe-resolver tests/fixtures/components
rtk git commit -m "feat: resolve immutable agent ir"
```

### Task 5: Expose the shared pipeline through `check`, `lock`, `explain`, and `compile`

**Files:**
- Create: `crates/kiteframe-cli/Cargo.toml`
- Create: `crates/kiteframe-cli/src/main.rs`
- Create: `crates/kiteframe-cli/src/command.rs`
- Create: `crates/kiteframe-cli/src/render.rs`
- Modify: `Cargo.toml`
- Test: `crates/kiteframe-cli/tests/commands.rs`

**Interfaces:**
- Consumes: Wave 1 `load_package`; Wave 2 catalog, lock, verification, negotiation, and resolution APIs.
- Produces: `kiteframe check`, `kiteframe lock`, `kiteframe explain`, and `kiteframe compile`; stable process exit categories.

- [ ] **Step 1: Write failing command tests**

```rust
#[test]
fn check_locked_does_not_require_catalog_access() {
    let output = command()
        .args(["check", fixture("support-agent"), "--locked", "--json"])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .assert()
        .success();
    let body: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(body["status"], "valid");
}

#[test]
fn compile_emits_canonical_ir_not_runtime_graph() {
    let output = command()
        .args([
            "compile", fixture("support-agent"),
            "--binding", fixture("support-agent/bindings/deepagents.yaml"),
            "--target", fixture("components/deepagents-test.json"),
            "--locked",
            "--json",
        ])
        .assert()
        .success();
    let body: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(body["schemaVersion"], "kiteframe.dev/ir/v1alpha1");
    assert!(body.get("compiledGraph").is_none());
}
```

- [ ] **Step 2: Run CLI tests**

Run: `rtk cargo test -p kiteframe-cli --test commands`

Expected: FAIL because the CLI crate does not exist.

- [ ] **Step 3: Add exact subcommands and exit mapping**

```rust
#[derive(clap::Subcommand)]
enum Command {
    Check(CheckArgs),
    Lock(LockArgs),
    Explain(ExplainArgs),
    Compile(CompileArgs),
}

#[repr(u8)]
enum ExitCategory {
    Success = 0,
    Package = 2,
    LockOrCatalog = 3,
    Resolution = 4,
    RuntimeTarget = 5,
}
```

`lock` requires `--catalog <canonical-json>` and defaults output to `<package>/capability.lock`. `compile` requires `--binding`, `--target`, and `--locked` for V1 alpha. `--json` writes one structured result to stdout and no human prose; human mode renders the same diagnostics to stderr.

- [ ] **Step 4: Implement redacted explain output**

`explain` lists package and digest identity, selected exact capability versions, model-role resolution, required/optional feature outcomes, precedence decisions, child delegation boundaries, and sorted diagnostics. It must render registry symbols but never component values, credentials, provider tokens, prompt bodies, or asset contents.

- [ ] **Step 5: Run CLI tests and smoke commands**

Run: `rtk cargo test -p kiteframe-cli --test commands`

Expected: PASS.

Run: `rtk cargo run -p kiteframe-cli -- check tests/fixtures/packages/minimal --json`

Expected: JSON status `valid`.

Run: `rtk cargo run -p kiteframe-cli -- compile tests/fixtures/packages/support-agent --binding tests/fixtures/packages/support-agent/bindings/deepagents.yaml --target tests/fixtures/components/deepagents-test.json --locked --json`

Expected: canonical `ResolvedAgent` JSON and exit 0.

- [ ] **Step 6: Commit the V1 CLI**

```bash
rtk git add Cargo.toml crates/kiteframe-cli
rtk git commit -m "feat: add kiteframe package cli"
```

### Task 6: Freeze golden IR, digest, schema, and diagnostic fixtures

**Files:**
- Create: `tests/fixtures/resolved/support-agent.json`
- Create: `tests/fixtures/resolved/support-agent.digests.json`
- Create: `crates/kiteframe-resolver/tests/golden_ir.rs`
- Create: `crates/kiteframe-cli/tests/golden_diagnostics.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`

**Interfaces:**
- Consumes: complete Wave 2 resolver and CLI.
- Produces: canonical fixture corpus consumed unchanged by Waves 3, 4, and 6.

- [ ] **Step 1: Write failing golden comparisons**

```rust
#[test]
fn resolved_support_agent_matches_checked_in_canonical_json() {
    let resolved = resolve_agent(resolution_fixture()).unwrap();
    let actual = canonical_json(&resolved).unwrap();
    let expected = include_bytes!("../../../tests/fixtures/resolved/support-agent.json");
    assert_eq!(actual.as_slice(), expected);
}

#[test]
fn all_diagnostic_codes_have_redacted_json_fixtures() {
    let actual = render_reserved_diagnostic_fixture();
    let expected = include_str!("fixtures/diagnostics.json");
    assert_eq!(actual, expected);
    assert!(!actual.contains("secret"));
    assert!(!actual.contains("prompt"));
}
```

- [ ] **Step 2: Run golden tests**

Run: `rtk cargo test -p kiteframe-resolver --test golden_ir`

Expected: FAIL until canonical fixture bytes and digest expectations are checked in.

- [ ] **Step 3: Generate fixtures through the public CLI**

Run: `rtk cargo run -p kiteframe-cli -- compile tests/fixtures/packages/support-agent --binding tests/fixtures/packages/support-agent/bindings/deepagents.yaml --target tests/fixtures/components/deepagents-test.json --locked --json --output tests/fixtures/resolved/support-agent.json`

Expected: creates canonical IR through the same command users run.

Record portable, lock, binding, and resolved digests in `support-agent.digests.json`; do not hand-calculate them.

- [ ] **Step 4: Run the full Wave 2 verification suite**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `rtk cargo test --workspace --all-features`

Expected: PASS.

Run: `rtk cargo run -p kiteframe-schema -- --check schemas/v1alpha1`

Expected: PASS with no drift.

- [ ] **Step 5: Document the lock and compile trust boundary**

Document that `lock` is the only mutating command; `compile` emits IR only; actor grants and point-of-use authorization are not part of deterministic resolution.

- [ ] **Step 6: Commit the Wave 2 golden gate**

```bash
rtk git add tests/fixtures/resolved crates/kiteframe-resolver/tests crates/kiteframe-cli/tests .github/workflows/ci.yml README.md
rtk git commit -m "test: freeze resolved agent contract"
```

## Wave 2 Exit Criteria

- Descriptor validation covers schemas, stable errors, execution modes, resource selectors, effects, idempotency, freshness, preconditions, confirmation, approval, and consent.
- `kiteframe lock` selects the highest compatible exact version and writes only after full validation.
- Locked verification rejects stale package bytes, absent versions, catalog drift, descriptor tampering, schema tampering, stable-error drift, safety drift, and unsupported resolver/schema versions.
- Required features and model roles fail closed; optional omissions and valid primary fallback are visible in `CompilationReport`.
- `ResolvedAgent` is canonical, credential-free, runtime-object-free, carries only narrowed content-capture classifications, and recursively includes only declared subagents.
- All four CLI commands use the library pipeline and emit deterministically ordered structured diagnostics.
- Golden IR and digest fixtures are checked in for cross-language and portability waves.
