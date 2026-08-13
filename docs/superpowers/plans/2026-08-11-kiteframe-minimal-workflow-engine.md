# Kiteframe Minimal Workflow Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current package/adapter architecture with one Rust harness
that runs durable author-authored workflows for first-party Python and
TypeScript clients over ACP, principal-scoped MCP catalogs, and default-deny
Cedar session policy.

**Architecture:** One Rust binary owns compilation, PostgreSQL state, model and
tool turns, MCP connections, Cedar authorization, ACP v1/v2 adapters, and all
transports. Python and TypeScript packages are typed process/transport clients
that bundle the same versioned binary and contain no workflow runtime. Tasks are
cumulative security-boundary slices; each ends with an independently testable
commit.

**Tech Stack:** Rust 1.97.1 / edition 2024, Tokio 1, Axum 0.8, SQLx 0.8 with
PostgreSQL 16, Reqwest 0.12 with rustls, Serde/serde_json 1, UUID 1, `time` 0.3,
RFC 8785/SHA-256, `agent-client-protocol = "=2.0.0"`,
`agent-client-protocol-http = "=2.0.0"`, `rmcp = "=3.1.2"`,
`cedar-policy = "=4.11.0"`,
`cedar-policy-mcp-schema-generator = "=0.6.0"`, Python 3.11+ with Maturin bin
wheels, and TypeScript on Bun with platform-specific optional packages.

**Design:**
`docs/superpowers/specs/2026-08-11-kiteframe-minimal-workflow-engine-design.md`

## Global Constraints

- The approved design is normative. Older Kiteframe specs and plans are
  historical evidence only.
- V1 is one Rust package, one harness process, and one PostgreSQL database. Do
  not add another workflow runtime, distributed workers, queues, leases, or
  provider-owned workflow state.
- Python installs with `uv add kiteframe`. TypeScript installs with
  `bun add kiteframe` and imports from `kiteframe/client`.
- Both thin clients bundle a version-matched `kiteframe-harness` executable.
  They contain no compiler, scheduler, policy evaluator, model loop, or durable
  workflow state.
- Pin `agent-client-protocol` and `agent-client-protocol-http` exactly at
  `2.0.0`. Keep ACP v1 enabled. Compile v2 only through the crate's
  `unstable_protocol_v2` feature and require an explicit runtime flag.
- `stdio`, WebSocket, and Streamable HTTP carry one version-neutral application
  command model. Remote clients prefer WebSocket and fall back only after a
  transport or upgrade incompatibility, never after authentication, policy,
  malformed-message, or application failures.
- Steering is not part of this plan. Do not add `steer`, prompt interruption,
  queued in-flight input, or steering events while implementing these tasks.
- PostgreSQL is the sole workflow authority. MCP credentials, bearer tokens,
  provider keys, prompts, model content, tool arguments/results, and human
  answers are never persisted in events or logged by default.
- `tools/list` is executed on each authenticated principal's MCP connection.
  Never fetch an administrator/global superset and filter it locally.
- The model-visible tools are exactly principal-visible catalog intersection
  immutable workflow bindings. Cedar gates invocation after a validated call is
  proposed; MCP servers still authorize every `tools/call`.
- Typed session policy is the only public V1 policy surface. Do not accept raw
  Cedar, add OpenFGA, or introduce workspace/deployment/organization/tenant
  policy scopes.
- Exact-tool rules bind qualified name plus descriptor digest. Capability tags
  come only from trusted configuration; unknown MCP tools receive
  `mcp_unknown`.
- Every Responses request sets `store: false`; PostgreSQL retains and replays
  complete ordered items. No provider conversation, background, hosted tool, or
  WebSocket model session is permitted.
- Strict schemas set `additionalProperties: false`, require every property, and
  encode optional values as nullable required values.
- Authors own control flow. The only operations are `call`, `goto`, `parallel`,
  `ask`, `done`, and `fail`; no model-authored graph or detached work exists.
- Record durable intent before every external call. Retry only read-only or
  idempotent effects, use status recovery where declared, and map unresolved
  effects to `outcome_unknown`.
- Preserve unrelated `site/`. Every commit stages only paths listed by its task.
- Use TDD: run the named focused test to observe failure, implement the smallest
  contract, run focused and cumulative gates, then commit.
