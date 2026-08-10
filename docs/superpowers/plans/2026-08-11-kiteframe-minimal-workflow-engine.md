# Kiteframe Minimal Workflow Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current package/adapter architecture with one Rust service
that compiles author-authored workflows, runs durable model/tool turns through a
portable Responses protocol, and persists structured parallelism and human
suspension in PostgreSQL.

**Architecture:** A single Rust package exposes focused `dsl`, `engine`,
`store`, `model`, `tools`, and `api` modules and builds one service binary.
PostgreSQL is the sole workflow authority; model calls use stateless
`store: false` Responses requests; every effect is intent-recorded and recovered
through idempotency or status. Tasks below are cumulative and each ends at an
independently testable commit boundary.

**Tech Stack:** Rust 1.97.1 / edition 2024, Tokio 1, Axum 0.8, SQLx 0.8 with
PostgreSQL, Reqwest 0.12 with rustls, Serde/serde_json 1, UUID 1, `time` 0.3,
SHA-256 plus RFC 8785 canonical JSON, `async-trait` 0.1, `thiserror` 2,
`tracing`/`tracing-subscriber`, Tower HTTP 0.6, and PostgreSQL 16 in CI.

**Design:**
`docs/superpowers/specs/2026-08-11-kiteframe-minimal-workflow-engine-design.md`

## Global Constraints

- The approved design is the normative contract. Older Kiteframe specs and
  plans are historical evidence only.
- V1 is one Rust package and one service process. Do not add workspace members,
  Python, PyO3, Deep Agents, LangGraph, LangChain, OpenFGA, MCP, queues,
  distributed leases, or a second runtime.
- PostgreSQL is the sole writable workflow authority. Do not add SQLite, S3
  workflow state, dual writes, or provider-side conversation state.
- Every Responses request sets `store: false`; PostgreSQL retains and replays
  every ordered input, output, reasoning, function-call, and function-output
  item.
- The provider profile is non-streaming `POST {base_url}/responses` with
  `model`, `instructions`, `input`, `tools`, `tool_choice`, and `store`.
- Strict tool schemas set `additionalProperties: false`, list every property in
  `required`, and encode `T?` as `[T, "null"]`.
- Authors own the graph. The model can call only current-task tools and the
  generated `complete_task`; it cannot create tasks, transitions, or detached
  work.
- The only control operations are `call`, `goto`, `parallel`, `ask`, `done`,
  and `fail`. `goto` cycles are bounded; direct and indirect recursive `call`
  chains are compile errors.
- Parallel children are structured: create them atomically, join all terminal
  children, assemble results in source order, cancel unstarted siblings after
  failure, and allow already-running external calls to settle.
- External effects are never claimed exactly once. Record intent before invoke;
  retry read-only or idempotent calls; use status recovery where declared; move
  uncertain effects to `outcome_unknown`.
- Logs and events contain metadata only by default. Never log prompts, model
  content, reasoning, tool arguments/results, human answers, or credentials.
- Preserve unrelated `site/` work. Every task stages only its listed paths.
- Use TDD for behavior: observe the named failing test, add the smallest
  implementation that satisfies it, then run the task's focused and cumulative
  gates before committing.
