# Kiteframe V1 Wave 1 Authoritative Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish the Rust workspace, normative V1 schemas and diagnostics, hostile-input package loader, containment rules, and deterministic portable package digest.

**Architecture:** `kiteframe-contract` contains serializable Rust-owned contract types with no filesystem or runtime-framework dependencies. `kiteframe-core` turns untrusted directory bytes into an `AgentPackage` by applying fixed V1 limits, strict YAML validation, containment checks, and canonical hashing. A small schema binary generates checked-in JSON Schema fixtures from the same Rust types.

**Tech Stack:** Rust 1.97.1, Cargo edition 2024, serde, schemars, yaml-rust2, serde_yaml_ng, camino, sha2, serde_json_canonicalizer, proptest, libfuzzer-sys.

## Global Constraints

- Rust is the single authority for package parsing, exhaustive validation, containment, normalization, diagnostics, and portable digest construction.
- `agent.yaml` uses `apiVersion: kiteframe.dev/v1alpha1` and `kind: Agent`; bindings use `apiVersion: kiteframe.dev/binding/v1alpha1` and `kind: RuntimeBinding`.
- Unknown YAML fields are errors unless the Rust schema explicitly defines an extension map.
- Reject duplicate YAML keys, non-UTF-8 text assets, absolute paths, parent traversal, case-colliding paths, referenced symlinks, and resolved paths outside the package root.
- V1 fixed input limits are: 1 MiB per YAML document, 4 MiB per text asset, 32 MiB total referenced bytes, nesting depth 32, 10,000 collection entries, 128 aliases, and 16 nested-agent levels.
- V1 hashes use SHA-256, lowercase hexadecimal encoding, and domain-separated bytes.
- Typed values are normalized as RFC 8785 canonical JSON.
- Runtime binding bytes do not contribute to `portable_digest`.
- Portable content capture permission defaults to disabled; a package must declare allowed data classifications before a deployment policy can opt in.
- Portable core crates MUST NOT depend on Deep Agents, other runtime frameworks, OpenFGA, or OpenTelemetry SDK object types.

---

## File Structure

```text
Cargo.toml                                      # Workspace members and shared dependency versions
rust-toolchain.toml                             # Rust 1.97.1 toolchain pin
.cargo/config.toml                              # Workspace lint/test aliases only
crates/kiteframe-contract/
├── Cargo.toml
└── src/
    ├── lib.rs                                  # Public contract re-exports
    ├── diagnostic.rs                           # Stable codes, stages, retry classes, ordering
    ├── digest.rs                               # Sha256Digest value type and domain hashing
    ├── manifest.rs                             # Agent manifest and portable requirement types
    ├── binding.rs                              # Runtime binding and typed symbol names
    ├── package.rs                              # PackagePath, validated assets, AgentPackage
    └── schema.rs                               # Schema-version constants
crates/kiteframe-core/
├── Cargo.toml
└── src/
    ├── lib.rs                                  # Public load_package API
    ├── yaml.rs                                 # Strict bounded YAML scanner/deserializer
    ├── path.rs                                 # Lexical and filesystem containment checks
    ├── discover.rs                             # Referenced-only recursive package discovery
    ├── load.rs                                 # Validated AgentPackage assembly
    └── canonical.rs                            # RFC 8785 bytes and portable digest construction
crates/kiteframe-schema/
├── Cargo.toml
└── src/main.rs                                 # Deterministic checked-in schema generator
schemas/v1alpha1/
├── agent.schema.json
└── runtime-binding.schema.json
tests/fixtures/packages/
├── minimal/                                    # Small valid package
├── nested/                                     # Valid parent/child package
└── hostile/                                    # Duplicate, traversal, symlink, collision fixtures
fuzz/
├── Cargo.toml
└── fuzz_targets/strict_yaml.rs                  # Parser panic/limit target
```