- Current upstream contracts for this plan are the
  [ACP protocol](https://agentclientprotocol.com/),
  [v2 migration guide](https://agentclientprotocol.com/protocol/v2/migration),
  [ACP Rust SDK](https://github.com/agentclientprotocol/rust-sdk),
  [official MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk), and
  [`cedar-for-agents`](https://github.com/cedar-policy/cedar-for-agents/tree/main/rust).

---

## Final File Structure

```text
Cargo.toml
Cargo.lock
README.md
rust-toolchain.toml
.github/workflows/ci.yml
migrations/
  0001_session_authority.sql
  0002_workflow_engine.sql
src/
  lib.rs
  main.rs
  config.rs
  domain.rs
  hash.rs
  schema.rs
  dsl/{mod.rs,diagnostic.rs,lexer.rs,ast.rs,parser.rs,ir.rs,compiler.rs}
  model/{mod.rs,types.rs,responses.rs}
  tools/{mod.rs,descriptor.rs,bound.rs,catalog.rs}
  mcp/{mod.rs,config.rs,connection.rs,discovery.rs,bound_tool.rs}
  policy/{mod.rs,types.rs,compile.rs,authorize.rs}
  store/{mod.rs,models.rs,postgres.rs}
  engine/{mod.rs,transition.rs,turn.rs,scheduler.rs,recovery.rs}
  protocol/{mod.rs,application.rs,types.rs,v1.rs,v2.rs,transport.rs}
clients/
  python/{pyproject.toml,src/kiteframe/,tests/}
  typescript/{package.json,src/,test/,npm/}
examples/support.kite
tests/
  fixtures/{dsl/,acp/,mcp/,policy/}
  support/
  repository_shape.rs
  schema_and_tools.rs
  dsl_parser.rs
  dsl_compiler.rs
  responses_conformance.rs
  session_store.rs
  mcp_catalog.rs
  cedar_policy.rs
  execution_store.rs
  transition_engine.rs
  sequential_runtime.rs
  structured_runtime.rs
  recovery.rs
  acp_conformance.rs
  transports.rs
  e2e_restart.rs
```

## Dependency Flow

```text
domain/schema/tools -> DSL compiler -> session catalog + Cedar
                    -> execution store -> engine
engine + session authority -> ACP application adapters -> transports
ACP fixtures + bundled binary -> Python and TypeScript clients
all prior slices -> restart/package acceptance suite
```

## Task 1: Establish the Single-Harness Repository Shell

**Files:**

- Replace: `Cargo.toml`
- Regenerate: `Cargo.lock`
- Create: `src/lib.rs`, `src/main.rs`, `tests/repository_shape.rs`
- Modify: `.github/workflows/ci.yml`, `README.md`
- Delete: `crates/`, `python/`, `fuzz/`, `openfga/`, `runtime-targets/`,
  `schemas/`, `scripts/`, and old tracked `tests/`

**Interfaces:** Produces one package named `kiteframe` and one binary named
`kiteframe-harness`; preserves `docs/`, `LICENSE`, `rust-toolchain.toml`, and
unrelated `site/`.

- [ ] **Step 1: Create the compileable package shell**

```toml
[package]
name = "kiteframe"
version = "0.1.0"
edition = "2024"
rust-version = "1.97.1"
license = "Apache-2.0"
default-run = "kiteframe-harness"

[lib]
path = "src/lib.rs"

[[bin]]
name = "kiteframe-harness"
path = "src/main.rs"

[features]
default = []
acp-v2 = []

[lints.rust]
unsafe_code = "forbid"
```

`src/lib.rs` exports `VERSION`; `src/main.rs` prints the version until service
composition lands.

- [ ] **Step 2: Write and run the failing shape test**

```rust
#[test]
fn repository_contains_one_runtime() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for removed in ["crates", "python", "fuzz", "openfga", "runtime-targets", "schemas", "scripts"] {
        assert!(!root.join(removed).exists(), "obsolete path: {removed}");
    }
    assert!(root.join("src/main.rs").is_file());
}
```

Run: `cargo test --test repository_shape`

Expected: FAIL on the first legacy directory.

- [ ] **Step 3: Remove only superseded tracked/generated paths**

Use `git rm -r` for tracked paths. Inspect ignored content first and delete only
generated caches underneath those paths. Never remove `docs/`, root `target/`,
or `site/`.

- [ ] **Step 4: Install honest baseline CI and README**

CI runs `cargo fmt --all --check`, warning-denied Clippy, and all Rust tests.
README says the replacement is under implementation and links the design and
plan; it claims no runtime behavior yet.

- [ ] **Step 5: Verify and commit**

```bash
cargo generate-lockfile
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
git add Cargo.toml Cargo.lock README.md .github/workflows/ci.yml src tests/repository_shape.rs
git add -u crates python fuzz openfga runtime-targets schemas scripts tests
git commit -m "refactor: establish single kiteframe harness"
```

## Task 2: Add Closed Schemas, Domain IDs, and Tool Catalog Contracts

**Files:**

- Modify: `Cargo.toml`, `Cargo.lock`, `src/lib.rs`
- Create: `src/domain.rs`, `src/hash.rs`, `src/schema.rs`,
  `src/tools/{mod.rs,descriptor.rs,bound.rs,catalog.rs}`,
  `tests/schema_and_tools.rs`

**Interfaces:**

```rust
pub struct SessionId(uuid::Uuid);
pub struct PrincipalId(String);
pub struct CatalogEpochId(uuid::Uuid);
pub struct PolicyRevision(u64);
pub struct PermissionRequestId(uuid::Uuid);

pub struct CatalogTool {
    pub qualified_name: String,
    pub server_alias: String,
    pub description: String,
    pub input: ClosedSchema,
    pub output: ClosedSchema,
    pub descriptor_digest: String,
    pub capabilities: std::collections::BTreeSet<Capability>,
    pub effect: EffectKind,
    pub recovery: RecoverySupport,
}

pub struct ToolCatalogSnapshot {
    pub session_id: SessionId,
    pub epoch_id: CatalogEpochId,
    pub digest: String,
    pub tools: std::collections::BTreeMap<String, CatalogTool>,
}

#[async_trait::async_trait]
pub trait BoundTool: Send + Sync {
    fn descriptor(&self) -> &CatalogTool;
    async fn invoke(&self, call: ToolCall) -> Result<serde_json::Value, ToolError>;
    async fn status(&self, key: &IdempotencyKey) -> Result<ToolStatus, ToolError>;
}

pub trait ToolResolver: Send + Sync {
    fn bound_tool(
        &self,
        session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<std::sync::Arc<dyn BoundTool>, ToolError>;
}
```

- [ ] **Step 1: Add foundational dependencies**

```toml
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_json_canonicalizer = "0.3"
sha2 = "0.10"
thiserror = "2"
time = { version = "0.3", features = ["serde"] }
uuid = { version = "1", features = ["serde", "v7"] }
```

- [ ] **Step 2: Write failing strict-schema and catalog tests**

Test required-nullable encoding, unknown/missing property rejection, canonical
descriptor digests, qualified-name collision rejection, effectful tools without
recovery rejection, `mcp_unknown` defaulting, and redacted `Secret` debug.

Run: `cargo test --test schema_and_tools`

Expected: FAIL because the modules do not exist.

- [ ] **Step 3: Implement the closed vocabulary and identities**

`ClosedSchema` supports primitives, nullable values, homogeneous arrays, named
closed objects, and primitive enums. Define all durable IDs/statuses/failures,
including `waiting_for_permission` and `tool_contract_changed`. Canonical JSON
uses RFC 8785 and SHA-256 lowercase hex.

- [ ] **Step 4: Implement catalog and bound-tool invariants**

Qualify MCP tools as `<server-alias>/<tool-name>`. A deployment-owned
`InProcessToolRegistry` exposes trusted built-ins through the same
`ToolCatalogSnapshot` and `ToolResolver` contracts without making them MCP
servers. Hash the complete normalized descriptor. Reject duplicates,
unsupported schemas, untrusted capability tags, and unsafe effect recovery.
`ToolCall` carries session, run, frame, policy revision, validated arguments,
and idempotency key.

- [ ] **Step 5: Verify and commit**

```bash
cargo test --test schema_and_tools
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add Cargo.toml Cargo.lock src tests/schema_and_tools.rs
git commit -m "feat: define schemas and tool catalog contracts"
```

## Task 3: Parse the Author-Authored Workflow DSL

**Files:** Create `src/dsl/{mod.rs,diagnostic.rs,lexer.rs,ast.rs,parser.rs}`,
`tests/dsl_parser.rs`, `tests/fixtures/dsl/support.kite`, and
`examples/support.kite`; modify `src/lib.rs`.

**Interfaces:**

```rust
pub fn parse(source: &str, limits: ParseLimits) -> Result<SourceFile, Vec<Diagnostic>>;
pub enum StatementAst {
    Call(CallAst), Parallel(Vec<CallAst>),
    Ask { binding: String, answer_type: TypeAst, question: ValueRefAst },
    Goto(CallAst), Done(ValueRefAst),
    Fail { code: ValueRefAst, message: ValueRefAst },
}
```

- [ ] **Step 1: Check in the approved support workflow fixture**

Copy the design's full `Evidence`, `support`, `triage`, `inspect_logs`,
`inspect_metrics`, and `compose` example verbatim to both fixture paths.

- [ ] **Step 2: Write and run failing parser tests**

Assert all six operations, spans, triple-quoted prompts, optionals, records,
limits, and bounded diagnostics. Run `cargo test --test dsl_parser`; expect a
missing-module failure.

- [ ] **Step 3: Implement bounded lexer and recursive-descent parser**

Recognize only the fixed grammar. Bound source bytes, tokens, identifier bytes,
nesting, and diagnostics. Require prompt/tools/outcomes in task bodies and one
terminal statement per outcome; reject trailing tokens.

- [ ] **Step 4: Add hostile fixtures and verify**

Cover invalid escapes, unterminated prompts, excessive nesting, duplicate
workflow, empty task/outcome/parallel, and statements after a terminal.

```bash
cargo test --test dsl_parser
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add src/dsl src/lib.rs tests/dsl_parser.rs tests/fixtures/dsl examples/support.kite
git commit -m "feat: parse author-authored workflows"
```

## Task 4: Compile Immutable Workflows Against a Principal Catalog

**Files:** Create `src/dsl/{ir.rs,compiler.rs}`, `tests/dsl_compiler.rs`,
`tests/fixtures/dsl/support.compiled.json`; modify `src/dsl/mod.rs`.

**Interfaces:**

```rust
pub fn compile(
    source: &str,
    catalog: &ToolCatalogSnapshot,
    ceilings: &Limits,
) -> Result<CompiledWorkflow, Vec<Diagnostic>>;

pub struct CompiledToolBinding {
    pub qualified_name: String,
    pub catalog_epoch_id: CatalogEpochId,
    pub descriptor_digest: String,
    pub descriptor: CatalogTool,
}
```

- [ ] **Step 1: Write failing success and rejection matrices**

Assert strict `complete_task`, exact qualified tool resolution, inaccessible
tool rejection, graph/type/binding errors, recursive `call` rejection,
unreachable tasks, and limit ceilings.

Run: `cargo test --test dsl_compiler`; expect missing compiler types.

- [ ] **Step 2: Implement type, graph, binding, and outcome compilation**

Resolve closed schemas, normalize omitted optionals to null, allow bounded
`goto` cycles, reject call recursion, and emit immutable statements with no
dynamic names.

- [ ] **Step 3: Implement deterministic identity**

Hash RFC 8785 JSON containing DSL version 1, exact UTF-8 source, and complete
descriptors ordered by qualified name. Persist each descriptor digest and
catalog epoch in the compiled binding.

- [ ] **Step 4: Freeze golden output, verify, and commit**

```bash
cargo test --test dsl_compiler
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add src/dsl tests/dsl_compiler.rs tests/fixtures/dsl/support.compiled.json
git commit -m "feat: compile workflows against visible catalogs"
```

## Task 5: Implement the Stateless Responses Boundary

**Files:** Create `src/model/{mod.rs,types.rs,responses.rs}`,
`tests/support/{mod.rs,responses_server.rs}`, `tests/responses_conformance.rs`;
modify `Cargo.toml`, `Cargo.lock`, `src/lib.rs`.

**Interfaces:**

```rust
#[async_trait::async_trait]
pub trait ModelClient: Send + Sync {
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}
pub struct ModelRequest {
    pub instructions: String,
    pub input: Vec<serde_json::Value>,
    pub tools: Vec<serde_json::Value>,
}
```

- [ ] **Step 1: Add Reqwest/Tokio and write the failing exact-body test**

Assert `store: false`, exact `/responses` URL, full prior-item replay, and
unknown output-item preservation. Run the focused test and observe missing
client types.

- [ ] **Step 2: Implement configuration and request construction**

Reject userinfo/redirects/zero limits. Send only `model`, `instructions`,
`input`, `tools`, `tool_choice: "auto"`, and `store: false`. Redact every secret.

- [ ] **Step 3: Implement lossless response parsing and classification**

Preserve raw ordered items; recognize messages, refusals, reasoning, function
calls, and outputs. Reject duplicates, malformed calls, incomplete or oversized
responses, and classify 429/5xx/timeouts separately from terminal protocol
errors.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --test responses_conformance
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add Cargo.toml Cargo.lock src/model src/lib.rs tests/support tests/responses_conformance.rs
git commit -m "feat: add stateless responses client"
```

## Task 6: Persist Sessions, Catalog Epochs, Policy Rules, and Definitions

**Files:** Create `migrations/0001_session_authority.sql`,
`src/store/{mod.rs,models.rs,postgres.rs}`, `tests/session_store.rs`; modify
`Cargo.toml`, `Cargo.lock`, `src/lib.rs`.

**Interfaces:**

```rust
pub struct CreateSession {
    pub principal: PrincipalId,
    pub working_directory: String,
    pub retention_until: time::OffsetDateTime,
}
pub struct SessionView {
    pub id: SessionId,
    pub principal: PrincipalId,
    pub policy_revision: PolicyRevision,
    pub current_catalog_epoch: Option<CatalogEpochId>,
    pub mcp_attached: bool,
}
impl PostgresStore {
    pub async fn create_session(&self, request: CreateSession) -> Result<SessionView, StoreError>;
    pub async fn resume_session(&self, id: SessionId, principal: &PrincipalId) -> Result<SessionView, StoreError>;
    pub async fn append_catalog_epoch(&self, epoch: &ToolCatalogSnapshot) -> Result<(), StoreError>;
    pub async fn append_policy_rules(&self, update: PolicyUpdate) -> Result<PolicyRevision, StoreError>;
    pub async fn publish(&self, session: SessionId, workflow: &CompiledWorkflow, source: &str) -> Result<PublishedDefinition, StoreError>;
}
```

- [ ] **Step 1: Add SQLx and write failing transaction tests**

Test immutable principal binding, session lifecycle, catalog scoping, policy
revision/idempotency conflicts, idempotent publication, and cross-session
catalog denial.

- [ ] **Step 2: Create the four-table authority migration**

Create `sessions`, `mcp_catalog_epochs`, `session_policy_rules`, and
`workflow_definitions`. Enforce UUID primary keys, monotonic revisions,
session/server/digest uniqueness, immutable JSON snapshots, unique rule IDs and
idempotency keys per session, workflow name/version uniqueness, and definition
hash idempotency. Policy rows retain both typed source and deterministic
compiled Cedar artifact. Store only non-secret MCP binding identity.

- [ ] **Step 3: Implement store transactions and secret-absence proof**

Publication verifies every compiled binding exists at the exact descriptor
digest in the calling session's current catalog. Tests write sentinel MCP and
provider secrets to connection configuration, then assert no text/JSON column
contains either sentinel.

- [ ] **Step 4: Verify and commit**

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test session_store
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add Cargo.toml Cargo.lock migrations/0001_session_authority.sql src/store src/lib.rs tests/session_store.rs
git commit -m "feat: persist session authority and definitions"
```

## Task 7: Discover Principal-Scoped MCP Catalogs and Bind Calls

**Files:** Create `src/mcp/{mod.rs,config.rs,connection.rs,discovery.rs,bound_tool.rs}`,
`tests/mcp_catalog.rs`, `tests/fixtures/mcp/`; modify `Cargo.toml`, `Cargo.lock`,
`src/lib.rs`, `src/store/postgres.rs`.

**Interfaces:**

```rust
pub struct McpManager {
    store: std::sync::Arc<PostgresStore>,
    connections: tokio::sync::RwLock<SessionConnections>,
    limits: McpLimits,
}
pub struct SessionToolResolver {
    in_process: std::sync::Arc<InProcessToolRegistry>,
    mcp: std::sync::Arc<McpManager>,
}
pub struct McpLimits {
    pub max_servers: usize,
    pub max_pages_per_server: usize,
    pub max_tools: usize,
    pub max_descriptor_bytes: usize,
    pub max_catalog_bytes: usize,
}
pub enum McpServerConfig {
    Stdio { alias: String, program: String, args: Vec<String>, env: Vec<(String, Secret)> },
    StreamableHttp { alias: String, url: url::Url, headers: Vec<(String, Secret)> },
}
pub struct CatalogRefresh {
    pub epoch: ToolCatalogSnapshot,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}
impl McpManager {
    pub async fn attach(&self, session: SessionId, principal: PrincipalId, configs: Vec<McpServerConfig>) -> Result<ToolCatalogSnapshot, McpError>;
    pub async fn refresh(&self, session: SessionId, server_alias: &str) -> Result<CatalogRefresh, McpError>;
    pub fn bound_tool(&self, session: SessionId, qualified_name: &str, digest: &str) -> Result<std::sync::Arc<dyn BoundTool>, McpError>;
}
```

- [ ] **Step 1: Pin the official MCP SDK**

```toml
rmcp = { version = "=3.1.2", default-features = false, features = [
  "client", "reqwest", "transport-child-process",
  "transport-streamable-http-client-reqwest"
] }
```

- [ ] **Step 2: Write the failing two-principal discovery test**

Run two authenticated fake MCP peers whose paginated `tools/list` results
overlap only partly. Assert each session snapshot, diagnostics, and events omit
the other principal's hidden descriptors. Run `cargo test --test mcp_catalog`;
expect missing manager types.

- [ ] **Step 3: Implement authenticated attachment and full pagination**

Use `rmcp::Peer::list_tools` until `nextCursor` is absent. Enforce server/page,
tool-count, descriptor-byte, and total-catalog limits before persisting one
canonical epoch. Qualify names and apply only deployment-owned capability and
recovery mappings; unmapped tools receive `mcp_unknown`.

- [ ] **Step 4: Implement refresh and list-change handling**

Subscribe only when the server advertises list-changed capability. Re-list on
notification with the same authenticated peer. Additions affect later
publication/start, removals revoke future calls, and descriptor changes return
their names without mutating run bindings.

- [ ] **Step 5: Implement `BoundTool` over `Peer::call_tool`**

Require the session, qualified name, and descriptor digest to match the live
connection. Validate input/output and apply trusted recovery metadata. Never
forward one MCP server's token to another server. Implement
`SessionToolResolver` so the engine resolves trusted in-process and MCP tools
through one digest-checked interface; only the MCP branch depends on a live
connection.

- [ ] **Step 6: Verify and commit**

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test mcp_catalog
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add Cargo.toml Cargo.lock src/mcp src/store/postgres.rs src/lib.rs tests/mcp_catalog.rs tests/fixtures/mcp
git commit -m "feat: discover principal scoped MCP tools"
```

## Task 8: Compile Typed Session Policy to Cedar

**Files:** Create `src/policy/{mod.rs,types.rs,compile.rs,authorize.rs}`,
`tests/cedar_policy.rs`, `tests/fixtures/policy/`; modify `Cargo.toml`,
`Cargo.lock`, `src/lib.rs`, `src/store/postgres.rs`.

**Interfaces:**

```rust
pub struct SessionPolicy { pub version: u32, pub rules: Vec<PolicyRule> }
pub struct PolicyRule {
    pub rule_id: String,
    pub effect: PolicyEffect,
    pub target: PolicyTarget,
    pub input: std::collections::BTreeMap<String, OneOfPrimitive>,
}
pub enum PolicyTarget {
    Capability { names: std::collections::BTreeSet<CapabilityName> },
    Tool { names: std::collections::BTreeSet<String> },
}
pub enum Primitive { String(String), Integer(i64), Number(serde_json::Number), Boolean(bool), Null }
pub struct OneOfPrimitive(pub Vec<Primitive>);
pub enum PolicyDecision { Allow, Forbid { rule_ids: Vec<String> }, NeedsPermission }
pub trait SessionAuthorizer: Send + Sync {
    fn authorize(&self, request: &PolicyRequest) -> Result<PolicyDecision, PolicyError>;
}
```

- [ ] **Step 1: Pin published Cedar crates**

```toml
cedar-policy = "=4.11.0"
cedar-policy-mcp-schema-generator = { version = "=0.6.0", features = ["serde", "serde_json"] }
```

Do not add the unpublished `cedar-policy-agent-builder` crate.

- [ ] **Step 2: Write failing typed-policy validation tests**

Cover version 1, unique `ruleId`, non-empty tagged targets, qualified visible
tool names, trusted capabilities, RFC 6901 input pointers, primitive non-empty
`oneOf`, unsupported paths, and values outside tool schemas.

- [ ] **Step 3: Generate schema and compile deterministic Cedar artifacts**

Generate actions from the exact current MCP descriptors. Use session principal,
qualified-name-plus-digest resource, exact tool action beneath trusted
capability parent actions, and validated invocation context. Exact-tool rules
capture descriptor digests; removed tools become inactive. Rebuild active
artifacts after catalog refresh while retaining immutable typed rules.

- [ ] **Step 4: Implement default deny and forbid precedence**

Return `Allow` only on Cedar allow, `Forbid` when any matching forbid applies,
and `NeedsPermission` on implicit/default deny. Prove Cedar cannot add a tool
outside the workflow binding or bypass schema/effect checks.

- [ ] **Step 5: Persist revision-checked updates and verify**

An update takes session, expected revision, client idempotency key, and complete
typed rules. Same key/body replays; changed body conflicts; accepted rules
append and increment exactly once.

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test cedar_policy
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add Cargo.toml Cargo.lock src/policy src/store/postgres.rs src/lib.rs tests/cedar_policy.rs tests/fixtures/policy
git commit -m "feat: enforce typed Cedar session policy"
```

## Task 9: Persist Runs, Frames, Calls, Permissions, and Events

**Files:** Create `migrations/0002_workflow_engine.sql`,
`tests/execution_store.rs`; modify `src/store/{models.rs,postgres.rs}`.

**Interfaces:**

```rust
pub struct StartRun {
    pub session_id: SessionId,
    pub workflow_name: WorkflowName,
    pub version: Option<i64>,
    pub input: serde_json::Value,
    pub idempotency_key: IdempotencyKey,
}
pub enum FrameCommand {
    StartCall(NewChild), StartParallel(Vec<NewChild>), SuspendAsk(Question),
    SuspendPermission(NewPermissionRequest), Goto(TaskName, Bindings),
    Complete(serde_json::Value), Fail(Failure),
}
pub type Bindings = std::collections::BTreeMap<BindingName, serde_json::Value>;
pub struct NewChild { pub target: TaskName, pub binding: BindingName, pub arguments: Bindings, pub branch_index: i32 }
pub struct Question { pub id: SuspensionId, pub binding: BindingName, pub text: String, pub schema: ClosedSchema }
pub struct NewPermissionRequest {
    pub id: PermissionRequestId,
    pub provider_call_id: String,
    pub qualified_tool: String,
    pub descriptor_digest: String,
    pub input_digest: String,
    pub policy_revision: PolicyRevision,
}
```

- [ ] **Step 1: Write failing execution transaction tests**

Test idempotent start, immutable run/session binding, exact run tool bindings,
model intent-before-result, ordered model items, tool call idempotency, human
questions, permission requests, event sequences, and stale revisions.

- [ ] **Step 2: Create the remaining seven tables**

Create `runs`, `run_tool_bindings`, `frames`, `model_items`, `tool_calls`,
`permission_requests`, and `events`. Include `waiting_for_permission` in run and
frame checks. Enforce one start request per scoped key, immutable binding digest,
one provider call per frame, one resolution per question/permission, monotonic
revisions, and `(run_id, sequence)` event order.

- [ ] **Step 3: Implement definition selection and immutable run binding**

In one transaction select explicit/latest definition, validate input, verify
every exact descriptor in the session's current catalog, insert run bindings,
root frame, initial item, and `run.started`. Reusing a key with different
normalized content conflicts.

- [ ] **Step 4: Implement external-call and suspension transactions**

`begin_model_attempt`, `commit_model_attempt`, `begin_tool_call`,
`finish_tool_call`, `apply_frame_command`, `create_permission_request`,
`resolve_permission`, `answer`, and `cancel_run` all lock expected revisions and
append metadata-only events in the same transaction.

- [ ] **Step 5: Verify and commit**

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test execution_store
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add migrations/0002_workflow_engine.sql src/store tests/execution_store.rs
git commit -m "feat: persist durable workflow execution"
```

## Task 10: Interpret the Six Control Operations Purely

**Files:** Create `src/engine/{mod.rs,transition.rs}`,
`tests/transition_engine.rs`; modify `src/lib.rs`.

**Interfaces:**

```rust
pub fn next_command(
    workflow: &CompiledWorkflow,
    frame: &FrameSnapshot,
) -> Result<FrameCommand, Failure>;
```

- [ ] **Step 1: Write failing table-driven tests for every operation**

Assert call continuation, source-ordered parallel children, typed ask, tail
goto, root/child done, stable fail, optional-null normalization, and corrupt-IR
internal failure.

- [ ] **Step 2: Implement one-command evaluation with no I/O**

Resolve only literals and existing bindings. Revalidate every argument/result
against compiled schemas. Embed the next cursor only for nonterminal statements.

- [ ] **Step 3: Verify and commit**

```bash
cargo test --test transition_engine
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add src/engine src/lib.rs tests/transition_engine.rs
git commit -m "feat: interpret workflow transitions"
```

## Task 11: Execute Sequential Turns Through the Authorization Boundary

**Files:** Create `src/engine/{turn.rs,scheduler.rs}`,
`tests/support/{fake_model.rs,fake_tools.rs}`, `tests/sequential_runtime.rs`;
modify `src/engine/mod.rs`.

**Interfaces:**

```rust
pub struct Engine {
    pub store: std::sync::Arc<PostgresStore>,
    pub model: std::sync::Arc<dyn ModelClient>,
    pub tools: std::sync::Arc<dyn ToolResolver>,
    pub authorizer: std::sync::Arc<dyn SessionAuthorizer>,
    pub config: EngineConfig,
}
impl Engine {
    pub async fn tick_until_idle(&self) -> Result<(), EngineError>;
    pub async fn run(self: std::sync::Arc<Self>, shutdown: Shutdown) -> Result<(), EngineError>;
}
```

- [ ] **Step 1: Write the failing permitted-call workflow test**

Script a model tool call then `complete_task`; give the session exact catalog
and permit rule; assert intent, MCP invocation, validated output, second model
turn, and typed root result.

- [ ] **Step 2: Implement one-process scheduling and model decisions**

Use one scheduler, bounded semaphore, and `JoinSet`. Build each task transcript
from only its prompt/arguments/history. Present catalog/workflow intersection
plus `complete_task`. Reject mixed completion/tool calls, undeclared names,
malformed arguments, refusals, and invalid outcomes within turn limits.

- [ ] **Step 3: Evaluate Cedar immediately before every tool call**

For `Allow`, resolve `BoundTool` with session/name/digest and invoke. For
`Forbid`, append
`{"error":{"code":"policy_denied","message":"Tool invocation denied by session policy"}}`
as the stable function output without prompting. For
`NeedsPermission`, atomically create a durable request and set only that frame
to `waiting_for_permission`; do not call MCP.

- [ ] **Step 4: Prove isolation and fail-closed behavior**

Test child transcript isolation, goto reset, hidden/undeclared tools never
shown, changed descriptor producing `tool_contract_changed`, MCP server denial
after Cedar permit, and invalid output producing `tool_failed`.

- [ ] **Step 5: Verify and commit**

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test sequential_runtime
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add src/engine tests/support tests/sequential_runtime.rs
git commit -m "feat: execute authorized sequential turns"
```

## Task 12: Resolve Permissions, Parallel Work, Questions, and Cancellation

**Files:** Modify `src/engine/{mod.rs,turn.rs,scheduler.rs}`,
`src/store/postgres.rs`; create `tests/structured_runtime.rs`.

**Interfaces:**

```rust
pub enum PermissionChoice {
    AllowOnce, AllowToolForSession, DenyOnce, DenyToolForSession,
}
pub struct ResolvePermission {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub permission_request_id: PermissionRequestId,
    pub choice: PermissionChoice,
    pub idempotency_key: IdempotencyKey,
}
pub struct AnswerQuestion {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub suspension_id: SuspensionId,
    pub answer: serde_json::Value,
    pub idempotency_key: IdempotencyKey,
}
pub struct CancelRun {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub reason: Option<String>,
    pub idempotency_key: IdempotencyKey,
}
impl Engine {
    pub async fn resolve_permission(&self, request: ResolvePermission) -> Result<RunView, EngineError>;
    pub async fn answer(&self, request: AnswerQuestion) -> Result<RunView, EngineError>;
    pub async fn cancel(&self, request: CancelRun) -> Result<RunView, EngineError>;
}
```

- [ ] **Step 1: Write failing permission-choice tests**

Prove `allow_once` binds one durable call identity;
`allow_tool_for_session` appends a name+digest permit; `deny_once` resolves only
the pending call; `deny_tool_for_session` appends a digest-bound forbid. Replays
with same key/body succeed and conflicts fail.

- [ ] **Step 2: Implement transactional decision-to-policy updates**

Append immutable rule/decision metadata, increment policy revision exactly once
for session-wide choices, resolve request, and wake the frame. Never reinterpret
an already-invoked effect. Capability/input-wide changes remain exclusive to
the explicit policy-update command.

- [ ] **Step 3: Drive bounded children and deterministic joins**

Children use the global semaphore. Join only after all children are terminal;
bind results by source branch index. On first failure cancel unstarted siblings,
let in-flight calls settle, and never detach or compensate effects.

- [ ] **Step 4: Implement independent questions and cooperative cancel**

Multiple parallel questions/permissions remain independently visible. Answers
validate against stored schemas. Cancellation prevents new work, records late
external results without advancing, and is idempotent.

- [ ] **Step 5: Verify and commit**

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test structured_runtime
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add src/engine src/store/postgres.rs tests/structured_runtime.rs
git commit -m "feat: resolve permissions and structured suspension"
```

## Task 13: Recover Calls, Reconnect Catalogs, and Enforce Limits

**Files:** Create `src/engine/recovery.rs`, `tests/recovery.rs`; modify
`src/engine/{mod.rs,scheduler.rs,turn.rs}`, `src/store/{models.rs,postgres.rs}`,
`src/mcp/mod.rs`.

**Interfaces:**

```rust
pub struct RecoveryReport {
    pub model_attempts_requeued: u64,
    pub tools_recovered: u64,
    pub tools_requeued: u64,
    pub sessions_waiting_for_mcp: u64,
    pub outcomes_unknown: u64,
}
```

- [ ] **Step 1: Write interrupted model/tool recovery tests**

Cover requeued unstored model output, read-only retry, idempotent retry, status
Succeeded/Failed/NotStarted/Running/Unknown, and nonrecoverable effect.

- [ ] **Step 2: Implement status-first recovery**

Load durable intent, call status outside transactions, and apply results with
revision checks. Use the same idempotency key. Unknown after retry budget moves
tool/frame/run to `outcome_unknown`; never guess or blindly repeat.

- [ ] **Step 3: Block MCP-dependent work until fresh resume attachment**

After process restart retain no MCP secrets. Mark affected sessions unavailable,
accept fresh config only from authenticated session load/resume, relist catalogs,
and verify every run binding. Missing/changed tools fail attempted future calls
with `tool_contract_changed` without making unrelated sessions unready.

- [ ] **Step 4: Enforce all deployment ceilings**

Test source/compiled size, sessions/runs, MCP server/pages/tools/bytes, policy
rules/bytes/evaluation time, open permissions, workflow steps, model turns,
tool calls, response items, branches/concurrency, external timeouts, and active
execution time. Human/permission waiting consumes no active time.

- [ ] **Step 5: Verify and commit**

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test recovery
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add src/engine src/store src/mcp/mod.rs tests/recovery.rs
git commit -m "feat: recover durable work and catalog bindings"
```

## Task 14: Add the Version-Neutral ACP Application Surface

**Files:** Create `src/protocol/{mod.rs,application.rs,types.rs,v1.rs,v2.rs}`,
`tests/acp_conformance.rs`, `tests/fixtures/acp/`; modify `Cargo.toml`,
`Cargo.lock`, `src/lib.rs`.

**Interfaces:**

```rust
pub enum ApplicationCommand {
    Publish(PublishWorkflow), Start(StartRun), Get(GetRun),
    Answer(AnswerQuestion), Cancel(CancelRun),
    UpdatePolicy(UpdateSessionPolicy), ReplayEvents(EventCursor),
}
pub struct PublishWorkflow { pub source: String }
pub struct GetRun { pub run_id: RunId }
pub struct EventCursor { pub run_id: RunId, pub after: i64, pub limit: u32 }
pub struct UpdateSessionPolicy {
    pub expected_revision: PolicyRevision,
    pub idempotency_key: IdempotencyKey,
    pub policy: SessionPolicy,
}
pub struct ApplicationContext {
    pub connection_principal: PrincipalId,
    pub session_id: Option<SessionId>,
}
pub enum ApplicationResult {
    Published(PublishedDefinition),
    Run(RunView),
    PolicyUpdated { revision: PolicyRevision },
    Events(Vec<RunEvent>),
    Session(SessionView),
    Sessions(Vec<SessionView>),
    Closed,
}
pub trait ProtocolAdapter: Send + Sync {
    fn decode_extension(&self, method: &str, params: serde_json::Value) -> Result<ApplicationCommand, ProtocolError>;
    fn encode_result(&self, result: ApplicationResult) -> Result<serde_json::Value, ProtocolError>;
}
```

- [ ] **Step 1: Add exact ACP dependencies and feature gate**

```toml
agent-client-protocol = "=2.0.0"
agent-client-protocol-http = { version = "=2.0.0", features = ["client", "server"] }

[features]
default = []
acp-v2 = ["agent-client-protocol/unstable_protocol_v2"]
```

- [ ] **Step 2: Write failing versioned golden-message tests**

Fixtures cover initialize negotiation, v1 `session/new` and `session/load`, v2
`session/new`, `session/list`, `session/resume`, and `session/close`, v1
tool-call permissions, v2 generic permissions,
unknown `_meta` preservation, and every `_kiteframe/*` method. Steering methods
must be rejected as unknown.

- [ ] **Step 3: Implement one application dispatcher**

Map `_kiteframe/workflow/publish`, run start/get/answer/cancel,
session policy update, and durable run events to the store/engine. Enforce
session ownership, idempotency, expected policy revision, and cursor replay.
The dispatcher receives `ApplicationContext` from the authenticated connection;
no extension request may supply or override `connection_principal`.

- [ ] **Step 4: Implement separate v1 and gated-v2 adapters**

Keep wire DTOs isolated; map both into identical commands. V1 known IDs load
durable sessions and rely on retention because v1 lacks list/close. V2 is
advertised only when compiled and runtime-enabled. One connection negotiates
one version.

- [ ] **Step 5: Verify both builds and commit**

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test acp_conformance
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --features acp-v2 --test acp_conformance
cargo clippy --all-targets --all-features -- -D warnings
git add Cargo.toml Cargo.lock src/protocol src/lib.rs tests/acp_conformance.rs tests/fixtures/acp
git commit -m "feat: expose workflow commands through ACP"
```

## Task 15: Serve ACP over stdio, WebSocket, and Streamable HTTP

**Files:** Create `src/config.rs`, `src/protocol/transport.rs`,
`tests/transports.rs`; replace `src/main.rs`; modify `Cargo.toml`, `Cargo.lock`.

**Interfaces:**

```rust
pub struct AuthenticatedConnection { pub principal: PrincipalId, pub transport: TransportKind }
pub trait ConnectionAuthenticator: Send + Sync {
    fn authenticate(&self, headers: &http::HeaderMap) -> Result<PrincipalId, AuthError>;
}
pub enum HarnessMode { Stdio, Server { bind: std::net::SocketAddr } }
```

- [ ] **Step 1: Write failing transport-equivalence tests**

Send the same initialize/session/publish/start/get sequence through NDJSON
stdio, WebSocket `/acp`, and Streamable HTTP `/acp`; compare normalized
application results and durable event cursors.

- [ ] **Step 2: Implement service configuration and authentication**

Require database/model configuration. Local stdio uses the configured local
principal inside the child-process trust boundary. Remote mode requires TLS at
the deployment boundary and a configured set of bearer-secret-to-principal
bindings; compare secrets in constant time and never accept a principal field
from request JSON.

- [ ] **Step 3: Serve all transports through one dispatcher**

Use the ACP core stdio stream and ACP HTTP server support. `/acp` accepts
WebSocket upgrades and Streamable HTTP; only `/healthz` and `/readyz` exist
outside ACP. There is no REST workflow API or product SSE route.

- [ ] **Step 4: Compose startup, recovery, readiness, and shutdown**

Validate config, connect/migrate PostgreSQL, take a single-process advisory
lock, recover durable work, serve, and schedule. Global readiness covers
database/model/service health; per-session views report MCP attachment status.
Graceful shutdown stops new work and leaves unresolved intents recoverable.

- [ ] **Step 5: Test auth/fallback classifications, redaction, and commit**

The server returns distinguishable upgrade incompatibility versus 401/403,
malformed JSON-RPC, and application errors so clients cannot misclassify
fallback. Captured logs contain no configured sentinel secret/content.

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test transports
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git add Cargo.toml Cargo.lock src/config.rs src/main.rs src/protocol/transport.rs tests/transports.rs
git commit -m "feat: serve ACP across local and remote transports"
```

## Task 16: Ship the Python Thin Client and Bundled Harness Wheel

**Files:** Create `clients/python/pyproject.toml`,
`clients/python/src/kiteframe/{__init__.py,client.py,models.py,transport.py,binary.py,py.typed}`,
`clients/python/tests/`; modify `.github/workflows/ci.yml`.

**Interfaces:**

```python
class Kiteframe:
    @classmethod
    async def local(cls, *, config: HarnessConfig, policy: SessionPolicy) -> "Kiteframe": ...
    @classmethod
    async def connect(cls, url: str, *, token: str, policy: SessionPolicy) -> "Kiteframe": ...
    async def publish(self, source: str) -> PublishedWorkflow: ...
    async def start(self, name: str, input: object, *, idempotency_key: str) -> Run: ...

@dataclass(frozen=True)
class PolicyRule:
    rule_id: str
    effect: Literal["permit", "forbid"]
    target: CapabilityTarget | ToolTarget
    input: Mapping[str, OneOfPrimitive] = field(default_factory=dict)

@dataclass(frozen=True)
class SessionPolicy:
    version: Literal[1]
    rules: tuple[PolicyRule, ...]
```

- [ ] **Step 1: Configure a mixed Python/bin wheel**

```toml
[build-system]
requires = ["maturin>=1.9,<2"]
build-backend = "maturin"

[project]
name = "kiteframe"
requires-python = ">=3.11"
dependencies = ["httpx>=0.28,<0.29", "websockets>=16,<17"]

[dependency-groups]
dev = ["maturin>=1.9,<2", "pytest>=8,<9", "pytest-asyncio>=0.25,<1", "ruff>=0.12,<1", "pyright>=1.1,<2"]

[tool.maturin]
manifest-path = "../../Cargo.toml"
bindings = "bin"
python-source = "src"
```

The wheel exposes `kiteframe-harness` as its bundled script. `binary.py`
resolves that exact executable and validates its reported client/harness
version before initialization.

- [ ] **Step 2: Write failing local/remote typed-client tests**

Test local process lifecycle, NDJSON, version negotiation, typed policy JSON,
permission callback choices, event cursor resume, clean shutdown, and startup
diagnostics. For remote mode test WebSocket-first and valid-only HTTP fallback.

- [ ] **Step 3: Implement transports and typed façade**

Use asyncio subprocess for stdio, `websockets.asyncio.client.connect` for
WebSocket, and `httpx.AsyncClient.stream` for Streamable HTTP. Fallback only on
network/upgrade incompatibility; propagate 401/403 and JSON-RPC/application
errors. Offer v2 first only when enabled, then negotiate v1—not by reconnecting
after an application error.

- [ ] **Step 4: Build and inspect supported wheels**

Support macOS arm64/x86_64, Linux glibc arm64/x86_64, and Windows x86_64. CI
builds a wheel per target, installs it into an empty uv project, runs
`uv add <wheel>`, imports `kiteframe`, and launches the embedded executable.

- [ ] **Step 5: Verify and commit**

```bash
uv run --project clients/python pytest -q
uv run --project clients/python ruff check src tests
uv run --project clients/python pyright
uv run --project clients/python maturin build
git add clients/python .github/workflows/ci.yml
git commit -m "feat: ship Python ACP client with harness"
```

## Task 17: Ship the TypeScript Thin Client and Platform Packages

**Files:** Create `clients/typescript/package.json`, `clients/typescript/tsconfig.json`,
`clients/typescript/src/{index.ts,client.ts,models.ts,transport.ts,binary.ts}`,
`clients/typescript/test/`, and platform packages under
`clients/typescript/npm/`; modify `.github/workflows/ci.yml`.

**Interfaces:**

```ts
export class Kiteframe {
  static local(options: LocalOptions): Promise<Kiteframe>;
  static connect(url: URL, options: RemoteOptions): Promise<Kiteframe>;
  publish(source: string): Promise<PublishedWorkflow>;
  start(name: string, input: unknown, options: { idempotencyKey: string }): Promise<Run>;
}

export type PolicyRule = {
  ruleId: string;
  effect: "permit" | "forbid";
  target: { type: "capability" | "tool"; names: readonly string[] };
  input?: Readonly<Record<string, { oneOf: readonly (string | number | boolean | null)[] }>>;
};
export type SessionPolicy = { version: 1; rules: readonly PolicyRule[] };
export type LocalOptions = { config: HarnessConfig; policy: SessionPolicy };
export type RemoteOptions = { token: string; policy: SessionPolicy };
```

- [ ] **Step 1: Define root exports and platform package resolution**

```json
{
  "name": "kiteframe",
  "version": "0.1.0",
  "type": "module",
  "exports": {".": "./dist/index.js", "./client": "./dist/client.js"},
  "scripts": {"build": "bun build src/index.ts src/client.ts --outdir dist", "typecheck": "tsc --noEmit"},
  "devDependencies": {"typescript": "^5.9.3"},
  "optionalDependencies": {
    "@kiteframe/darwin-arm64": "0.1.0",
    "@kiteframe/darwin-x64": "0.1.0",
    "@kiteframe/linux-arm64": "0.1.0",
    "@kiteframe/linux-x64": "0.1.0",
    "@kiteframe/win32-x64": "0.1.0"
  }
}
```

Each platform package contains exactly one executable plus package metadata;
`binary.ts` selects only the exact `process.platform`/`process.arch` package and
verifies the harness version.

- [ ] **Step 2: Write failing Bun client and packaging tests**

Test `import { Kiteframe } from "kiteframe/client"`, `Bun.spawn` local stdio,
WebSocket-first remote, Streamable HTTP fallback classification, typed policy
and permission mapping, event cursor resume, missing-platform diagnostics, and
SIGTERM cleanup.

- [ ] **Step 3: Implement the typed client without runtime semantics**

Use Bun's WebSocket, `fetch`, and subprocess APIs. Share golden ACP JSON fixtures
from Task 14 in tests. Keep workflow source/input/output opaque except for typed
client DTO validation; no scheduling or Cedar code enters this package.
Offer runtime-enabled ACP v2 during initialization and accept negotiated v1.
Do not reconnect or downgrade after authentication, policy, protocol, or
application errors.

- [ ] **Step 4: Build, install, inspect, and commit**

```bash
bun install --cwd clients/typescript --frozen-lockfile
bun test --cwd clients/typescript
bun run --cwd clients/typescript typecheck
bun run --cwd clients/typescript build
KITEFRAME_BUN_SMOKE_DIR="$(mktemp -d)"
bun init -y --cwd "$KITEFRAME_BUN_SMOKE_DIR"
bun add --cwd "$KITEFRAME_BUN_SMOKE_DIR" "$(pwd)/clients/typescript"
git add clients/typescript .github/workflows/ci.yml
git commit -m "feat: ship TypeScript ACP client with harness"
```

The smoke install must resolve one platform package and launch its binary; it
must not use `bun install kiteframe/client` because `kiteframe/client` is an
export path, not a package name.

## Task 18: Prove Cross-Language Restart Safety and Finish the Replacement

**Files:** Create `tests/e2e_restart.rs`; modify
`tests/support/{responses_server.rs,fake_tools.rs}`, `.github/workflows/ci.yml`,
`README.md`, the current design status, and supersession banners in older
`docs/superpowers/specs/` and `docs/superpowers/plans/` files.

**Interfaces:** Produces the acceptance evidence for the complete Rust harness,
both clients, both ACP versions, all transports, principal isolation, Cedar,
MCP refresh, and restart recovery.

- [ ] **Step 1: Write the failing process-restart acceptance harness**

The parent starts real PostgreSQL, scripted Responses and two authenticated MCP
servers, and a real `kiteframe-harness` child. It kills/restarts at persisted
model intent, committed effect before result, parallel join, question,
permission, and catalog-refresh boundaries. The MCP effect table proves invoke
count and status recovery.

- [ ] **Step 2: Exercise both principals and session policy**

Principal A and B receive different paginated catalogs. Assert no hidden name,
description, schema, diagnostic, or event leakage. Publish using A's exact
descriptor; default deny creates permission; allow-for-session resumes; forbid
returns denial without prompting; descriptor mutation later yields
`tool_contract_changed`.

- [ ] **Step 3: Exercise both clients, versions, and transports**

Install the built wheel and npm packages into empty projects. Python completes
the workflow locally over stdio. TypeScript completes remotely over WebSocket
and repeats with a server configuration that validly forces Streamable HTTP.
Run ACP v1 and runtime-gated v2 variants and compare normalized final result and
event ordering.

- [ ] **Step 4: Prove workflow and effect semantics**

The scenario covers ordinary tools, isolated child transcripts, parallel
children finishing out of order but joining in source order, durable question,
idempotent answers/permission decisions, process restarts, typed final output,
and cursor replay. A second scenario returns unknown effect status and proves
`outcome_unknown` with no unsafe retry.

- [ ] **Step 5: Finish CI and documentation**

CI provisions PostgreSQL 16 and runs Rust default/v2 builds, all SQLx tests,
Python tests/type checks/wheel smoke, Bun tests/type checks/package smoke, and
the restart suite. README documents `uv add kiteframe`, `bun add kiteframe`,
`kiteframe/client`, local/remote setup, PostgreSQL/model/MCP configuration,
typed policy, permission choices, transports, and verified limitations. Add a
superseded banner to older specs/plans without rewriting historical bodies.

- [ ] **Step 6: Run the complete verification matrix**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --features acp-v2 --all-targets
uv run --project clients/python pytest -q
uv run --project clients/python ruff check src tests
uv run --project clients/python pyright
bun test --cwd clients/typescript
bun run --cwd clients/typescript typecheck
git diff --check
```

Expected: every gate passes; no secret sentinel appears; `site/` remains
untracked and unstaged.

- [ ] **Step 7: Audit scope, update status, and commit**

Search runtime/client code for `steer`, OpenFGA, PyO3, Deep Agents, LangGraph,
old REST workflow routes, and product SSE routes; expect no matches. Confirm all
eleven tables, every `_kiteframe/*` method, three transports, two client install
smokes, and all acceptance states are present. Only then change the design
status to `Implemented and verified`.

```bash
git add .github/workflows/ci.yml README.md tests/e2e_restart.rs tests/support clients docs/superpowers
git commit -m "test: prove kiteframe harness v1 end to end"
```

## Spec Coverage Map

| Approved requirement | Tasks |
|---|---|
| One Rust harness; legacy runtime removed | 1, 18 |
| Closed schemas, DSL, immutable descriptor-bound workflows | 2-4 |
| Stateless Responses protocol | 5, 11, 18 |
| Eleven-table PostgreSQL authority | 6, 9, 18 |
| Principal-scoped paginated MCP discovery and refresh | 7, 13, 18 |
| Typed Cedar session policy and digest-bound grants | 8, 11, 12, 18 |
| Durable permissions and four choices | 9, 11, 12, 14, 18 |
| Sequential, structured parallel, questions, cancellation | 10-12, 18 |
| Status-first recovery, limits, `outcome_unknown` | 13, 18 |
| ACP v1 plus gated draft v2 | 14, 18 |
| stdio, WebSocket, Streamable HTTP | 15-18 |
| Python `uv add kiteframe` bundled client | 16, 18 |
| TypeScript `bun add kiteframe`, `kiteframe/client` | 17, 18 |
| Steering excluded as separate feature | Global constraints, 14, 18 |

## Plan Completion Criteria

Implementation is complete only when every checkbox and ordered commit exists,
the Task 18 matrix passes from a fresh PostgreSQL database, client artifacts
launch their bundled version-matched harness, two-principal tests prove catalog
non-disclosure, all external-call crash boundaries are covered, documentation
contains no unverified behavior claim, and unrelated `site/` remains untouched.