- Current API guidance used by this plan: [Axum state and serving](https://github.com/tokio-rs/axum/blob/main/axum/src/docs/routing/with_state.md),
  [Axum SSE](https://docs.rs/axum/0.8/axum/response/sse/index.html),
  [SQLx migrations](https://docs.rs/sqlx/0.8/sqlx/macro.migrate.html),
  [SQLx tests](https://docs.rs/sqlx/0.8/sqlx/attr.test.html), and
  [OpenAI strict function schemas](https://developers.openai.com/api/docs/guides/function-calling).

---

## Final File Structure

```text
Cargo.toml
Cargo.lock
README.md
rust-toolchain.toml
.github/workflows/ci.yml
migrations/
  0001_workflow_engine.sql
src/
  lib.rs                       # Public composition boundary
  main.rs                      # Configuration, migrations, recovery, serve
  config.rs                    # Environment-owned service configuration
  domain.rs                    # IDs, statuses, limits, failures, run views
  hash.rs                      # RFC 8785 and SHA-256 helpers
  schema.rs                    # Closed portable schema and value validation
  dsl/
    mod.rs
    diagnostic.rs              # Source spans and compiler diagnostics
    lexer.rs                    # Bounded tokenization
    ast.rs                      # Parsed author syntax
    parser.rs                   # Hand-written recursive-descent parser
    ir.rs                       # Immutable compiled workflow
    compiler.rs                 # Resolution, type/graph checks, hashes
  tools/
    mod.rs
    descriptor.rs              # Tool contract and recovery declaration
    registry.rs                # Trusted startup registry
  model/
    mod.rs
    types.rs                   # ModelClient and Responses item contracts
    responses.rs               # Reqwest Responses implementation
  store/
    mod.rs
    models.rs                  # SQL row and transaction command types
    postgres.rs                # PostgresStore and all state transitions
  engine/
    mod.rs
    transition.rs              # Pure interpretation of compiled statements
    turn.rs                    # Model/ordinary-tool loop
    scheduler.rs               # One-process bounded executor
    recovery.rs                # Startup interrupted-call recovery
  api/
    mod.rs
    types.rs                   # HTTP request/response/error contracts
    sse.rs                     # Durable cursor event stream
examples/
  support.kite
tests/
  repository_shape.rs
  schema_and_tools.rs
  dsl_parser.rs
  dsl_compiler.rs
  responses_conformance.rs
  store_definitions.rs
  store_execution.rs
  transition_engine.rs
  sequential_runtime.rs
  structured_runtime.rs
  recovery.rs
  api.rs
  e2e_restart.rs
  fixtures/
    dsl/support.kite
    dsl/support.compiled.json
  support/
    mod.rs
    fake_model.rs
    fake_tools.rs
    responses_server.rs
```

## Task 1: Collapse the Repository to One Rust Package

**Files:**

- Replace: `Cargo.toml`
- Regenerate: `Cargo.lock`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `tests/repository_shape.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Delete: `crates/`
- Delete: `python/`
- Delete: `fuzz/`
- Delete: `openfga/`
- Delete: `runtime-targets/`
- Delete: `schemas/`
- Delete: `scripts/`
- Delete: current contents of `tests/` before creating the listed replacement
  test

**Interfaces:**

- Produces one package named `kiteframe`, with a library target and the
  `kiteframe` binary.
- Preserves `docs/`, `LICENSE`, `rust-toolchain.toml`, and unrelated `site/`.
- Later tasks add dependencies and public modules through `src/lib.rs`.

- [ ] **Step 1: Replace the workspace manifest with the minimal package shell**

```toml
[package]
name = "kiteframe"
version = "0.1.0"
edition = "2024"
rust-version = "1.97.1"
license = "Apache-2.0"
repository = "https://github.com/swiftdiaries/kiteframe"

[lib]
path = "src/lib.rs"

[[bin]]
name = "kiteframe"
path = "src/main.rs"

[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
all = "deny"
pedantic = "deny"
```

Create the two compileable targets before running the structural test:

```rust
// src/lib.rs
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
```

```rust
// src/main.rs
fn main() {
    println!("kiteframe {}", kiteframe::VERSION);
}
```

- [ ] **Step 2: Write the failing repository-shape test**

```rust
// tests/repository_shape.rs
use std::path::Path;

#[test]
fn runtime_is_one_rust_package() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for removed in [
        "crates", "python", "fuzz", "openfga", "runtime-targets", "schemas",
        "scripts",
    ] {
        assert!(!root.join(removed).exists(), "obsolete path remains: {removed}");
    }
    assert!(root.join("src/lib.rs").is_file());
    assert!(root.join("src/main.rs").is_file());
}
```

- [ ] **Step 3: Run the structural test and verify the old architecture fails it**

Run: `cargo test --test repository_shape`

Expected: FAIL on `crates` before the first obsolete path is removed.

- [ ] **Step 4: Delete only the superseded runtime surfaces**

Use `git rm -r` for the seven tracked obsolete directories and the old tracked
`tests/` contents. Recreate `tests/repository_shape.rs` if the directory removal
staged it. Before asserting the directories are gone, list ignored files with
`git status --ignored --short` and remove only these currently observed generated
paths:

```text
fuzz/corpus/
fuzz/target/
python/kiteframe-deepagents/.venv/
python/kiteframe/.pytest_cache/
python/kiteframe/.ruff_cache/
python/kiteframe/.venv/
```

Do not remove root `target/`, root Python caches, `docs/`, or untracked `site/`.

- [ ] **Step 5: Replace CI and README with honest monolith gates**

CI has one Rust job with these commands:

```yaml
- run: cargo fmt --all --check
- run: cargo clippy --all-targets --all-features -- -D warnings
- run: cargo test --all-targets --all-features
```

README states that the old package/Deep Agents implementation has been removed,
links the approved design and this plan, and labels the new engine as under
implementation. It must not claim DSL, runtime, or API behavior yet.

- [ ] **Step 6: Regenerate the lockfile and verify the clean shell**

Run:

```bash
cargo generate-lockfile
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
git status --short
```

Expected: all Rust gates PASS; `site/` remains untracked and unstaged.

- [ ] **Step 7: Commit the repository replacement boundary**

```bash
git add Cargo.toml Cargo.lock README.md .github/workflows/ci.yml src tests/repository_shape.rs
git add -u crates python fuzz openfga runtime-targets schemas scripts tests
git commit -m "refactor: replace legacy kiteframe workspace with monolith shell"
```

## Task 2: Add Closed Schemas, Domain Types, and the Trusted Tool Boundary

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/lib.rs`
- Create: `src/domain.rs`
- Create: `src/hash.rs`
- Create: `src/schema.rs`
- Create: `src/tools/mod.rs`
- Create: `src/tools/descriptor.rs`
- Create: `src/tools/registry.rs`
- Create: `tests/schema_and_tools.rs`

**Interfaces:**

```rust
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self;
    pub fn expose(&self) -> &str;
}

pub enum ClosedSchema {
    String { enum_values: Vec<String>, nullable: bool },
    Integer { enum_values: Vec<i64>, nullable: bool },
    Number { enum_values: Vec<serde_json::Number>, nullable: bool },
    Boolean { nullable: bool },
    Array { items: Box<ClosedSchema>, nullable: bool },
    Object {
        properties: std::collections::BTreeMap<String, ClosedSchema>,
        nullable: bool,
    },
}

impl ClosedSchema {
    pub fn validate(&self, value: &serde_json::Value) -> Result<(), ValueError>;
    pub fn strict_json_schema(&self) -> serde_json::Value;
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn descriptor(&self) -> &ToolDescriptor;
    async fn invoke(&self, call: ToolCall) -> Result<serde_json::Value, ToolError>;
    async fn status(&self, key: &IdempotencyKey) -> Result<ToolStatus, ToolError>;
}

pub struct ToolCall {
    pub run_id: RunId,
    pub frame_id: FrameId,
    pub arguments: serde_json::Value,
    pub idempotency_key: IdempotencyKey,
}

pub struct ToolError {
    pub code: String,
    pub message: String,
    pub transient: bool,
}
```

- Consumes no old Kiteframe contract types.
- Produces all IDs, states, schemas, limit types, failures, tool descriptors,
  `Tool`, and `ToolRegistry` used by every later task.

- [ ] **Step 1: Add only the foundational dependencies**

```toml
[dependencies]
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_json_canonicalizer = "0.3"
sha2 = "0.10"
thiserror = "2"
time = { version = "0.3", features = ["serde"] }
uuid = { version = "1", features = ["serde", "v7"] }
```

- [ ] **Step 2: Write failing closed-schema tests**

```rust
#[test]
fn strict_optional_property_is_required_and_nullable() {
    let schema = ClosedSchema::object([
        ("ticket", ClosedSchema::string()),
        ("clarification", ClosedSchema::nullable_string()),
    ]);
    assert_eq!(
        schema.strict_json_schema(),
        json!({
            "type": "object",
            "properties": {
                "clarification": {"type": ["string", "null"]},
                "ticket": {"type": "string"}
            },
            "required": ["clarification", "ticket"],
            "additionalProperties": false
        })
    );
    assert!(schema.validate(&json!({"ticket": "T-1", "clarification": null})).is_ok());
    assert!(schema.validate(&json!({"ticket": "T-1", "extra": true})).is_err());
}
```

- [ ] **Step 3: Run the schema test and verify missing types fail compilation**

Run: `cargo test --test schema_and_tools strict_optional_property_is_required_and_nullable -- --exact`

Expected: FAIL because `ClosedSchema` does not exist.

- [ ] **Step 4: Implement the closed schema and canonical hash helpers**

`ClosedSchema::strict_json_schema` sorts object properties through `BTreeMap`,
marks every property required, emits `additionalProperties: false`, and uses a
two-element type array only for nullability. `validate` rejects unknown fields,
missing properties, non-finite numeric representations, and enum misses.

```rust
pub fn canonical_json(value: &serde_json::Value) -> Result<Vec<u8>, HashError> {
    serde_json_canonicalizer::to_vec(value).map_err(HashError::Canonicalize)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
```

- [ ] **Step 5: Write failing tool-boundary tests**

```rust
#[test]
fn effectful_tool_requires_recovery_support() {
    let descriptor = descriptor(EffectKind::Effectful, RecoverySupport::None);
    assert_eq!(descriptor.validate().unwrap_err().code(), "tool_recovery_required");
}

#[test]
fn registry_rejects_duplicate_names_and_hashes_descriptors() {
    let tool = std::sync::Arc::new(EchoTool::new());
    assert!(ToolRegistry::try_new([tool.clone(), tool]).is_err());
    let registry = ToolRegistry::try_new([std::sync::Arc::new(EchoTool::new())]).unwrap();
    assert_eq!(registry.descriptor_hash("echo").unwrap().len(), 64);
}
```

- [ ] **Step 6: Implement exact domain and tool contracts**

`src/domain.rs` defines UUID-backed `DefinitionId`, `RunId`, `FrameId`,
`AttemptId`, `ToolCallId`, and `SuspensionId`; validated `WorkflowName` and
`IdempotencyKey`; validated `TaskName` and `BindingName` ordered string newtypes
that implement `Borrow<str>`; `RunStatus`, `FrameStatus`, `FailureKind`,
`Failure`, `Limits`, and a `Secret` string wrapper whose `Debug` and `Display`
implementations are always redacted. Generate UUID v7 IDs in process.

```rust
pub enum EffectKind { ReadOnly, Effectful }
pub enum RecoverySupport { None, Idempotent, Status, IdempotentAndStatus }
pub enum ToolStatus {
    Unsupported,
    NotStarted,
    Running,
    Succeeded(serde_json::Value),
    Failed { code: String, message: String },
    Unknown,
}

pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input: ClosedSchema,
    pub output: ClosedSchema,
    pub effect: EffectKind,
    pub recovery: RecoverySupport,
}
```

`ToolRegistry` owns a `BTreeMap<String, Arc<dyn Tool>>`, validates descriptors
once at startup, exposes exact descriptor hashes, and never imports code or
endpoints from workflow source.

- [ ] **Step 7: Run focused and cumulative gates**

Run:

```bash
cargo test --test schema_and_tools
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS and Clippy emits no warnings.

- [ ] **Step 8: Commit the foundational contracts**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/domain.rs src/hash.rs src/schema.rs src/tools tests/schema_and_tools.rs
git commit -m "feat: add minimal schema and tool contracts"
```

## Task 3: Parse the Author-Authored DSL

**Files:**

- Modify: `src/lib.rs`
- Create: `src/dsl/mod.rs`
- Create: `src/dsl/diagnostic.rs`
- Create: `src/dsl/lexer.rs`
- Create: `src/dsl/ast.rs`
- Create: `src/dsl/parser.rs`
- Create: `tests/dsl_parser.rs`
- Create: `tests/fixtures/dsl/support.kite`
- Create: `examples/support.kite`

**Interfaces:**

```rust
pub fn parse(source: &str, limits: ParseLimits) -> Result<SourceFile, Vec<Diagnostic>>;

pub struct SourceFile {
    pub records: Vec<RecordAst>,
    pub workflow: WorkflowAst,
    pub tasks: Vec<TaskAst>,
}

pub enum StatementAst {
    Call { binding: String, target: String, arguments: Vec<ArgumentAst> },
    Parallel { branches: Vec<CallStatementAst> },
    Ask { binding: String, answer_type: TypeAst, question: ValueRefAst },
    Goto { target: String, arguments: Vec<ArgumentAst> },
    Done { value: ValueRefAst },
    Fail { code: ValueRefAst, message: ValueRefAst },
}
```

- Consumes `ClosedSchema` vocabulary only during the next compiler task; the
  parser preserves authored type names and spans.
- Produces a complete AST with byte spans on every declaration and statement.

- [ ] **Step 1: Check in the complete valid workflow fixture**

Copy the approved design's `Evidence`, `support`, `triage`, `inspect_logs`,
`inspect_metrics`, and `compose` definitions verbatim into
`tests/fixtures/dsl/support.kite`, and copy the same source to
`examples/support.kite`.

- [ ] **Step 2: Write failing parser tests for the full surface**

```rust
#[test]
fn parses_the_complete_v1_surface() {
    let source = include_str!("fixtures/dsl/support.kite");
    let file = parse(source, ParseLimits::default()).unwrap();
    assert_eq!(file.workflow.name.text(), "support");
    assert_eq!(file.tasks.len(), 4);
    assert!(file.tasks[0].outcomes.iter().any(|outcome| {
        outcome.statements.iter().any(|statement| matches!(statement, StatementAst::Parallel { .. }))
    }));
    assert!(file.tasks[0].outcomes.iter().any(|outcome| {
        outcome.statements.iter().any(|statement| matches!(statement, StatementAst::Ask { .. }))
    }));
}

#[test]
fn reports_bounded_lexical_errors_with_spans() {
    let diagnostics = parse("workflow x() -> string {", ParseLimits::default()).unwrap_err();
    assert_eq!(diagnostics[0].code, "KF-DSL-001");
    assert!(diagnostics[0].span.end > diagnostics[0].span.start);
}
```

- [ ] **Step 3: Run the parser tests and verify the module is missing**

Run: `cargo test --test dsl_parser`

Expected: FAIL because `kiteframe::dsl` and `parse` do not exist.

- [ ] **Step 4: Implement the bounded lexer**

The lexer recognizes identifiers, integers, quoted strings with JSON escapes,
triple-quoted UTF-8 prompts, punctuation, `->`, and the fixed keywords. It
tracks byte spans and rejects input beyond `max_source_bytes`, tokens beyond
`max_tokens`, identifiers beyond `max_identifier_bytes`, and unterminated
strings without panicking.

```rust
pub struct ParseLimits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_identifier_bytes: usize,
    pub max_nesting: usize,
}
```

- [ ] **Step 5: Implement the recursive-descent parser and exact grammar**

Parse exactly one workflow, zero or more `type` records, and one or more tasks.
The workflow body accepts one `start` and an optional `limits` block. Task
bodies require `prompt`, `tools`, and one or more `on` clauses in that order.
Outcome blocks accept zero or more `call`, `parallel`, or `ask` statements and
exactly one final `goto`, `done`, or `fail`. Reject trailing tokens and recover
only to the next top-level `type`, `workflow`, or `task` so diagnostics stay
bounded.

- [ ] **Step 6: Add hostile parser cases**

Add table-driven cases for duplicate workflow declarations, invalid escapes,
unclosed triple strings, excessive nesting, oversized source, empty tasks,
empty outcomes, and a non-terminal statement after `done`. Assert stable
diagnostic code, span, and at most 20 diagnostics per parse.

- [ ] **Step 7: Run parser and cumulative gates**

Run:

```bash
cargo test --test dsl_parser
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 8: Commit the DSL parser**

```bash
git add src/lib.rs src/dsl tests/dsl_parser.rs tests/fixtures/dsl/support.kite examples/support.kite
git commit -m "feat: parse author-authored kite workflows"
```

## Task 4: Compile and Validate Immutable Workflows

**Files:**

- Modify: `src/dsl/mod.rs`
- Create: `src/dsl/ir.rs`
- Create: `src/dsl/compiler.rs`
- Create: `tests/dsl_compiler.rs`
- Create: `tests/fixtures/dsl/support.compiled.json`

**Interfaces:**

```rust
pub fn compile(
    source: &str,
    registry: &ToolRegistry,
    ceilings: &Limits,
) -> Result<CompiledWorkflow, Vec<Diagnostic>>;

pub struct CompiledWorkflow {
    pub dsl_version: u32,
    pub name: WorkflowName,
    pub input: ClosedSchema,
    pub result: ClosedSchema,
    pub entry_task: TaskName,
    pub tasks: std::collections::BTreeMap<TaskName, CompiledTask>,
    pub limits: Limits,
    pub tool_descriptors: std::collections::BTreeMap<String, ToolDescriptor>,
    pub definition_hash: String,
}

pub enum CompiledStatement {
    Call { binding: BindingName, target: TaskName, arguments: Vec<Argument> },
    Parallel { branches: Vec<CompiledCall> },
    Ask { binding: BindingName, schema: ClosedSchema, question: ValueRef },
    Goto { target: TaskName, arguments: Vec<Argument> },
    Done { value: ValueRef },
    Fail { code: ValueRef, message: ValueRef },
}
```

- Consumes the parser AST, deployment ceilings, and exact `ToolRegistry`
  descriptor snapshots.
- Produces immutable, serializable IR with fully resolved names and no source
  spans in executable statements; diagnostics retain spans separately.

- [ ] **Step 1: Write the failing successful-compilation test**

```rust
#[test]
fn compiles_support_workflow_and_generates_strict_completion_schema() {
    let registry = support_registry();
    let compiled = compile(
        include_str!("fixtures/dsl/support.kite"),
        &registry,
        &Limits::deployment_default(),
    ).unwrap();
    let triage = &compiled.tasks["triage"];
    let completion = triage.complete_task_schema.strict_json_schema();
    assert_eq!(completion["additionalProperties"], false);
    assert_eq!(completion["required"], json!([
        "answer", "outcome", "question", "query"
    ]));
    assert_eq!(completion["properties"]["query"]["type"], json!(["string", "null"]));
    assert_eq!(compiled.definition_hash.len(), 64);
}
```

- [ ] **Step 2: Run the compiler test and verify the compiler is missing**

Run: `cargo test --test dsl_compiler compiles_support_workflow_and_generates_strict_completion_schema -- --exact`

Expected: FAIL because `CompiledWorkflow` and `compile` do not exist.

- [ ] **Step 3: Implement type resolution and strict completion schemas**

Resolve primitives, optionals, lists, and named records into `ClosedSchema`.
For each task, generate `complete_task` as a required `outcome` enum plus every
outcome field. All fields are required on the wire; fields not selected by the
outcome are nullable and must arrive as `null`. Reject the reserved names
`complete_task` and `outcome` and reject incompatible reuse of one field name
across outcomes.

- [ ] **Step 4: Write the failing validation matrix**

```rust
#[test]
fn rejects_each_static_contract_violation() {
    for (source, code) in invalid_workflows() {
        let diagnostics = compile(source, &support_registry(), &Limits::deployment_default())
            .unwrap_err();
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.code == code),
            "missing {code} for {source}");
    }
}
```

`invalid_workflows()` contains explicit source strings for duplicate names,
missing entry/target/tool, unsupported tool schema, argument/result/answer type
mismatch, unhandled or duplicate outcome, reserved names, incompatible outcome
fields, unbound and duplicate bindings, missing terminal operation, direct and
indirect recursive `call`, empty parallel, parallel binding collision,
unreachable task, non-positive workflow limit, and workflow limits above the
deployment ceiling.

- [ ] **Step 5: Implement graph, binding, and limit validation**

Build separate adjacency sets for `call` and all reachability edges. Reject
cycles only in the call graph; allow `goto` cycles. Walk each outcome block with
a typed binding environment. Require a `goto` target's result type to match the
current invocation, `done` to match that result, `fail` code/message to be
strings, and every parallel branch result to have a unique source-order binding.

- [ ] **Step 6: Implement deterministic definition identity**

Serialize this value with RFC 8785 and hash it with SHA-256:

```json
{
  "dsl_version": 1,
  "source": "<exact UTF-8 source>",
  "tool_descriptors": {
    "<tool-name>": "<complete descriptor JSON>"
  }
}
```

The tool map is ordered by name. Store both full captured descriptors and their
individual hashes in the IR.

- [ ] **Step 7: Freeze and verify the compiled golden fixture**

Generate `tests/fixtures/dsl/support.compiled.json` once from the compiler, then
make the test compare pretty-printed semantic JSON and the exact definition
hash. A second compile of the same source and descriptors must be byte-identical;
changing only a tool descriptor must change the definition hash.

- [ ] **Step 8: Run compiler and cumulative gates**

Run:

```bash
cargo test --test dsl_compiler
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 9: Commit the immutable compiler**

```bash
git add src/dsl tests/dsl_compiler.rs tests/fixtures/dsl/support.compiled.json
git commit -m "feat: compile validated immutable workflows"
```

## Task 5: Implement the Portable Responses Client

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/lib.rs`
- Create: `src/model/mod.rs`
- Create: `src/model/types.rs`
- Create: `src/model/responses.rs`
- Create: `tests/support/mod.rs`
- Create: `tests/support/responses_server.rs`
- Create: `tests/responses_conformance.rs`

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

pub enum ModelErrorKind {
    Timeout,
    RateLimited,
    Unavailable,
    HttpTerminal,
    Protocol,
    ResponseTooLarge,
}

pub struct ModelError {
    pub kind: ModelErrorKind,
    pub message: String,
}

pub struct ModelResponse {
    pub response_id: Option<String>,
    pub output: Vec<ResponseItem>,
    pub usage: Option<serde_json::Value>,
}

pub struct ResponseItem {
    pub raw: serde_json::Value,
    pub kind: ResponseItemKind,
}

pub enum ResponseItemKind {
    Message,
    Refusal,
    Reasoning,
    FunctionCall { call_id: String, name: String, arguments: String },
    FunctionCallOutput,
    Unknown,
}
```

- Consumes provider configuration, compiled tool schemas, and full ordered item
  history.
- Produces lossless response items and typed transport/protocol failures. It
  owns no workflow state, retry loop, or task transitions.

- [ ] **Step 1: Add the async HTTP dependencies**

```toml
[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
tokio = { version = "1", features = ["macros", "net", "rt-multi-thread", "sync", "time"] }

[dev-dependencies]
axum = "0.8"
```

- [ ] **Step 2: Write a failing exact-request conformance test**

```rust
#[tokio::test]
async fn sends_stateless_request_and_replays_items_without_loss() {
    let server = ResponsesServer::start(json!({
        "id": "resp_1",
        "status": "completed",
        "output": [
            {"type": "reasoning", "encrypted_content": "opaque"},
            {"type": "provider_extension", "value": 7},
            {"type": "function_call", "call_id": "call_1", "name": "search", "arguments": "{\"q\":\"x\"}"}
        ]
    })).await;
    let client = ResponsesClient::new(server.config()).unwrap();
    let response = client.respond(request_with_prior_items()).await.unwrap();
    let body = server.single_request().await;
    assert_eq!(body["store"], false);
    assert_eq!(body["input"][1]["encrypted_content"], "prior-opaque");
    assert_eq!(response.output[1].kind, ResponseItemKind::Unknown);
    assert_eq!(response.output[1].raw["value"], 7);
}
```

- [ ] **Step 3: Run the conformance test and verify the client is missing**

Run: `cargo test --test responses_conformance sends_stateless_request_and_replays_items_without_loss -- --exact`

Expected: FAIL because `ResponsesClient` and `ModelClient` do not exist.

- [ ] **Step 4: Implement request construction and configuration validation**

```rust
pub struct ResponsesClientConfig {
    pub base_url: url::Url,
    pub api_key: Option<Secret>,
    pub headers: reqwest::header::HeaderMap,
    pub model: String,
    pub timeout: std::time::Duration,
    pub max_response_bytes: usize,
}
```

Add direct `url = "2"` to dependencies. Reject base URLs that cannot append one
`responses` path segment, contain user information, empty models, invalid
headers, zero timeouts, and zero response limits. `Secret` exposes bytes only to
request construction and implements `Debug` as `Secret([REDACTED])`.
Build one reusable Reqwest client with rustls and disabled redirects. Send
`Authorization: Bearer ...` only when configured. Never include credentials in
`Debug`, errors, or logs.

Each body has exactly `model`, `instructions`, `input`, `tools`,
`tool_choice: "auto"`, and `store: false`. Do not send
`previous_response_id`, `conversation`, `background`, `stream`, or hosted tools.

- [ ] **Step 5: Implement lossless response parsing**

Require HTTP success, top-level `status: "completed"`, and an output array.
Recognize the item kinds in `ResponseItemKind` while retaining each untouched
JSON object in `raw`. Reject duplicate function `call_id`, missing names,
non-string arguments, incomplete responses, malformed JSON, oversized bodies,
and non-success statuses with stable `ModelErrorKind` values.

- [ ] **Step 6: Extend conformance coverage**

Add explicit tests for custom headers, no API key, request timeout, HTTP 429/500
classification as transient, 400 as terminal, refusal preservation, malformed
function calls, duplicate call IDs, unknown item replay, response-size limit,
and a base URL already ending in `/v1` producing exactly `/v1/responses`.

- [ ] **Step 7: Run focused and cumulative gates**

Run:

```bash
cargo test --test responses_conformance
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 8: Commit the model protocol boundary**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/model tests/support tests/responses_conformance.rs
git commit -m "feat: add portable responses client"
```

## Task 6: Create the PostgreSQL Schema and Definition/Run Store

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/lib.rs`
- Create: `migrations/0001_workflow_engine.sql`
- Create: `src/store/mod.rs`
- Create: `src/store/models.rs`
- Create: `src/store/postgres.rs`
- Create: `tests/store_definitions.rs`

**Interfaces:**

```rust
pub struct PostgresStore { pool: sqlx::PgPool }

pub struct SingleProcessGuard {
    connection: sqlx::pool::PoolConnection<sqlx::Postgres>,
}

pub struct StartRun {
    pub workflow_name: WorkflowName,
    pub version: Option<i64>,
    pub input: serde_json::Value,
    pub idempotency_key: IdempotencyKey,
    pub active_tool_hashes: std::collections::BTreeMap<String, String>,
}

pub struct RunView {
    pub id: RunId,
    pub definition_id: DefinitionId,
    pub status: RunStatus,
    pub output: Option<serde_json::Value>,
    pub failure: Option<Failure>,
    pub suspensions: Vec<SuspensionView>,
    pub revision: i64,
}

pub struct PublishedDefinition {
    pub id: DefinitionId,
    pub name: WorkflowName,
    pub version: i64,
    pub definition_hash: String,
}

pub struct SuspensionView {
    pub id: SuspensionId,
    pub frame_id: FrameId,
    pub question: String,
    pub answer_schema: ClosedSchema,
}

pub struct RunEvent {
    pub run_id: RunId,
    pub sequence: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: time::OffsetDateTime,
}

pub enum StoreError {
    NotFound,
    Validation(String),
    IdempotencyConflict,
    RevisionConflict,
    DefinitionUnavailable(String),
    ServiceAlreadyRunning,
    Database(String),
}

impl PostgresStore {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, StoreError>;
    pub async fn migrate(&self) -> Result<(), StoreError>;
    pub async fn try_acquire_single_process_guard(&self)
        -> Result<SingleProcessGuard, StoreError>;
    pub async fn publish(&self, workflow: &CompiledWorkflow, source: &str)
        -> Result<PublishedDefinition, StoreError>;
    pub async fn start_run(&self, request: StartRun) -> Result<RunView, StoreError>;
    pub async fn get_run(&self, run_id: RunId) -> Result<RunView, StoreError>;
    pub async fn events_after(&self, run_id: RunId, after: i64, limit: i64)
        -> Result<Vec<RunEvent>, StoreError>;
}
```

- Consumes immutable compiled workflows and validated start input.
- Produces the six-table durable schema, atomic publication, idempotent run
  creation, root frames, initial model input items, and ordered events.

- [ ] **Step 1: Add SQLx PostgreSQL support**

```toml
sqlx = { version = "0.8", default-features = false, features = [
  "json", "migrate", "postgres", "runtime-tokio-rustls", "time", "uuid"
] }
```

- [ ] **Step 2: Write the failing migration and definition-store tests**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn publication_is_versioned_and_same_definition_is_idempotent(pool: sqlx::PgPool) {
    let store = PostgresStore::from_pool(pool);
    let workflow = compiled_support();
    let first = store.publish(&workflow, support_source()).await.unwrap();
    let same = store.publish(&workflow, support_source()).await.unwrap();
    assert_eq!(same.id, first.id);
    let changed = store.publish(&compiled_with_changed_tool(), support_source()).await.unwrap();
    assert_eq!(changed.version, first.version + 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn start_key_replays_same_run_and_conflicts_on_changed_input(pool: sqlx::PgPool) {
    let store = store_with_support(pool).await;
    let first = store.start_run(start("key-1", json!({"ticket": "T-1"}))).await.unwrap();
    let same = store.start_run(start("key-1", json!({"ticket": "T-1"}))).await.unwrap();
    assert_eq!(same.id, first.id);
    assert!(matches!(
        store.start_run(start("key-1", json!({"ticket": "T-2"}))).await,
        Err(StoreError::IdempotencyConflict)
    ));
}
```

- [ ] **Step 3: Run the store tests and verify the schema is absent**

Run: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test store_definitions`

Expected: FAIL because the migration and `PostgresStore` do not exist. If local
PostgreSQL is not running, start the same PostgreSQL 16 service defined in Task
13's CI before evaluating the expected code failure.

- [ ] **Step 4: Create the six-table migration with exact constraints**

```sql
CREATE TABLE workflow_definitions (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    source text NOT NULL,
    compiled jsonb NOT NULL,
    definition_hash char(64) NOT NULL,
    tool_descriptor_hashes jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (name, version),
    UNIQUE (name, definition_hash)
);

CREATE TABLE runs (
    id uuid PRIMARY KEY,
    definition_id uuid NOT NULL REFERENCES workflow_definitions(id),
    workflow_name text NOT NULL,
    start_idempotency_key text NOT NULL,
    start_request_hash char(64) NOT NULL,
    input jsonb NOT NULL,
    status text NOT NULL CHECK (status IN
      ('queued','running','waiting_for_answer','outcome_unknown','succeeded','failed','cancelled')),
    limits jsonb NOT NULL,
    output jsonb,
    failure jsonb,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    event_sequence bigint NOT NULL DEFAULT 0 CHECK (event_sequence >= 0),
    step_count bigint NOT NULL DEFAULT 0 CHECK (step_count >= 0),
    active_millis bigint NOT NULL DEFAULT 0 CHECK (active_millis >= 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (workflow_name, start_idempotency_key)
);

CREATE TABLE frames (
    id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    parent_frame_id uuid REFERENCES frames(id),
    join_id uuid,
    branch_index integer,
    task_name text NOT NULL,
    cursor jsonb NOT NULL,
    bindings jsonb NOT NULL,
    state text NOT NULL CHECK (state IN
      ('runnable','waiting_for_model','waiting_for_tool','waiting_for_children',
       'waiting_for_answer','outcome_unknown','completed','failed','cancelled')),
    result jsonb,
    suspension_id uuid,
    question text,
    answer_schema jsonb,
    answer jsonb,
    answer_idempotency_key text,
    active_attempt_id uuid,
    active_attempt_started_at timestamptz,
    model_turns bigint NOT NULL DEFAULT 0 CHECK (model_turns >= 0),
    tool_calls bigint NOT NULL DEFAULT 0 CHECK (tool_calls >= 0),
    cancel_requested boolean NOT NULL DEFAULT false,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((branch_index IS NULL) = (join_id IS NULL)),
    CHECK ((suspension_id IS NULL) = (question IS NULL)),
    CHECK ((suspension_id IS NULL) = (answer_schema IS NULL)),
    CHECK ((active_attempt_id IS NULL) = (active_attempt_started_at IS NULL))
);

CREATE UNIQUE INDEX frames_one_suspension
    ON frames(suspension_id) WHERE suspension_id IS NOT NULL;
CREATE INDEX frames_runnable ON frames(run_id, id) WHERE state = 'runnable';

CREATE TABLE model_items (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    frame_id uuid NOT NULL REFERENCES frames(id) ON DELETE CASCADE,
    position bigint NOT NULL CHECK (position >= 0),
    attempt_id uuid,
    item jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (frame_id, position)
);

CREATE TABLE tool_calls (
    id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    frame_id uuid NOT NULL REFERENCES frames(id) ON DELETE CASCADE,
    provider_call_id text NOT NULL,
    name text NOT NULL,
    arguments jsonb NOT NULL,
    idempotency_key text NOT NULL UNIQUE,
    effect text NOT NULL CHECK (effect IN ('read_only','effectful')),
    recovery text NOT NULL CHECK (recovery IN ('none','idempotent','status','idempotent_and_status')),
    status text NOT NULL CHECK (status IN
      ('pending','running','succeeded','failed','outcome_unknown','cancelled')),
    result jsonb,
    failure jsonb,
    started_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (frame_id, provider_call_id)
);

CREATE TABLE events (
    run_id uuid NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    sequence bigint NOT NULL CHECK (sequence > 0),
    kind text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (run_id, sequence)
);
```

- [ ] **Step 5: Implement migration, atomic publication, and descriptor checks**

Use `static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();` and run it
before readiness. Publication takes a transaction-level advisory lock derived
from workflow name, returns the existing `(name, definition_hash)` row when
present, otherwise assigns `MAX(version) + 1`, and inserts source plus compiled
IR. Before `start_run`, compare every captured descriptor hash with the active
registry; return `DefinitionUnavailable` on any mismatch.

`try_acquire_single_process_guard` checks one fixed session advisory-lock key on
a dedicated pooled connection. Return `ServiceAlreadyRunning` when the lock is
held and retain that connection inside `SingleProcessGuard` so dropping the
guard releases the lock.

- [ ] **Step 6: Implement idempotent run creation and first event**

Hash canonical `{definition_id, input}` as `start_request_hash`. In one
transaction insert the run, root frame, initial canonical-argument user item at
position 0, and `run.started` event sequence 1. On unique-key conflict, return
the existing run only when request hashes match.

- [ ] **Step 7: Test immutable versions, latest-version selection, and event order**

Add tests for explicit version, latest version in the same transaction,
descriptor mismatch, invalid input before insert, concurrent publication of two
different definitions, immutable definition rows, root frame creation, and
`events_after` exclusive cursors.

- [ ] **Step 8: Run focused and cumulative gates**

Run:

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test store_definitions
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 9: Commit the first durable store slice**

```bash
git add Cargo.toml Cargo.lock migrations src/lib.rs src/store tests/store_definitions.rs
git commit -m "feat: persist workflow definitions and runs"
```

## Task 7: Add Transactional Frame, Model, Tool, and Suspension Operations

**Files:**

- Modify: `src/store/models.rs`
- Modify: `src/store/postgres.rs`
- Create: `tests/store_execution.rs`

**Interfaces:**

```rust
pub struct FrameSnapshot {
    pub id: FrameId,
    pub run_id: RunId,
    pub task_name: TaskName,
    pub cursor: FrameCursor,
    pub bindings: serde_json::Map<String, serde_json::Value>,
    pub state: FrameStatus,
    pub revision: i64,
    pub cancel_requested: bool,
}

pub enum FrameCursor {
    Model,
    Outcome {
        name: String,
        fields: serde_json::Map<String, serde_json::Value>,
        next: usize,
    },
}

pub struct NewChild {
    pub target: TaskName,
    pub binding: String,
    pub bindings: serde_json::Map<String, serde_json::Value>,
    pub branch_index: i32,
}

pub struct PendingToolCall {
    pub provider_call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub source_index: i32,
}

pub enum ModelCommitAction {
    Outcome { name: String, fields: serde_json::Map<String, serde_json::Value> },
    Tools { calls: Vec<PendingToolCall> },
    Invalid { feedback_item: serde_json::Value },
}

pub enum FrameCommand {
    StartCall { child: NewChild, next_cursor: FrameCursor },
    StartParallel { children: Vec<NewChild>, next_cursor: FrameCursor },
    SuspendAsk { suspension_id: SuspensionId, binding: String, question: String,
                 schema: ClosedSchema, next_cursor: FrameCursor },
    Goto { task: TaskName, bindings: serde_json::Map<String, serde_json::Value> },
    Complete { result: serde_json::Value },
    Fail { failure: Failure },
}
```

`PostgresStore` adds `compiled_workflow_for_run`, `frame`, `model_items`,
`runnable_frames`, `begin_model_attempt`, `commit_model_attempt`,
`begin_next_tool_call`, `finish_tool_call`, `apply_frame_command`, `answer`,
`cancel_run`, `mark_run_failure`, and `open_suspensions`.

- [ ] **Step 1: Write failing model-attempt transaction tests**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn model_intent_precedes_items_and_revision_rejects_double_finish(pool: sqlx::PgPool) {
    let store = started_store(pool).await;
    let frame = store.runnable_frames(1).await.unwrap().remove(0);
    let attempt = store.begin_model_attempt(frame.id, frame.revision).await.unwrap().unwrap();
    assert_eq!(store.frame(frame.id).await.unwrap().state, FrameStatus::WaitingForModel);
    store.commit_model_attempt(outcome_commit(&attempt)).await.unwrap();
    assert!(matches!(
        store.commit_model_attempt(outcome_commit(&attempt)).await,
        Err(StoreError::RevisionConflict)
    ));
}
```

- [ ] **Step 2: Run the model-attempt test and verify methods are missing**

Run: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test store_execution model_intent_precedes_items_and_revision_rejects_double_finish -- --exact`

Expected: FAIL because execution-store methods do not exist.

- [ ] **Step 3: Implement model intent and completion transactions**

`begin_model_attempt` locks the runnable frame at its expected revision, checks
run cancellation and limits, assigns an `AttemptId`, changes state to
`waiting_for_model`, increments model turns/revisions, and appends
`model.started` with the database-clock start time. `commit_model_attempt`
verifies the active attempt, appends all raw response items at consecutive
positions, records usage metadata only in the event, charges capped elapsed
active time, applies one `ModelCommitAction`, clears the attempt and start time,
and appends exactly one completion/invalid event in one transaction.

- [ ] **Step 4: Write failing tool intent and idempotency tests**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn tool_intent_is_unique_and_outputs_follow_call_order(pool: sqlx::PgPool) {
    let store = frame_waiting_on_two_tools(pool).await;
    let first = store.begin_next_tool_call(frame_id()).await.unwrap().unwrap();
    let replay = store.begin_next_tool_call(frame_id()).await.unwrap().unwrap();
    assert_eq!(first.id, replay.id);
    store.finish_tool_call(success(&first, json!({"value": 1}))).await.unwrap();
    let second = store.begin_next_tool_call(frame_id()).await.unwrap().unwrap();
    assert_ne!(second.id, first.id);
    assert!(store.model_items(frame_id()).await.unwrap().last().unwrap().item
        .get("call_id").is_some());
}
```

- [ ] **Step 5: Implement ordered durable tool calls**

Insert every proposed ordinary function call during the model completion
transaction with status `pending`, source-order position in its JSON failure
metadata, and idempotency key
`sha256("tool\0" || run_id || frame_id || provider_call_id)`. Start only the
first non-terminal call. On success validate output against the captured tool
schema, append a `function_call_output` model item, and advance to the next
pending call or back to runnable model state. A definitive tool error records
`tool_failed` and fails the run.

- [ ] **Step 6: Write failing call/parallel/ask atomicity tests**

Add PostgreSQL tests proving one `FrameCommand` transaction creates a single
child or all parallel children, stores source-order branch indexes, suspends an
ask, accepts one typed answer, replays the same answer key, rejects a different
answer/key, allocates gap-free event sequences, and rejects stale revisions.

- [ ] **Step 7: Implement frame commands, joins, answers, and cancellation**

`apply_frame_command` locks frame and run, increments `step_count`, and applies
exactly one command. `StartCall` and `StartParallel` set the parent
`waiting_for_children`; `SuspendAsk` sets only that frame
`waiting_for_answer`; `Complete` marks a child terminal and resumes its parent
only when the complete join succeeds, or succeeds the root run. A root `Fail`
fails the run immediately. A child `Fail` marks that branch failed, cancels
unstarted siblings, requests cancellation of in-flight siblings, and leaves the
parent waiting until the join settles; only then does the join fail the run.
Recompute run state as `running` while executable/in-flight frames exist and
`waiting_for_answer` only when all remaining non-terminal frames are suspended.

`cancel_run` sets run status `cancelled`, marks runnable and suspended frames
cancelled, sets `cancel_requested` on in-flight frames, and never deletes model
items, tool calls, or events.

- [ ] **Step 8: Run focused and cumulative gates**

Run:

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test store_execution
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 9: Commit the execution-store transactions**

```bash
git add src/store tests/store_execution.rs
git commit -m "feat: persist durable frame and external-call turns"
```

## Task 8: Interpret the Six Control Operations Purely

**Files:**

- Modify: `src/lib.rs`
- Create: `src/engine/mod.rs`
- Create: `src/engine/transition.rs`
- Create: `tests/transition_engine.rs`

**Interfaces:**

```rust
pub fn next_command(
    workflow: &CompiledWorkflow,
    frame: &FrameSnapshot,
) -> Result<FrameCommand, Failure>;
```

- Consumes only immutable IR and a frame snapshot.
- Produces one validated `FrameCommand`; it performs no I/O and mutates no
  shared state.

- [ ] **Step 1: Write failing table-driven transition tests**

```rust
#[test]
fn evaluates_every_control_operation_and_preserves_parallel_source_order() {
    let cases = transition_cases();
    for case in cases {
        assert_eq!(next_command(&case.workflow, &case.frame).unwrap(), case.expected);
    }
    let parallel = case_named("parallel").expected;
    let FrameCommand::StartParallel { children, .. } = parallel else { panic!() };
    assert_eq!(children.iter().map(|child| child.binding.as_str()).collect::<Vec<_>>(),
               ["logs", "metrics"]);
}
```

- [ ] **Step 2: Run the transition test and verify the evaluator is missing**

Run: `cargo test --test transition_engine evaluates_every_control_operation_and_preserves_parallel_source_order -- --exact`

Expected: FAIL because `next_command` does not exist.

- [ ] **Step 3: Implement binding evaluation and one-command stepping**

Resolve only literals and existing typed bindings. At cursor `next`, return the
command for that statement and embed `next + 1` in its continuation where
needed. `goto`, `done`, and `fail` have no continuation. Revalidate arguments
and results against compiled schemas as an invariant check; map impossible IR
violations to `FailureKind::Internal` rather than accepting corrupt state.

- [ ] **Step 4: Cover nested calls, joins, asks, and terminal behavior**

Add cases proving a child `done` result binds into the parent continuation,
`goto` replaces task/bindings and resets cursor to `Model`, optional omitted
arguments become explicit null, `ask` uses the declared schema, `fail` maps to
`workflow_failed`, and no statement after a terminal operation can execute.

- [ ] **Step 5: Run pure and cumulative gates**

Run:

```bash
cargo test --test transition_engine
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 6: Commit the pure interpreter**

```bash
git add src/lib.rs src/engine tests/transition_engine.rs
git commit -m "feat: interpret typed workflow transitions"
```

## Task 9: Run Sequential Model and Tool Turns

**Files:**

- Create: `src/engine/turn.rs`
- Create: `src/engine/scheduler.rs`
- Modify: `src/engine/mod.rs`
- Create: `tests/support/fake_model.rs`
- Create: `tests/support/fake_tools.rs`
- Create: `tests/sequential_runtime.rs`

**Interfaces:**

```rust
pub struct Engine {
    store: std::sync::Arc<PostgresStore>,
    model: std::sync::Arc<dyn ModelClient>,
    tools: std::sync::Arc<ToolRegistry>,
    config: EngineConfig,
    wake: std::sync::Arc<tokio::sync::Notify>,
}

pub struct EngineConfig {
    pub max_concurrent_frames: usize,
    pub scheduler_batch: i64,
    pub idle_poll: std::time::Duration,
    pub retry: RetryPolicy,
}

pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: std::time::Duration,
    pub max_delay: std::time::Duration,
}

pub type Shutdown = tokio::sync::watch::Receiver<bool>;

pub enum EngineError {
    Store(StoreError),
    Model(ModelError),
    Tool(ToolError),
    Invariant(Failure),
}

impl Engine {
    pub async fn run(self: std::sync::Arc<Self>, shutdown: Shutdown) -> Result<(), EngineError>;
    pub async fn tick_until_idle(&self) -> Result<(), EngineError>;
    pub fn wake(&self);
}
```

- Consumes `PostgresStore`, `ModelClient`, `ToolRegistry`, compiled workflows,
  and the pure transition evaluator.
- Produces the direct model-with-tools loop for sequential tasks. It does not
  add a framework or another runtime abstraction.

- [ ] **Step 1: Write a failing sequential workflow test**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn model_calls_allowed_tool_then_completes_root(pool: sqlx::PgPool) {
    let model = FakeModel::script([
        response_with_call("call_1", "search_docs", json!({"query": "T-1"})),
        response_with_outcome("resolved", json!({"answer": "fixed"})),
    ]);
    let tools = registry([FakeTool::read_only("search_docs", json!({"doc": "answer"}))]);
    let app = runtime(pool, model.clone(), tools).await;
    let run = app.start("support", json!({"ticket": "T-1"}), "run-key").await;
    app.engine.tick_until_idle().await.unwrap();
    assert_eq!(app.store.get_run(run.id).await.unwrap().output, Some(json!("fixed")));
    assert_eq!(model.requests()[1].input.last().unwrap()["type"], "function_call_output");
}
```

- [ ] **Step 2: Run the sequential test and verify the engine is missing**

Run: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test sequential_runtime model_calls_allowed_tool_then_completes_root -- --exact`

Expected: FAIL because `Engine`, fake clients, and turn execution do not exist.

- [ ] **Step 3: Implement one-process scheduling**

The scheduler reads a bounded list of runnable frames, excludes frame IDs
already active in its local set, acquires one owned semaphore permit per frame,
and tracks tasks in a `tokio::task::JoinSet`. There is exactly one scheduler
loop. Dropping in-memory tasks never changes durable state; every external call
has already committed intent.

`tick_until_idle` uses the same path without sleeping and returns only when no
runnable frame or locally active task remains, making integration tests
deterministic.

- [ ] **Step 4: Implement model turn construction and completion validation**

Build `instructions` from the immutable prompt plus fixed completion rules.
Load all `model_items` in position order, capture the exact tool descriptors
from the definition, append generated `complete_task`, and call
`begin_model_attempt` before `ModelClient::respond`.

Classify a response as exactly one of:

```rust
enum ModelDecision {
    OrdinaryTools(Vec<PendingToolCall>),
    Outcome { name: String, fields: serde_json::Map<String, serde_json::Value> },
    Invalid { feedback: String },
}
```

Reject a mixed completion/ordinary-tool response, unknown tool, malformed
arguments, assistant message without a call, refusal, duplicate completion,
or an outcome whose selected fields/null sentinels violate the compiled schema.
Persist invalid feedback as a metadata-free user item and retry only within the
task's model-turn budget.

- [ ] **Step 5: Implement ordered ordinary-tool execution**

Call `begin_next_tool_call` before looking up the handler. Verify the current
registry descriptor hash equals the definition snapshot. Invoke with run,
frame, validated arguments, and stored idempotency key. Validate successful
output before `finish_tool_call`; map transport/handler failure into the stable
tool error path. Execute multiple calls from one model response strictly in
response item order.

- [ ] **Step 6: Add sequential call/goto and isolation tests**

Add scripted tests proving `call` creates a transcript containing only the
child prompt/arguments, child `done` returns only its typed value, parent
history receives no child items, `goto` reuses the invocation result type but
starts a fresh task transcript, invalid outcomes consume turn budget, unknown
tools never invoke the registry, and a terminal tool failure yields
`tool_failed`.

- [ ] **Step 7: Run focused and cumulative gates**

Run:

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test sequential_runtime
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 8: Commit the sequential runtime**

```bash
git add src/engine tests/support tests/sequential_runtime.rs
git commit -m "feat: execute durable model and tool turns"
```

## Task 10: Execute Structured Parallelism, Human Answers, and Cancellation

**Files:**

- Modify: `src/engine/scheduler.rs`
- Modify: `src/engine/turn.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/store/postgres.rs`
- Create: `tests/structured_runtime.rs`

**Interfaces:**

```rust
impl Engine {
    pub async fn answer(
        &self,
        run_id: RunId,
        suspension_id: SuspensionId,
        answer: serde_json::Value,
        idempotency_key: IdempotencyKey,
    ) -> Result<RunView, EngineError>;

    pub async fn cancel(
        &self,
        run_id: RunId,
        idempotency_key: IdempotencyKey,
        reason: Option<String>,
    ) -> Result<RunView, EngineError>;
}
```

- Consumes atomic frame commands and the existing scheduler.
- Produces bounded child concurrency, deterministic joins, multiple open human
  suspensions, and cooperative cancellation.

- [ ] **Step 1: Write a failing out-of-order parallel completion test**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn parallel_children_join_in_source_order_not_finish_order(pool: sqlx::PgPool) {
    let runtime = parallel_runtime(pool, CompletionOrder::MetricsThenLogs).await;
    let run = runtime.start_support().await;
    runtime.engine.tick_until_idle().await.unwrap();
    assert_eq!(runtime.fake_model.compose_arguments(), json!({
        "ticket": "T-1",
        "logs": {"source": "logs", "summary": "L", "references": []},
        "metrics": {"source": "metrics", "summary": "M", "references": []}
    }));
    assert_eq!(runtime.store.get_run(run.id).await.unwrap().status, RunStatus::Succeeded);
}
```

- [ ] **Step 2: Run the parallel test and verify join scheduling fails**

Run: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test structured_runtime parallel_children_join_in_source_order_not_finish_order -- --exact`

Expected: FAIL because parallel children are persisted but not yet driven and
joined by the engine.

- [ ] **Step 3: Drive child frames under the global semaphore**

Wake the scheduler after `StartCall` or `StartParallel`. Child frames use the
same semaphore and execution path as root frames. When a child becomes terminal,
lock all frames sharing its `join_id`; resume the parent only when every branch
is terminal, binding successful results by ascending `branch_index`.

- [ ] **Step 4: Implement failure propagation without detached work**

On first branch failure, atomically cancel sibling frames still `runnable` or
`waiting_for_answer` and set `cancel_requested` on siblings already
`waiting_for_model` or `waiting_for_tool`. Do not abort their external futures.
Record late results, mark those frames cancelled without further transitions,
and fail the join only after every branch is terminal. Add tests for failure
before sibling start, failure while sibling tool runs, and no compensation of a
completed sibling effect.

- [ ] **Step 5: Write failing multiple-ask and idempotent-answer tests**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn parallel_asks_suspend_independently_and_resume_once(pool: sqlx::PgPool) {
    let runtime = two_question_runtime(pool).await;
    let run = runtime.start().await;
    runtime.engine.tick_until_idle().await.unwrap();
    let waiting = runtime.store.get_run(run.id).await.unwrap();
    assert_eq!(waiting.status, RunStatus::WaitingForAnswer);
    assert_eq!(waiting.suspensions.len(), 2);
    let first = waiting.suspensions[0].id;
    runtime.engine.answer(run.id, first, json!("yes"), key("answer-1")).await.unwrap();
    runtime.engine.answer(run.id, first, json!("yes"), key("answer-1")).await.unwrap();
    assert!(runtime.engine.answer(run.id, first, json!("no"), key("answer-2")).await.is_err());
}
```

- [ ] **Step 6: Wire answer/cancel methods and scheduler wakeups**

Validate answers against stored schemas before opening a transaction. Preserve
every open suspension in `RunView`. Wake the engine after a new answer. Make
cancel idempotency scoped to run ID and key by recording the first cancellation
key and request hash in run failure metadata; same request returns the same
view, different request conflicts.

- [ ] **Step 7: Test cooperative cancellation around in-flight work**

Use a fake tool blocked on `Notify`: start it, cancel the run, release it, and
assert its result is recorded while the run remains `cancelled`, no child or
parent advances, and the effect is not compensated or reinvoked.

- [ ] **Step 8: Run focused and cumulative gates**

Run:

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test structured_runtime
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 9: Commit structured runtime behavior**

```bash
git add src/engine src/store/postgres.rs tests/structured_runtime.rs
git commit -m "feat: add structured parallel and human suspension"
```

## Task 11: Recover Interrupted Calls and Enforce Budgets

**Files:**

- Create: `src/engine/recovery.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/engine/scheduler.rs`
- Modify: `src/engine/turn.rs`
- Modify: `src/store/models.rs`
- Modify: `src/store/postgres.rs`
- Create: `tests/recovery.rs`

**Interfaces:**

```rust
pub struct RecoveryReport {
    pub model_attempts_requeued: u64,
    pub tools_recovered: u64,
    pub tools_requeued: u64,
    pub outcomes_unknown: u64,
}

impl Engine {
    pub async fn recover_startup(&self) -> Result<RecoveryReport, EngineError>;
}
```

- Consumes persisted waiting states and tool recovery declarations.
- Produces deterministic startup recovery, capped retry behavior, stable
  failures, and all deployment/workflow ceilings.

- [ ] **Step 1: Write failing interrupted-model recovery test**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn startup_requeues_model_intent_without_applying_unstored_output(pool: sqlx::PgPool) {
    let runtime = runtime_with_waiting_model(pool).await;
    let report = runtime.engine.recover_startup().await.unwrap();
    assert_eq!(report.model_attempts_requeued, 1);
    let frame = runtime.store.frame(runtime.frame_id).await.unwrap();
    assert_eq!(frame.state, FrameStatus::Runnable);
    assert!(frame.active_attempt_id.is_none());
    assert_eq!(runtime.store.model_items(frame.id).await.unwrap().len(), 1);
}
```

- [ ] **Step 2: Run recovery tests and verify startup recovery is missing**

Run: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test recovery startup_requeues_model_intent_without_applying_unstored_output -- --exact`

Expected: FAIL because `recover_startup` does not exist.

- [ ] **Step 3: Implement model and read-only recovery**

In one transaction per frame, clear interrupted model attempt identity, return
the frame to `runnable`, and append `model.interrupted`; never append a response
item. Read-only interrupted tools return to `pending` and are safe to execute
again with the same idempotency key.

- [ ] **Step 4: Write the effect recovery matrix**

Create fake effectful tools for every `ToolStatus` and assert:

| Recovery declaration/status | Required action |
|---|---|
| idempotent / any interrupted state | invoke again with the same key |
| status / `Succeeded(output)` | validate and commit output without invoke |
| status / `Failed` | record definitive `tool_failed` |
| status / `NotStarted` | invoke once |
| status / `Running` | retain waiting state and poll with capped delay |
| status / `Unknown` or status error after retry budget | `outcome_unknown` |
| effectful / unsupported status and non-idempotent | `outcome_unknown` |

- [ ] **Step 5: Implement status-first effect recovery**

Call `Tool::status` outside a transaction after loading the durable intent.
Apply the matrix through revision-checked store methods. On
`outcome_unknown`, mark tool, frame, and run consistently and append one
metadata-only event. Never schedule that frame again; cancellation remains
available.

- [ ] **Step 6: Implement retry delay and error classification**

Use capped exponential delay `min(base_delay * 2^(attempt-1), max_delay)` plus
deterministic jitter derived from SHA-256 of attempt identity so tests need no
random clock. Retry only transient model HTTP errors and transient tool/status
errors. Terminal provider protocol errors become `model_unavailable` or
`model_output_invalid` according to whether a valid completed response arrived.

- [ ] **Step 7: Write and enforce every budget**

Add tests for source/compiled size at publication; steps, model turns, tool
calls, parallel branches, response-item bytes, model/tool timeout, concurrent
branches, and active execution time. Assert workflow limits may lower but not
raise deployment ceilings. Store active elapsed time around external calls,
exclude durable human waiting, and charge at most one configured call timeout
when startup recovers an interrupted call so process downtime is not counted.
Map every exhausted budget to `limit_exceeded` without transcript compaction.

- [ ] **Step 8: Verify registry drift blocks readiness and active runs**

Change one registered descriptor after publishing, call recovery, and assert
the affected run does not advance and readiness reports the exact missing/hash
mismatch. Restoring the captured descriptor makes recovery and readiness pass;
no compatible substitution is accepted.

- [ ] **Step 9: Run focused and cumulative gates**

Run:

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test recovery
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 10: Commit recovery and limits**

```bash
git add src/engine src/store tests/recovery.rs
git commit -m "feat: recover interrupted work and enforce limits"
```

## Task 12: Expose the HTTP API, SSE Events, and Service Lifecycle

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/lib.rs`
- Replace: `src/main.rs`
- Create: `src/config.rs`
- Create: `src/api/mod.rs`
- Create: `src/api/types.rs`
- Create: `src/api/sse.rs`
- Create: `tests/api.rs`

**Interfaces:**

```rust
pub struct AppState {
    pub store: std::sync::Arc<PostgresStore>,
    pub engine: std::sync::Arc<Engine>,
    pub tools: std::sync::Arc<ToolRegistry>,
    pub ceilings: Limits,
    pub events: std::sync::Arc<tokio::sync::Notify>,
    pub readiness: std::sync::Arc<Readiness>,
}

pub struct Readiness {
    ready: std::sync::atomic::AtomicBool,
    reason: tokio::sync::RwLock<Option<String>>,
}

pub struct ReadinessView {
    pub ready: bool,
    pub reason: Option<String>,
}

impl Readiness {
    pub async fn mark_ready(&self);
    pub async fn mark_not_ready(&self, reason: String);
    pub async fn snapshot(&self) -> ReadinessView;
}

pub fn router(state: AppState) -> axum::Router;

pub struct ServiceConfig {
    pub bind_addr: std::net::SocketAddr,
    pub database_url: Secret,
    pub responses: ResponsesClientConfig,
    pub limits: Limits,
    pub engine: EngineConfig,
}

pub struct ConfigError {
    pub key: &'static str,
    pub message: String,
}

impl ServiceConfig {
    pub fn from_env() -> Result<Self, ConfigError>;
}
```

- Consumes compiler, store, engine, registry, and provider client.
- Produces exactly the approved endpoints and one executable startup/shutdown
  path. No tool, provider, tenant, or policy administration API is added.

- [ ] **Step 1: Add service dependencies**

```toml
axum = "0.8"
futures-util = "0.3"
tower-http = { version = "0.6", features = ["trace"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

[dev-dependencies]
http-body-util = "0.1"
tower = { version = "0.5", features = ["util"] }
```

- [ ] **Step 2: Write failing router tests for the complete API**

```rust
#[sqlx::test(migrations = "./migrations")]
async fn publish_start_ask_answer_and_get_run_round_trip(pool: sqlx::PgPool) {
    let app = test_router(pool).await;
    let published = post_json(&app, "/v1/workflows", json!({"source": support_source()}), None).await;
    assert_eq!(published.status(), 201);
    let started = post_json(
        &app,
        "/v1/workflows/support/runs",
        json!({"input": {"ticket": "T-1"}}),
        Some(("Idempotency-Key", "run-1")),
    ).await;
    assert_eq!(started.status(), 202);
    let run_id = json_body(started).await["run_id"].as_str().unwrap().to_owned();
    drive_to_question(&app, &run_id).await;
    let waiting = get_json(&app, &format!("/v1/runs/{run_id}")).await;
    assert_eq!(waiting["status"], "waiting_for_answer");
}
```

- [ ] **Step 3: Run API tests and verify routes are missing**

Run: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test api publish_start_ask_answer_and_get_run_round_trip -- --exact`

Expected: FAIL because `api::router` does not exist.

- [ ] **Step 4: Implement exact JSON endpoints and stable errors**

Register only:

```text
POST /v1/workflows
POST /v1/workflows/{name}/runs
GET  /v1/runs/{run_id}
GET  /v1/runs/{run_id}/events
POST /v1/runs/{run_id}/answers
POST /v1/runs/{run_id}/cancel
GET  /healthz
GET  /readyz
```

Use `201` for new publication, `200` for idempotent publication replay, `202`
for start/answer/cancel, `400` for syntax/type input, `404` for unknown IDs,
`409` for idempotency or state conflicts, `422` for valid JSON that violates a
workflow schema, `503` for definition/provider/tool readiness, and `500` only
for internal failures. Every error body is `{ "code", "message",
"diagnostics"? }` and contains no secrets or model/tool content.

- [ ] **Step 5: Implement durable JSON/SSE event reads**

For normal Accept headers, query `events_after(run_id, after, limit)` and return
JSON. For `text/event-stream`, use `futures_util::stream::unfold` over
`(store, run_id, cursor, Notify)`; emit persisted events with SSE `id` equal to
the database sequence. When caught up, use `tokio::select!` between
`Notify::notified()` and a 15-second keepalive timer, then query PostgreSQL
again. Every API or engine path that commits events calls `notify_waiters`
after commit. The timer covers a notification race or process-local missed
wake. Reconnection with `after` is exclusive and never depends on an in-memory
broadcast buffer.

- [ ] **Step 6: Test route method rejection, idempotency, SSE, and redaction**

Use `tower::ServiceExt::oneshot` and `http_body_util::BodyExt` to test every
route, missing/wrong `Idempotency-Key`, explicit/latest definition versions,
duplicate answers, conflicting cancellation, JSON event pagination, SSE IDs and
reconnect, health/readiness, body limits, and 404/405 behavior. Capture tracing
output and assert prompt, API key, tool arguments/results, and answer strings do
not appear.

- [ ] **Step 7: Implement environment configuration and service composition**

Read and validate:

```text
KITEFRAME_BIND_ADDR                 default 127.0.0.1:8080
KITEFRAME_DATABASE_URL             required
KITEFRAME_RESPONSES_BASE_URL        required
KITEFRAME_RESPONSES_API_KEY         optional
KITEFRAME_RESPONSES_HEADERS_JSON    optional JSON object of string headers
KITEFRAME_MODEL                     required
KITEFRAME_MODEL_TIMEOUT_SECONDS     default 60
KITEFRAME_MAX_CONCURRENT_FRAMES     default 8
KITEFRAME_LOG                       default info
```

`main` initializes JSON tracing, builds an empty trusted registry through a
single `build_tool_registry()` function, connects to PostgreSQL, acquires one
fixed session-level advisory lock through `try_acquire_single_process_guard`,
and exits before migration if another service process holds it. It then
migrates, constructs the Responses client/engine/router, runs `recover_startup`
before setting readiness, starts the scheduler, binds
`tokio::net::TcpListener`, and uses
`axum::serve(...).with_graceful_shutdown(...)`. Keep `SingleProcessGuard` alive
through shutdown. Add a test proving a second guard fails until the first is
dropped. Shutdown stops scheduling, waits up to the configured model/tool
timeout for in-flight calls to settle, then exits without rewriting their
durable intents.

- [ ] **Step 8: Run focused and cumulative gates**

Run:

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test api
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests PASS.

- [ ] **Step 9: Commit the service API**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/main.rs src/config.rs src/api tests/api.rs
git commit -m "feat: expose durable workflow service API"
```

## Task 13: Prove Restart Safety and Finish the Replacement

**Files:**

- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-07-25-kiteframe-declarative-agent-harness-design.md`
- Modify: `docs/superpowers/specs/2026-08-04-kiteframe-v1-design-state.md`
- Modify: `docs/superpowers/specs/2026-08-05-kiteframe-capability-control-and-mcp-design.md`
- Modify: `docs/superpowers/specs/2026-08-11-kiteframe-minimal-workflow-engine-design.md`
- Modify: all pre-replacement files under `docs/superpowers/plans/` except this
  plan
- Modify: `tests/support/responses_server.rs`
- Modify: `tests/support/fake_tools.rs`
- Create: `tests/e2e_restart.rs`

**Interfaces:**

- Consumes the complete service and a PostgreSQL 16 test database.
- Produces the normative end-to-end acceptance evidence, process-restart
  harness, final CI gates, accurate README, and supersession banners.

- [ ] **Step 1: Write the failing child-process acceptance test**

`tests/e2e_restart.rs` has one ignored child entrypoint and normal parent tests.
The parent obtains `std::env::current_exe()`, starts the same test binary with
`--ignored --exact child_service`, and supplies database/provider/control
addresses through test-scoped environment variables. Derive the isolated SQLx
test database name with `SELECT current_database()` and replace the database
path in the parent `DATABASE_URL` before passing it to the child. The child
registers two fake read tools and one effect tool whose idempotency results
persist in a test-only PostgreSQL table, then runs the real router and engine.

```rust
#[test]
#[ignore]
fn child_service() {
    if std::env::var_os("KITEFRAME_E2E_CHILD").is_none() { return; }
    run_child_service();
}

#[sqlx::test(migrations = "./migrations")]
async fn full_workflow_survives_process_restarts(pool: sqlx::PgPool) {
    let provider = ScriptedResponsesServer::start(full_support_script()).await;
    let mut child = ChildService::spawn(&pool, &provider).await;
    let run = publish_and_start(&child, support_source()).await;

    wait_for_frame_state(&pool, run, "waiting_for_model").await;
    child.kill_and_wait();
    child = ChildService::spawn(&pool, &provider).await;

    wait_for_effect_committed_before_result(&pool, run).await;
    child.kill_and_wait();
    child = ChildService::spawn(&pool, &provider).await;

    wait_for_run_state(&pool, run, "waiting_for_answer").await;
    child.kill_and_wait();
    child = ChildService::spawn(&pool, &provider).await;
    answer(&child, run, "approved", "answer-1").await;

    assert_eq!(wait_for_terminal(&pool, run).await.status, RunStatus::Succeeded);
}
```

- [ ] **Step 2: Run the acceptance test and verify the process harness fails**

Run: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test e2e_restart full_workflow_survives_process_restarts -- --exact --nocapture`

Expected: FAIL because the child-process harness and scripted acceptance
workflow are not implemented.

- [ ] **Step 3: Implement the scripted provider and process controls**

The provider records each request and returns responses keyed by task prompt and
persisted item history. It deliberately blocks the first model response until
the parent kills the child, then allows the retried attempt. The effect tool
inserts `(idempotency_key, output)` before blocking its first return; after
restart, `status` returns that exact output and the invoke count remains one.
Use ephemeral TCP ports passed explicitly to the child and bound polling with a
30-second test deadline.

- [ ] **Step 4: Exercise the complete accepted workflow**

The single scenario must prove: publish, idempotent start, ordinary model/tool
loop, task-local transcript isolation, two parallel children completing out of
order, source-order join, model-attempt restart, effect-status recovery without
reinvocation, durable `ask`, restart while suspended, idempotent answer, final
typed result, and SSE replay from sequence 0 through terminal event.

Add a second scenario whose effect commits but returns `ToolStatus::Unknown`;
assert run/frame/tool all become `outcome_unknown`, no retry occurs, and cancel
remains idempotent.

- [ ] **Step 5: Update CI with PostgreSQL and final gates**

Add this service to the Rust job:

```yaml
services:
  postgres:
    image: postgres:16-alpine
    env:
      POSTGRES_PASSWORD: postgres
    ports:
      - 5432:5432
    options: >-
      --health-cmd "pg_isready -U postgres"
      --health-interval 5s
      --health-timeout 5s
      --health-retries 10
env:
  DATABASE_URL: postgres://postgres:postgres@localhost:5432/postgres
```

Keep the three Rust gates from Task 1 and add the explicit restart test with
`--nocapture`. Remove every Python, PyO3, Deep Agents, OpenFGA, schema-generation,
and old fuzz gate.

- [ ] **Step 6: Make documentation describe only verified behavior**

README includes prerequisites, environment variables, `cargo run`, workflow
publication/start/answer/cancel curl examples, SSE reconnection, tool
registration in trusted Rust code, failure taxonomy, and the exact verification
commands. It labels one-process/PostgreSQL and portable Responses limitations.

Add this banner immediately below the title of every older spec and plan:

```markdown
> **Superseded:** Historical design/implementation evidence only. The current
> contract is `2026-08-11-kiteframe-minimal-workflow-engine-design.md` and the
> replacement plan is `2026-08-11-kiteframe-minimal-workflow-engine.md`.
```

Do not alter historical body text or commits.

- [ ] **Step 7: Run the complete verification matrix**

Run:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --all-targets --all-features
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test e2e_restart -- --nocapture
git diff --check
```

Expected: formatting PASS, Clippy reports zero warnings, every test PASS,
restart scenarios PASS, and the diff check reports no whitespace errors.

- [ ] **Step 8: Audit the clean-replacement boundary**

Now that the complete verification matrix has passed, change the current design
status from `Approved; implementation not started` to
`Implemented and verified` and record the Task 12 service implementation commit
hash; the immediately following Task 13 commit is the verification evidence in
Git history.

Run:

```bash
test ! -e crates
test ! -e python
test ! -e fuzz
test ! -e openfga
test ! -e runtime-targets
test ! -e schemas
test ! -e scripts
! rg -n "deepagents|langgraph|langchain|pyo3|openfga" Cargo.toml src tests .github README.md
git diff --check
git status --short
```

Expected: all path assertions PASS; the search returns no runtime/dependency
references (README historical links are replaced); only intended replacement
paths are staged and unrelated `site/` remains untouched.

- [ ] **Step 9: Commit the verified V1 replacement**

```bash
git add .github/workflows/ci.yml README.md tests/e2e_restart.rs tests/support docs/superpowers/specs docs/superpowers/plans
git commit -m "test: prove minimal kiteframe v1 end to end"
```

## Spec Coverage Map

| Approved design requirement | Implemented and proved by |
|---|---|
| One Rust monolith; old architecture removed | Task 1 shape test and Task 13 boundary audit |
| Closed types, trusted tools, descriptor identity | Task 2 |
| Author-authored syntax and six operations | Tasks 3 and 8 |
| Immutable publication, graph/type validation, exact hash | Tasks 4 and 6 |
| Portable `store: false` Responses protocol | Task 5 and Task 13 scripted provider |
| PostgreSQL sole authority and six logical tables | Tasks 6 and 7 |
| Durable model/tool intent and ordered item history | Tasks 7 and 9 |
| Sequential `call`, `goto`, `done`, `fail` | Tasks 8 and 9 |
| Structured `parallel`, source-order joins, bounded children | Task 10 |
| Multiple durable `ask` suspensions and idempotent resume | Tasks 7, 10, and 12 |
| Cooperative cancellation | Tasks 7, 10, and 12 |
| Status-first recovery and `outcome_unknown` | Task 11 and Task 13 |
| Deployment/workflow budgets and stable failures | Task 11 |
| Exact HTTP API, JSON events, SSE cursor reconnect | Task 12 |
| Real process restart through model/tool/parallel/ask states | Task 13 |
| Metadata-only logs and trusted deployment boundary | Tasks 5, 9, 12, and 13 |
| Historical docs marked superseded | Task 13 |

## Plan Completion Criteria

Implementation is complete only when all task checkboxes are checked, every
task commit exists in order, the Task 13 verification matrix passes from a
fresh PostgreSQL database, the process-restart acceptance tests cover every
required crash boundary, current documentation contains no unverified behavior
claim, and unrelated `site/` work remains untouched.