### Task 1: Bootstrap the workspace and stable diagnostic contract

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.cargo/config.toml`
- Create: `crates/kiteframe-contract/Cargo.toml`
- Create: `crates/kiteframe-contract/src/lib.rs`
- Create: `crates/kiteframe-contract/src/diagnostic.rs`
- Test: `crates/kiteframe-contract/tests/diagnostic_contract.rs`

**Interfaces:**
- Consumes: no earlier interfaces.
- Produces: `Diagnostic`, `DiagnosticCode`, `DiagnosticCategory`, `DiagnosticSeverity`, `DiagnosticStage`, `RetryClass`, `SafeMessage`, `SourceRange`, and deterministic `Ord` for diagnostics.

- [ ] **Step 1: Write the failing diagnostic serialization and ordering tests**

```rust
use kiteframe_contract::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticSeverity,
    DiagnosticStage, RetryClass,
};

#[test]
fn stable_code_serializes_as_reserved_wire_value() {
    let diagnostic = Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Parse,
        "manifest is invalid",
    );
    let json = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(json["code"], "KF-PKG-001");
    assert_eq!(json["retry"], "never");
    assert!(json.get("details").is_some());
}

#[test]
fn diagnostics_sort_by_stage_path_range_then_code() {
    let mut diagnostics = vec![
        Diagnostic::error(
            DiagnosticCode::LockStale,
            DiagnosticCategory::Lock,
            DiagnosticStage::Lock,
            "lock is stale",
        ),
        Diagnostic::error(
            DiagnosticCode::PackageInvalid,
            DiagnosticCategory::Package,
            DiagnosticStage::Parse,
            "manifest is invalid",
        ),
    ];
    diagnostics.sort();
    assert_eq!(diagnostics[0].stage, DiagnosticStage::Parse);
}
```

- [ ] **Step 2: Run the tests and verify the crate is absent**

Run: `rtk cargo test -p kiteframe-contract --test diagnostic_contract`

Expected: FAIL because the workspace and `kiteframe-contract` package do not exist.

- [ ] **Step 3: Add the workspace and exact diagnostic enum**

```toml
# Cargo.toml
[workspace]
resolver = "3"
members = [
  "crates/kiteframe-contract",
  "crates/kiteframe-core",
  "crates/kiteframe-schema",
]

[workspace.package]
edition = "2024"
rust-version = "1.97.1"
license = "Apache-2.0"
repository = "https://github.com/swiftdiaries/kiteframe"

[workspace.dependencies]
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
```

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema)]
pub enum DiagnosticCode {
    #[serde(rename = "KF-PKG-001")]
    PackageInvalid,
    #[serde(rename = "KF-PKG-002")]
    PackageContainment,
    #[serde(rename = "KF-LOCK-001")]
    LockStale,
    #[serde(rename = "KF-LOCK-002")]
    LockTampered,
    #[serde(rename = "KF-CAT-001")]
    CatalogIncompatible,
    #[serde(rename = "KF-FEAT-001")]
    FeatureUnsupported,
    #[serde(rename = "KF-AUTH-001")]
    AdmissionDenied,
    #[serde(rename = "KF-AUTH-002")]
    AdmissionExpired,
    #[serde(rename = "KF-AUTH-003")]
    InvocationDenied,
    #[serde(rename = "KF-AUTH-004")]
    PolicyStale,
    #[serde(rename = "KF-CAP-001")]
    PreconditionMissing,
    #[serde(rename = "KF-CAP-002")]
    ResultInvalid,
    #[serde(rename = "KF-CAP-003")]
    OutcomeUnknown,
    #[serde(rename = "KF-AUDIT-001")]
    AuditUnavailable,
    #[serde(rename = "KF-RUNTIME-001")]
    ComponentUnresolved,
    #[serde(rename = "KF-RUNTIME-002")]
    RuntimeConstruction,
}
```

Implement `Diagnostic` with `#[serde(deny_unknown_fields)]`, redacted `BTreeMap<String, serde_json::Value>` details, and an `Ord` key of `(stage, package_path, source_range, code)`. Do not derive `Display` from protected details.

- [ ] **Step 4: Run the contract tests**

Run: `rtk cargo test -p kiteframe-contract --test diagnostic_contract`

Expected: PASS with both diagnostic tests green.

- [ ] **Step 5: Run formatting and lint checks**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy -p kiteframe-contract --all-targets -- -D warnings`

Expected: PASS.

- [ ] **Step 6: Commit the diagnostic foundation**

```bash
rtk git add Cargo.toml rust-toolchain.toml .cargo/config.toml crates/kiteframe-contract
rtk git commit -m "feat: establish kiteframe contract diagnostics"
```

### Task 2: Define exhaustive manifest, binding, package-path, and schema types

**Files:**
- Create: `crates/kiteframe-contract/src/schema.rs`
- Create: `crates/kiteframe-contract/src/manifest.rs`
- Create: `crates/kiteframe-contract/src/binding.rs`
- Create: `crates/kiteframe-contract/src/package.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Create: `crates/kiteframe-schema/Cargo.toml`
- Create: `crates/kiteframe-schema/src/main.rs`
- Create: `schemas/v1alpha1/agent.schema.json`
- Create: `schemas/v1alpha1/runtime-binding.schema.json`
- Test: `crates/kiteframe-contract/tests/schema_contract.rs`

**Interfaces:**
- Consumes: Wave 1 Task 1 diagnostics.
- Produces: `AgentManifest`, `AgentSpec`, `ModelRequirement`, `CapabilityRequirement`, `DelegationRequirement`, `ObservabilityRequirements`, `ContentCaptureRequirement`, `RuntimeBinding`, `BindingContentCapturePolicy`, `RegistrySymbol`, `PackagePath`, `ValidatedTextAsset`, and generated V1 alpha schemas.

- [ ] **Step 1: Write failing exhaustive-schema tests**

```rust
use kiteframe_contract::{AgentManifest, RuntimeBinding};

#[test]
fn manifest_rejects_unknown_fields() {
    let yaml = r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: support, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text, tool-calling], minContextTokens: 64000 }
  surprise: true
"#;
    assert!(serde_yaml_ng::from_str::<AgentManifest>(yaml).is_err());
}

#[test]
fn binding_contains_symbols_but_no_import_path_field() {
    let schema = schemars::schema_for!(RuntimeBinding);
    let text = serde_json::to_string(&schema).unwrap();
    assert!(text.contains("capabilityProvider"));
    assert!(!text.contains("importPath"));
    assert!(!text.contains("credentials"));
}
```

- [ ] **Step 2: Run the tests and verify missing types**

Run: `rtk cargo test -p kiteframe-contract --test schema_contract`

Expected: FAIL because `AgentManifest` and `RuntimeBinding` are undefined.

- [ ] **Step 3: Add the Rust-owned V1 alpha types**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentManifest {
    pub api_version: AgentSchemaVersion,
    pub kind: AgentKind,
    pub metadata: PackageIdentity,
    pub spec: AgentSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSpec {
    pub prompt: PromptRequirement,
    #[serde(default)]
    pub skills: Vec<PackagePath>,
    pub models: BTreeMap<ModelRole, ModelRequirement>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityRequirement>,
    #[serde(default)]
    pub delegation: Vec<DelegationRequirement>,
    #[serde(default)]
    pub features: FeatureRequirements,
    #[serde(default)]
    pub observability: ObservabilityRequirements,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservabilityRequirements {
    #[serde(default)]
    pub content_capture: ContentCaptureRequirement,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentCaptureRequirement {
    #[serde(default)]
    pub allowed: bool,
    #[serde(default)]
    pub classifications: BTreeSet<DataClassification>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeBinding {
    pub api_version: BindingSchemaVersion,
    pub kind: RuntimeBindingKind,
    pub metadata: RuntimeBindingMetadata,
    pub spec: RuntimeBindingSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeBindingSpec {
    pub models: BTreeMap<ModelRole, RegistrySymbol>,
    #[serde(default)]
    pub components: TypedComponentSymbols,
    pub capability_provider: RegistrySymbol,
    pub audit_sink: RegistrySymbol,
    #[serde(default)]
    pub content_capture: Option<BindingContentCapturePolicy>,
}
```

`BindingContentCapturePolicy` contains only `enabled`, a set of classifications, redaction-policy symbol, retention-policy symbol, access-policy symbol, and encrypted-content-store symbol. It contains no captured content, endpoint, credential, or portable grant. Use newtypes for names, versions, roles, runtime targets, features, and symbols. `PackagePath::new` must accept only normalized relative `/`-separated paths with no empty, `.`, or `..` segments.

- [ ] **Step 4: Generate and check in the schemas**

```rust
fn write_schema<T: schemars::JsonSchema>(
    destination: &std::path::Path,
) -> anyhow::Result<()> {
    let schema = schemars::schema_for!(T);
    let bytes = serde_json::to_vec_pretty(&schema)?;
    std::fs::write(destination, bytes)?;
    Ok(())
}
```

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Expected: creates `agent.schema.json` and `runtime-binding.schema.json` in stable key order.

- [ ] **Step 5: Run schema tests and reject generated drift**

Run: `rtk cargo test -p kiteframe-contract --test schema_contract`

Expected: PASS.

Run: `rtk cargo run -p kiteframe-schema -- --check schemas/v1alpha1`

Expected: PASS and no file changes.

- [ ] **Step 6: Commit the normative types and schemas**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-schema schemas/v1alpha1
rtk git commit -m "feat: define v1 alpha package schemas"
```

### Task 3: Reject duplicate, oversized, deeply nested, and alias-heavy YAML

**Files:**
- Create: `crates/kiteframe-core/Cargo.toml`
- Create: `crates/kiteframe-core/src/lib.rs`
- Create: `crates/kiteframe-core/src/yaml.rs`
- Test: `crates/kiteframe-core/tests/strict_yaml.rs`
- Create: `tests/fixtures/packages/hostile/duplicate-key/agent.yaml`
- Create: `tests/fixtures/packages/hostile/alias-limit/agent.yaml`

**Interfaces:**
- Consumes: `AgentManifest`, `RuntimeBinding`, `Diagnostic`, `DiagnosticCode`.
- Produces: `PackageLimits::V1`, `parse_manifest(bytes, limits) -> Result<AgentManifest, Vec<Diagnostic>>`, and `parse_binding(bytes, limits) -> Result<RuntimeBinding, Vec<Diagnostic>>`.

- [ ] **Step 1: Write failing bounded-parser tests**

```rust
use kiteframe_core::{parse_manifest, PackageLimits};

#[test]
fn duplicate_mapping_key_is_rejected_before_deserialization() {
    let yaml = br#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata:
  name: first
  name: second
  version: 0.1.0
spec: {}
"#;
    let errors = parse_manifest(yaml, PackageLimits::V1).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("duplicate key"));
}

#[test]
fn nesting_over_32_is_rejected() {
    let yaml = format!("root: {}null{}", "[".repeat(33), "]".repeat(33));
    let errors = parse_manifest(yaml.as_bytes(), PackageLimits::V1).unwrap_err();
    assert!(errors[0].message.as_str().contains("nesting depth"));
}
```

- [ ] **Step 2: Run the parser tests**

Run: `rtk cargo test -p kiteframe-core --test strict_yaml`

Expected: FAIL because `kiteframe-core` and the parser API do not exist.

- [ ] **Step 3: Implement event-level preflight followed by typed deserialization**

```rust
pub const V1: Self = Self {
    max_yaml_bytes: 1_048_576,
    max_text_asset_bytes: 4_194_304,
    max_total_referenced_bytes: 33_554_432,
    max_nesting_depth: 32,
    max_collection_entries: 10_000,
    max_aliases: 128,
    max_subagent_depth: 16,
};

pub fn parse_manifest(
    bytes: &[u8],
    limits: PackageLimits,
) -> Result<AgentManifest, Vec<Diagnostic>> {
    StrictYamlScanner::new(limits).scan(bytes)?;
    serde_yaml_ng::from_slice(bytes).map_err(|error| {
        vec![yaml_diagnostic(DiagnosticStage::Parse, error)]
    })
}
```

`StrictYamlScanner` must keep a stack of mapping key sets, increment depth on sequence/mapping start, count collection entries and alias events, and reject a repeated scalar key within the same mapping. It must return a source range from YAML event markers and stop before typed deserialization on any limit violation.

- [ ] **Step 4: Run bounded parser tests**

Run: `rtk cargo test -p kiteframe-core --test strict_yaml`

Expected: PASS for duplicate keys, byte limits, depth, collection length, and alias expansion count.

- [ ] **Step 5: Run a parser panic smoke test**

Run: `rtk cargo test -p kiteframe-core strict_yaml -- --nocapture`

Expected: PASS without panic for empty input, invalid UTF-8 YAML, truncated collections, and recursive aliases.

- [ ] **Step 6: Commit strict YAML parsing**

```bash
rtk git add crates/kiteframe-core tests/fixtures/packages/hostile
rtk git commit -m "feat: reject unsafe package yaml"
```

### Task 4: Enforce referenced-only discovery and filesystem containment

**Files:**
- Create: `crates/kiteframe-core/src/path.rs`
- Create: `crates/kiteframe-core/src/discover.rs`
- Create: `crates/kiteframe-core/src/load.rs`
- Modify: `crates/kiteframe-core/src/lib.rs`
- Test: `crates/kiteframe-core/tests/package_containment.rs`
- Create: `tests/fixtures/packages/minimal/agent.yaml`
- Create: `tests/fixtures/packages/minimal/prompts/system.md`
- Create: `tests/fixtures/packages/nested/agent.yaml`
- Create: `tests/fixtures/packages/nested/agents/escalation/agent.yaml`
- Create: `tests/fixtures/packages/hostile/traversal/agent.yaml`
- Create: `tests/fixtures/packages/hostile/case-collision/agent.yaml`

**Interfaces:**
- Consumes: strict parser, `PackagePath`, `AgentManifest`, `RuntimeBinding`, `ValidatedTextAsset`.
- Produces: `load_package(root: &Path, limits: PackageLimits) -> Result<AgentPackage, Vec<Diagnostic>>` for portable content and `load_runtime_binding(root: &Path, selected: &PackagePath, limits: PackageLimits) -> Result<RuntimeBinding, Vec<Diagnostic>>` for an explicitly selected binding.

- [ ] **Step 1: Write failing containment and discovery tests**

```rust
use kiteframe_core::{load_package, PackageLimits};

#[test]
fn unreferenced_file_does_not_enter_package() {
    let package = load_package(fixture("minimal"), PackageLimits::V1).unwrap();
    assert_eq!(package.prompt_assets.len(), 1);
    assert!(package.prompt_assets.contains_key("prompts/system.md"));
    assert!(!package.prompt_assets.contains_key("notes/private.txt"));
}

#[test]
fn parent_traversal_is_rejected() {
    let errors = load_package(fixture("hostile/traversal"), PackageLimits::V1)
        .unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
}

#[test]
fn nested_identity_cycle_is_rejected() {
    let errors = load_package(fixture("hostile/nested-cycle"), PackageLimits::V1)
        .unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}
```

- [ ] **Step 2: Run the containment tests**

Run: `rtk cargo test -p kiteframe-core --test package_containment`

Expected: FAIL because `load_package` is undefined.

- [ ] **Step 3: Implement lexical and filesystem containment**

```rust
fn open_referenced_text(
    root: &CanonicalPackageRoot,
    path: &PackagePath,
    budget: &mut ByteBudget,
) -> Result<ValidatedTextAsset, Diagnostic> {
    reject_symlink_components(root.as_path(), path)?;
    let candidate = root.as_path().join(path.as_std_path());
    let canonical = candidate.canonicalize().map_err(package_io_diagnostic)?;
    if !canonical.starts_with(root.as_path()) {
        return Err(containment_diagnostic(path, "resolved path escapes package root"));
    }
    let bytes = read_bounded(&canonical, budget)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| containment_diagnostic(path, "text asset is not UTF-8"))?;
    Ok(ValidatedTextAsset::new(path.clone(), text))
}
```

Portable discovery starts from `agent.yaml` and reads only `spec.prompt.system`, `spec.skills`, and declared `spec.delegation[*].agent`. `load_runtime_binding` reads one caller-selected contained path such as `bindings/deepagents.yaml`; the loader never scans a binding directory or guesses a runtime target. It lowercases normalized paths into a collision map, tracks canonical package identities across recursion, and sorts every map by `PackagePath`.

- [ ] **Step 4: Run package containment tests**

Run: `rtk cargo test -p kiteframe-core --test package_containment`

Expected: PASS for valid minimal/nested packages and all traversal, symlink, case-collision, missing-file, non-UTF-8, cycle, duplicate-identity, and byte-budget fixtures.

- [ ] **Step 5: Verify platform-specific symlink behavior**

Run: `rtk cargo test -p kiteframe-core --test package_containment symlink -- --nocapture`

Expected: PASS on Unix; the test is explicitly marked unsupported only on targets that cannot create symlinks.

- [ ] **Step 6: Commit contained package discovery**

```bash
rtk git add crates/kiteframe-core tests/fixtures/packages
rtk git commit -m "feat: load only contained package assets"
```

### Task 5: Produce deterministic domain-separated portable digests

**Files:**
- Create: `crates/kiteframe-contract/src/digest.rs`
- Create: `crates/kiteframe-core/src/canonical.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Modify: `crates/kiteframe-core/src/load.rs`
- Test: `crates/kiteframe-core/tests/portable_digest.rs`

**Interfaces:**
- Consumes: fully validated `AgentPackage` inputs from Task 4.
- Produces: `Sha256Digest`, `canonical_json<T: Serialize>(&T) -> Result<Vec<u8>, Diagnostic>`, `hash_domain(domain, chunks)`, and `AgentPackage::portable_digest`.

- [ ] **Step 1: Write failing semantic-equivalence and asset-change tests**

```rust
#[test]
fn yaml_formatting_does_not_change_portable_digest() {
    let a = load_package(fixture("digest/format-a"), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/format-b"), PackageLimits::V1).unwrap();
    assert_eq!(a.portable_digest, b.portable_digest);
}

#[test]
fn prompt_bytes_change_portable_digest() {
    let a = load_package(fixture("digest/prompt-a"), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/prompt-b"), PackageLimits::V1).unwrap();
    assert_ne!(a.portable_digest, b.portable_digest);
}

#[test]
fn binding_change_does_not_change_portable_digest() {
    let a = load_package(fixture("digest/binding-a"), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/binding-b"), PackageLimits::V1).unwrap();
    assert_eq!(a.portable_digest, b.portable_digest);
}
```

- [ ] **Step 2: Run digest tests**

Run: `rtk cargo test -p kiteframe-core --test portable_digest`

Expected: FAIL because `portable_digest` is not constructed.

- [ ] **Step 3: Implement canonical and domain-separated hashing**

```rust
pub fn hash_domain<'a>(
    domain: &'static [u8],
    chunks: impl IntoIterator<Item = &'a [u8]>,
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"kiteframe:v1\0");
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    for chunk in chunks {
        hasher.update((chunk.len() as u64).to_be_bytes());
        hasher.update(chunk);
    }
    Sha256Digest::from_bytes(hasher.finalize().into())
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, Diagnostic> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|error| canonicalization_diagnostic(error.to_string()))
}
```

Build `portable_digest` from canonical manifest semantics followed by ordered `(canonical path, exact asset bytes)` hashes and ordered child portable digests. Exclude every binding value and absolute root path.

- [ ] **Step 4: Add property tests for ordering and formatting**

```rust
proptest! {
    #[test]
    fn map_insertion_order_never_changes_canonical_bytes(
        entries in proptest::collection::vec(("[a-z]{1,8}", any::<u32>()), 1..32)
    ) {
        let forward: BTreeMap<_, _> = entries.iter().cloned().collect();
        let reverse: BTreeMap<_, _> = entries.into_iter().rev().collect();
        prop_assert_eq!(canonical_json(&forward).unwrap(), canonical_json(&reverse).unwrap());
    }
}
```

Run: `rtk cargo test -p kiteframe-core --test portable_digest`

Expected: PASS.

- [ ] **Step 5: Verify deterministic results across two clean invocations**

Run: `rtk cargo test -p kiteframe-core yaml_formatting_does_not_change_portable_digest -- --exact`

Expected: PASS twice with the same checked-in expected lowercase SHA-256 value.

- [ ] **Step 6: Commit canonical package hashing**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-core tests/fixtures/packages/digest
rtk git commit -m "feat: compute deterministic portable digests"
```

### Task 6: Add schema drift, hostile corpus, and parser fuzz gates

**Files:**
- Create: `fuzz/Cargo.toml`
- Create: `fuzz/fuzz_targets/strict_yaml.rs`
- Create: `crates/kiteframe-core/tests/hostile_corpus.rs`
- Create: `.github/workflows/ci.yml`
- Modify: `README.md`

**Interfaces:**
- Consumes: all Wave 1 APIs.
- Produces: repeatable Wave 1 CI gates and documented package-checking contract; no new public Rust API.

- [ ] **Step 1: Write the hostile-corpus table test**

```rust
#[test]
fn hostile_fixture_corpus_fails_closed() {
    for (name, code) in [
        ("duplicate-key", "KF-PKG-001"),
        ("alias-limit", "KF-PKG-001"),
        ("traversal", "KF-PKG-002"),
        ("case-collision", "KF-PKG-002"),
        ("symlink", "KF-PKG-002"),
        ("non-utf8", "KF-PKG-002"),
        ("nested-cycle", "KF-PKG-001"),
    ] {
        let errors = load_package(fixture(&format!("hostile/{name}")), PackageLimits::V1)
            .unwrap_err();
        assert_eq!(errors[0].code.as_str(), code, "fixture {name}");
    }
}
```

- [ ] **Step 2: Run the corpus before adding missing fixtures**

Run: `rtk cargo test -p kiteframe-core --test hostile_corpus`

Expected: FAIL and name each absent or incorrectly classified fixture.

- [ ] **Step 3: Complete the corpus and fuzz target**

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let _ = kiteframe_core::parse_manifest(bytes, kiteframe_core::PackageLimits::V1);
});
```

The fuzz invariant is termination without panic, allocation beyond the fixed byte budget, or successful parse after any limit violation.

- [ ] **Step 4: Run all Wave 1 checks**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy --workspace --all-targets -- -D warnings`

Expected: PASS.

Run: `rtk cargo test --workspace`

Expected: PASS.

Run: `rtk cargo fuzz run strict_yaml -- -max_total_time=10`

Expected: exits after 10 seconds with no crash artifact.

Run: `rtk cargo run -p kiteframe-schema -- --check schemas/v1alpha1`

Expected: PASS with no schema drift.

- [ ] **Step 5: Document the delivered boundary**

Add a README section stating that Wave 1 validates and hashes portable package bytes only; it does not resolve capabilities, verify a lock, build runtime objects, or authorize an actor.

- [ ] **Step 6: Commit the Wave 1 verification gate**

```bash
rtk git add fuzz .github/workflows/ci.yml README.md crates/kiteframe-core/tests tests/fixtures/packages
rtk git commit -m "test: gate hostile package parsing"
```

## Wave 1 Exit Criteria

- `AgentManifest` and `RuntimeBinding` schemas are generated from Rust and checked for drift.
- Every hostile package class named in the design fails with `KF-PKG-001` or `KF-PKG-002`.
- Valid nested packages contain only referenced, UTF-8, non-symlink assets beneath the package root.
- Semantically equivalent YAML yields identical portable digests; prompt and skill byte changes alter the digest; binding changes do not.
- Diagnostics are redacted and deterministically ordered.
- No Wave 1 crate depends on a runtime, authorization, provider, or telemetry SDK.
