# Kiteframe Declarative Agents Slice 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the reviewed, NXL-shaped `agent` language surface, nested tasks,
typed traversal metadata, and an explicit deprecation diagnostic for legacy
`workflow` sources without changing the behavior of any published workflow.

**Architecture:** The parser produces a tagged `DefinitionAst` for either a
legacy workflow or a new agent. Both compile through one normalized graph into
the existing `CompiledWorkflow` execution shape, augmented with definition-kind
and agent metadata. Slice 1 is deliberately a compiler boundary: legacy
workflows remain publishable and executable, while compiled agents are rejected
at publication with `agent_runtime_unavailable` until the durable task, goal,
and runtime-authority slices are complete.

**Tech Stack:** Rust 1.97.1 / edition 2024, Serde/serde_json 1, existing
Kiteframe recursive-descent parser and compiler, Cargo test fixtures, Python
3.11+ and TypeScript 5.9 only for compatibility checks; no new dependencies.

**Spec:**
`docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md`

## Global Constraints

- The approved design is normative. This plan implements only cumulative V1
  Slice 1; write a new task-level plan before starting Slice 2.
- Preserve every currently valid `workflow` source, compiled JSON contract,
  store row, engine transition, ACP command, and Python/TypeScript client call.
- `workflow` is deprecated, not a textual alias. It cannot declare agent
  events, semantic model roles, or context modes.
- New `agent` definitions are parseable and compileable but MUST NOT be
  publishable or executable in this slice. Return the stable failure
  `agent_runtime_unavailable`; do not silently run one as a workflow.
- Use `dsl_version = 1` for legacy workflows and `dsl_version = 2` for agents.
  Preserve deserialization of existing version-1 compiled fixtures.
- `context isolated` is the default on agent task edges. `context kept` is
  represented in the compiled graph but has no runtime authority until Slice 4.
- `model reasoning` and `model coding` are semantic roles only. Do not add
  provider, model, credential, endpoint, or subscription fields.
- Agent event declarations are versioned semantic names represented as quoted
  strings. Slice 1 validates and freezes them but does not register channels or
  wait for delivery; that belongs to Slice 8.
- Preserve the current bounded-source, token, identifier, nesting, and
  diagnostic limits. Warnings consume the same bounded diagnostic budget.
- Definition identity includes definition kind, normalized agent metadata, the
  exact source, and immutable tool descriptors.
- No new database migration, plugin process, connector, IDE extension, goal
  lifecycle, budget, lease, or service-monitor behavior belongs in this plan.
- Use TDD. Every task starts with the named failing test, implements the minimum
  contract, runs focused and cumulative gates, then commits only listed paths.
- Preserve unrelated `.worktrees/`, `fuzz/`, and `site/` content.

---

## Reviewed Grammar Contract

The grammar review in Task 1 freezes this concrete Slice 1 spelling:

```kite
agent babysit_pr(repository: string, pull_request: integer) -> string {
  entry inspect_pr(repository = repository, pull_request = pull_request)

  events [
    "github.pull_request.changed.v1",
    "github.review.submitted.v1",
    "github.required_checks.changed.v1"
  ]

  limits {
    steps: 200
    model_turns_per_task: 12
    parallel_branches: 4
    active_execution_seconds: 300
    elapsed_seconds: 604800
  }

  task inspect_pr(repository: string, pull_request: integer) -> string {
    model reasoning
    prompt """Inspect the pull request and select its next safe action."""
    tools [github_context]

    on repair(review_ids: [string]) {
      result = call repair_reviews(review_ids = review_ids) context kept
      done result
    }

    on refresh() {
      return inspect_pr(repository = repository, pull_request = pull_request)
        context isolated
    }

    on compare(left: string, right: string) {
      fork {
        first = call inspect_branch(name = left) context isolated
        second = call inspect_branch(name = right) context isolated
      } join
      done first
    }
  }

  task repair_reviews(review_ids: [string]) -> string {
    model coding
    prompt """Address only the supplied actionable review threads."""
    tools []
    on repaired(commit: string) { done commit }
  }

  task inspect_branch(name: string) -> string {
    model reasoning
    prompt """Inspect one branch."""
    tools []
    on inspected(summary: string) { done summary }
  }
}
```

The parser maps `return` to the existing tail-transition operation and
`fork { ... } join` to the existing structured parallel operation. Legacy
`goto` and `parallel` remain available only in `workflow` definitions. `done`,
`fail`, and `ask` retain their current semantics in both definition kinds.

## Final File Structure

```text
docs/reference/kite-language.md
docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-1.md
examples/babysit_pr.kite
src/dsl/
  ast.rs
  compiler.rs
  diagnostic.rs
  ir.rs
  lexer.rs
  mod.rs
  parser.rs
src/protocol/application.rs
src/protocol/types.rs
tests/
  agent_compiler.rs
  agent_parser.rs
  dsl_compiler.rs
  dsl_parser.rs
  fixtures/dsl/agent_babysit_pr.compiled.json
  fixtures/dsl/agent_babysit_pr.kite
  fixtures/dsl/support.compiled.json
  fixtures/dsl/support.kite
  repository_shape.rs
```

## Dependency Flow

```text
grammar fixture
  -> lexer + tagged AST
  -> parser report + nested-agent parser
  -> normalized compiler + versioned IR
  -> publication fail-closed guard
  -> compatibility, golden, and documentation gates
```

---

## Task 1: Freeze the Slice 1 Grammar Before Parser Work

**Files:**

- Create: `docs/reference/kite-language.md`
- Create: `tests/fixtures/dsl/agent_babysit_pr.kite`
- Create: `tests/agent_parser.rs`

**Interfaces:** The reference defines the accepted `agent` and legacy
`workflow` productions, reserved words, nesting rules, diagnostic codes, and
compile-only publication boundary.

- [ ] **Step 1: Add a failing fixture-presence test**

```rust
use std::fs;

#[test]
fn reviewed_agent_grammar_and_fixture_are_checked_in() {
    let reference = fs::read_to_string("docs/reference/kite-language.md")
        .expect("language reference exists");
    let fixture = fs::read_to_string("tests/fixtures/dsl/agent_babysit_pr.kite")
        .expect("agent fixture exists");

    assert!(reference.contains("agent-definition"));
    assert!(reference.contains("workflow-definition (deprecated)"));
    assert!(fixture.starts_with("agent babysit_pr("));
}
```

- [ ] **Step 2: Run the test and observe the missing-artifact failure**

Run: `cargo test --test agent_parser reviewed_agent_grammar_and_fixture_are_checked_in -- --exact`

Expected: FAIL because `docs/reference/kite-language.md` and the fixture do not
exist.

- [ ] **Step 3: Write the grammar reference and canonical fixture**

The reference MUST define these productions exactly:

```text
source              = { record-definition }, definition ;
definition          = workflow-definition | agent-definition ;
agent-definition    = "agent", identifier, fields, "->", type, "{",
                      entry, [ events ], [ limits ], task-definition,
                      { task-definition }, "}" ;
entry               = "entry", invocation ;
events              = "events", "[", string, { [ "," ], string }, "]" ;
task-definition     = "task", identifier, fields, "->", type, "{",
                      model-role, prompt, tools, outcome,
                      { outcome }, "}" ;
model-role          = "model", ( "reasoning" | "coding" ) ;
call-statement      = identifier, "=", "call", invocation,
                      [ context-mode ] ;
return-statement    = "return", invocation, [ context-mode ] ;
fork-statement      = "fork", "{", call-statement,
                      { call-statement }, "}", "join" ;
context-mode        = "context", ( "isolated" | "kept" ) ;
workflow-definition = (* existing grammar; deprecated *) ;
```

Also state that semantic event names match
`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+\.v[1-9][0-9]*$`, agent names and task names
use the existing identifier rules, and commas remain optional where the current
grammar already permits them.

- [ ] **Step 4: Run the focused artifact test**

Run: `cargo test --test agent_parser reviewed_agent_grammar_and_fixture_are_checked_in -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit the review boundary**

```bash
git add docs/reference/kite-language.md tests/fixtures/dsl/agent_babysit_pr.kite tests/agent_parser.rs
git commit -m "docs: freeze declarative agent grammar"
```

---

## Task 2: Add Tagged Definitions and Bounded Diagnostic Reports

**Files:**

- Modify: `src/dsl/ast.rs`
- Modify: `src/dsl/compiler.rs`
- Modify: `src/dsl/diagnostic.rs`
- Modify: `src/dsl/lexer.rs`
- Modify: `src/dsl/mod.rs`
- Modify: `src/dsl/parser.rs`
- Modify: `tests/dsl_parser.rs`
- Modify: `tests/agent_parser.rs`

**Interfaces:**

```rust
pub enum DefinitionAst {
    Workflow(WorkflowAst),
    Agent(AgentAst),
}

pub struct SourceFile {
    pub span: Span,
    pub records: Vec<RecordAst>,
    pub definition: DefinitionAst,
}

// Add this field to the existing WorkflowAst.
pub tasks: Vec<TaskAst>,

pub struct AgentAst {
    pub name: String,
    pub inputs: Vec<FieldAst>,
    pub result: TypeAst,
    pub entry: CallAst,
    pub events: Vec<EventDeclarationAst>,
    pub limits: BTreeMap<String, u64>,
    pub tasks: Vec<TaskAst>,
    pub span: Span,
}

pub enum DiagnosticSeverity { Warning, Error }

pub struct ParseReport {
    pub source: SourceFile,
    pub diagnostics: Vec<Diagnostic>,
}
```

- [ ] **Step 1: Add failing tagged-AST and warning tests**

```rust
use kiteframe::dsl::{DefinitionAst, DiagnosticSeverity, parse_report};

#[test]
fn legacy_workflow_parses_with_a_bounded_deprecation_warning() {
    let report = parse_report(SUPPORT, limits()).expect("workflow remains valid");
    assert!(matches!(report.source.definition, DefinitionAst::Workflow(_)));
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(report.diagnostics[0].code, "deprecated_workflow");
}
```

- [ ] **Step 2: Run both parser suites and observe unresolved symbols**

Run: `cargo test --test dsl_parser --test agent_parser`

Expected: FAIL because `DefinitionAst`, `DiagnosticSeverity`, and
`parse_report` do not exist.

- [ ] **Step 3: Implement tagged source and diagnostics without breaking `parse`**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub span: Span,
}

pub fn parse(source: &str, limits: ParseLimits) -> Result<SourceFile, Vec<Diagnostic>> {
    parse_report(source, limits).map(|report| report.source)
}
```

Use constructors `Diagnostic::error(code, message, span)` and
`Diagnostic::warning(code, message, span)`. Convert every current parser and
lexer diagnostic to stable snake-case codes; tests continue asserting the
human-readable message where they already do so. Keep `Diagnostic::new` as a
temporary compiler-error constructor so parser refactoring does not expand
into compiler diagnostic naming.

- [ ] **Step 4: Move legacy top-level tasks into `WorkflowAst`**

`SourceFile` owns records plus one tagged definition. The parser continues to
read legacy top-level tasks, then attaches them to `WorkflowAst.tasks`. Update
the compiler to destructure `DefinitionAst::Workflow`; return the compile
diagnostic `agent_compiler_unavailable` for the agent tag until Task 6.

- [ ] **Step 5: Change existing AST consumers from `source.workflow` to the tag**

```rust
let DefinitionAst::Workflow(workflow) = &source.definition else {
    panic!("support fixture is a workflow");
};
assert_eq!(workflow.name, "support");
```

- [ ] **Step 6: Run focused and cumulative parser gates**

Run: `cargo test --test dsl_parser --test agent_parser`

Expected: PASS with exactly one deprecation warning in `parse_report`; the
compatibility `parse` API returns the AST and discards non-fatal warnings.

- [ ] **Step 7: Commit**

```bash
git add src/dsl/ast.rs src/dsl/compiler.rs src/dsl/diagnostic.rs src/dsl/lexer.rs src/dsl/mod.rs src/dsl/parser.rs tests/dsl_parser.rs tests/agent_parser.rs
git commit -m "refactor: tag kite definition syntax"
```

---

## Task 3: Parse Nested Agent Tasks and Agent-Only Declarations

**Files:**

- Modify: `src/dsl/ast.rs`
- Modify: `src/dsl/parser.rs`
- Modify: `tests/agent_parser.rs`

**Interfaces:** `parse_report` accepts exactly one top-level definition. Agent
tasks occur only inside the agent body. Workflow tasks retain their existing
top-level location.

- [ ] **Step 1: Add failing canonical-agent assertions**

```rust
const BABYSIT: &str = include_str!("fixtures/dsl/agent_babysit_pr.kite");

#[test]
fn parses_nested_agent_tasks_roles_events_and_elapsed_limit() {
    let report = parse_report(BABYSIT, limits()).expect("reviewed agent parses");
    assert!(report.diagnostics.is_empty());
    let DefinitionAst::Agent(agent) = report.source.definition else {
        panic!("fixture must be an agent");
    };
    assert_eq!(agent.name, "babysit_pr");
    assert_eq!(agent.entry.task, "inspect_pr");
    assert_eq!(agent.events[0].event_type, "github.pull_request.changed.v1");
    assert_eq!(agent.limits["elapsed_seconds"], 604_800);
    assert_eq!(agent.tasks.len(), 3);
    assert_eq!(agent.tasks[0].model_role, Some(ModelRoleAst::Reasoning));
}
```

- [ ] **Step 2: Run the canonical parser test and observe failure**

Run: `cargo test --test agent_parser parses_nested_agent_tasks_roles_events_and_elapsed_limit -- --exact`

Expected: FAIL because the parser only accepts top-level workflows and tasks.

- [ ] **Step 3: Implement `agent`, nested `task`, `entry`, `events`, and `model` parsing**

Add these AST fields:

```rust
pub struct TaskAst {
    pub name: String,
    pub inputs: Vec<FieldAst>,
    pub result: TypeAst,
    pub model_role: Option<ModelRoleAst>,
    pub prompt: PromptAst,
    pub tools: Vec<String>,
    pub outcomes: Vec<OutcomeAst>,
    pub span: Span,
}

pub enum ModelRoleAst { Reasoning, Coding }

pub struct EventDeclarationAst {
    pub event_type: String,
    pub span: Span,
}
```

Require `model` exactly once in agent tasks and reject it in workflow tasks.
Accept `elapsed_seconds` only in agent limits. Keep all current workflow limit
messages and ceilings unchanged.

- [ ] **Step 4: Add negative cardinality and separation tests**

Cover duplicate/missing `entry`, duplicate `events`, duplicate event names,
zero tasks, nested `agent`, top-level task beside an agent, model-less agent
task, `model` in a workflow task, and `elapsed_seconds` in a workflow.

- [ ] **Step 5: Run parser gates**

Run: `cargo test --test agent_parser --test dsl_parser`

Expected: PASS; hostile-input diagnostic counts remain within configured
limits.

- [ ] **Step 6: Commit**

```bash
git add src/dsl/ast.rs src/dsl/parser.rs tests/agent_parser.rs
git commit -m "feat: parse nested declarative agents"
```

---

## Task 4: Parse NXL-Shaped Traversal and Context Modes

**Files:**

- Modify: `src/dsl/ast.rs`
- Modify: `src/dsl/parser.rs`
- Modify: `tests/agent_parser.rs`

**Interfaces:** The new surface normalizes to the existing traversal AST so the
current compiler remains exhaustive while syntax evolves.

```rust
pub enum ContextModeAst { Legacy, Isolated, Kept }

pub struct CallAst {
    pub binding: Option<String>,
    pub task: String,
    pub arguments: Vec<ArgumentAst>,
    pub context: ContextModeAst,
    pub span: Span,
}

// StatementAst remains Call, Parallel, Ask, Goto, Done, and Fail.
// Agent `fork` parses as Parallel; agent `return` parses as Goto.
```

- [ ] **Step 1: Add failing traversal-shape tests**

Assert the canonical fixture contains `Call` with `Kept`, a `Goto` parsed from
`return` with `Isolated`, and a two-branch `Parallel` parsed from `fork`. Add
invalid fixtures for `fork` without `join`, empty fork, a fork call without a
result binding, `context summarized`, legacy `goto` in an agent, and new
`return` in a workflow.

- [ ] **Step 2: Run the focused traversal tests**

Run: `cargo test --test agent_parser traversal -- --nocapture`

Expected: FAIL because only legacy `parallel` and `goto` are recognized.

- [ ] **Step 3: Implement definition-aware statement parsing**

Pass a private `DefinitionSyntax` enum into task/outcome/statement parsing.
Default every agent edge to `ContextModeAst::Isolated`; legacy calls retain an
internal `Legacy` mode so v1 hashes and JSON remain byte-for-byte stable.

- [ ] **Step 4: Preserve structured-concurrency rules**

Reuse the current terminal-operation checks: `return`, `done`, and `fail` are
terminal; `fork` and `call` are not. Reject an empty fork and any statement
after a terminal operation.

- [ ] **Step 5: Run parser gates**

Run: `cargo test --test agent_parser --test dsl_parser`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/dsl/ast.rs src/dsl/parser.rs tests/agent_parser.rs
git commit -m "feat: parse typed agent traversal"
```

---

## Task 5: Extend the Compiled IR Without Breaking Version 1

**Files:**

- Modify: `src/dsl/ir.rs`
- Modify: `src/dsl/compiler.rs`
- Create: `tests/agent_compiler.rs`
- Modify: `tests/dsl_compiler.rs`
- Create: `tests/fixtures/dsl/legacy_minimal.compiled.json`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionKind { Workflow, Agent }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode { Legacy, Isolated, Kept }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole { Reasoning, Coding }

// Add this field to CompiledTask.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub model_role: Option<ModelRole>,

pub struct CompiledCall {
    pub task: String,
    pub arguments: BTreeMap<String, CompiledValue>,
    #[serde(default, skip_serializing_if = "ContextMode::is_legacy")]
    pub context: ContextMode,
}

pub struct CompiledWorkflow {
    pub dsl_version: u8,
    #[serde(default, skip_serializing_if = "DefinitionKind::is_workflow")]
    pub definition_kind: DefinitionKind,
    pub name: String,
    pub source: String,
    pub definition_hash: String,
    pub inputs: BTreeMap<String, CompiledType>,
    pub result: CompiledType,
    pub entry: CompiledCall,
    pub limits: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    pub records: BTreeMap<String, BTreeMap<String, CompiledType>>,
    pub tasks: BTreeMap<String, CompiledTask>,
    #[serde(default, skip_deserializing)]
    pub tool_bindings: Vec<CompiledToolBinding>,
}
```

`DefinitionKind::default()` returns `Workflow`; `ContextMode::default()` returns
`Legacy`. Add optional `model_role` to `CompiledTask` with a serde default and
skip it when absent so existing version-1 serialized output is unchanged.

- [ ] **Step 1: Add failing v1 deserialization and new-IR tests**

```rust
#[test]
fn version_one_compiled_json_defaults_to_workflow_and_legacy_context() {
    let json = include_str!("fixtures/dsl/legacy_minimal.compiled.json");
    let compiled: CompiledWorkflow = serde_json::from_str(json).unwrap();
    assert_eq!(compiled.definition_kind, DefinitionKind::Workflow);
    assert_eq!(compiled.entry.context, ContextMode::Legacy);
    assert!(compiled.events.is_empty());
}
```

- [ ] **Step 2: Run compiler tests and observe missing IR types**

Run: `cargo test --test agent_compiler --test dsl_compiler`

Expected: FAIL to compile.

- [ ] **Step 3: Add serde-compatible IR fields and defaults**

Do not rename `CompiledWorkflow` in this slice: the name is internal historical
debt, while a rename would create unrelated store and engine churn. Mark it for
replacement by a tagged published-definition type in Slice 2.

Update the compiler's legacy construction path to set
`DefinitionKind::Workflow`, empty events, absent model roles, and
`ContextMode::Legacy`. Continue returning `agent_compiler_unavailable` for the
agent tag until Task 6.

- [ ] **Step 4: Freeze one complete legacy compiled fixture**

Create `tests/fixtures/dsl/legacy_minimal.compiled.json` from a no-tool legacy
workflow before adding fields, then prove deserialize/serialize does not
introduce agent-only keys. A no-tool fixture is required because tool bindings
are intentionally skipped during deserialization.

- [ ] **Step 5: Run focused tests**

Run: `cargo test --test agent_compiler --test dsl_compiler`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/dsl/compiler.rs src/dsl/ir.rs tests/agent_compiler.rs tests/dsl_compiler.rs tests/fixtures/dsl/legacy_minimal.compiled.json
git commit -m "feat: version compiled definition metadata"
```

---

## Task 6: Compile Agents Through One Normalized Graph

**Files:**

- Modify: `src/dsl/compiler.rs`
- Modify: `src/dsl/ir.rs`
- Modify: `src/dsl/mod.rs`
- Modify: `tests/agent_compiler.rs`
- Modify: `tests/dsl_compiler.rs`
- Create: `tests/fixtures/dsl/agent_babysit_pr.compiled.json`

**Interfaces:**

```rust
pub struct Compilation {
    pub definition: CompiledWorkflow,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn compile_report(
    source: &str,
    catalog: &ToolCatalogSnapshot,
    ceilings: &Limits,
) -> Result<Compilation, Vec<Diagnostic>>;

pub fn compile(
    source: &str,
    catalog: &ToolCatalogSnapshot,
    ceilings: &Limits,
) -> Result<CompiledWorkflow, Vec<Diagnostic>> {
    compile_report(source, catalog, ceilings).map(|value| value.definition)
}
```

- [ ] **Step 1: Add failing canonical compilation assertions**

```rust
#[test]
fn compiles_agent_metadata_and_reuses_graph_safety_checks() {
    let result = compile_report(BABYSIT, &catalog([tool("github/context")]), &ceilings())
        .expect("reviewed agent compiles");
    let compiled = result.definition;
    assert!(result.diagnostics.is_empty());
    assert_eq!(compiled.dsl_version, 2);
    assert_eq!(compiled.definition_kind, DefinitionKind::Agent);
    assert_eq!(compiled.events.len(), 3);
    assert_eq!(compiled.tasks["repair_reviews"].model_role, Some(ModelRole::Coding));
    assert_eq!(compiled.tasks["inspect_pr"].outcomes["repair"].statements[0]
        .call_context(), Some(ContextMode::Kept));
}
```

- [ ] **Step 2: Run the agent compiler suite and observe failure**

Run: `cargo test --test agent_compiler`

Expected: FAIL because the compiler assumes `ast.workflow` and legacy
statements.

- [ ] **Step 3: Introduce a private normalized definition view**

```rust
struct DefinitionView<'a> {
    kind: DefinitionKind,
    dsl_version: u8,
    name: &'a str,
    inputs: &'a [FieldAst],
    result: &'a TypeAst,
    entry: &'a CallAst,
    limits: &'a BTreeMap<String, u64>,
    events: Vec<&'a str>,
    tasks: &'a [TaskAst],
    span: Span,
}
```

Construct this view from either AST tag, then reuse record collection, field
checking, tool binding, reachability, recursive-call rejection, result typing,
parallel ceilings, canonical hashing, and completion-contract compilation.

- [ ] **Step 4: Compile traversal metadata without granting runtime behavior**

Map agent `Fork` to `CompiledStatement::Parallel` and `Return` to
`CompiledStatement::Goto`, retaining `ContextMode` on every `CompiledCall`.
Map legacy nodes identically with `ContextMode::Legacy`.

- [ ] **Step 5: Validate semantic event names and agent limits**

Reject malformed or repeated event types and enforce
`elapsed_seconds > active_execution_seconds`. Extend `Limits` with
`elapsed_seconds`; default it to `u64::MAX`. Workflow sources must not set it.

- [ ] **Step 6: Add the agent golden fixture**

Freeze definition kind, DSL version, entry context, events, roles, traversal
contexts, exact tool descriptors, and definition hash in
`agent_babysit_pr.compiled.json`.

- [ ] **Step 7: Run focused and cumulative compiler gates**

Run: `cargo test --test agent_compiler --test dsl_compiler`

Expected: PASS; the existing `support.compiled.json` hash is unchanged.

- [ ] **Step 8: Commit**

```bash
git add src/dsl/compiler.rs src/dsl/ir.rs src/dsl/mod.rs tests/agent_compiler.rs tests/dsl_compiler.rs tests/fixtures/dsl/agent_babysit_pr.compiled.json
git commit -m "feat: compile declarative agent graphs"
```

---

## Task 7: Fail Closed at the Publication Boundary

**Files:**

- Modify: `src/protocol/application.rs`
- Modify: `src/protocol/transport.rs`
- Modify: `src/protocol/types.rs`
- Create: `tests/agent_publication.rs`
- Modify: `tests/acp_conformance.rs`
- Modify: `tests/transports.rs`

**Interfaces:** Agent compilation is available to authoring tools, but the V1
publication command refuses it until durable task and goal authority exist.

```rust
pub const AGENT_RUNTIME_UNAVAILABLE: &str = "agent_runtime_unavailable";

// Add this variant to the existing ProtocolError enum.
StableApplication {
    code: &'static str,
    message: String,
}

impl ProtocolError {
    pub const fn stable_code(&self) -> Option<&'static str> {
        match self {
            Self::StableApplication { code, .. } => Some(code),
            _ => None,
        }
    }
}
```

- [ ] **Step 1: Add a failing application-level publication test**

```rust
#[tokio::test]
async fn publication_rejects_agent_without_creating_a_definition_row() {
    let result = application
        .dispatch(&bound_context, ApplicationCommand::Publish(agent_publish_request()))
        .await
        .expect_err("Slice 1 agents are compile-only");

    assert_eq!(result.stable_code(), Some("agent_runtime_unavailable"));
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_definitions WHERE name = $1",
    )
    .bind("babysit_pr")
    .fetch_one(&verification_pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
}
```

- [ ] **Step 2: Run the publication test and observe the unsafe current path**

Run: `cargo test --test agent_publication publication_rejects_agent_without_creating_a_definition_row -- --exact`

Expected: FAIL because publication currently passes every compiled definition
to the workflow store.

- [ ] **Step 3: Use `compile_report` and reject before opening a store mutation**

```rust
let compilation = compile_report(&request.source, &catalog, &self.limits)?;
if compilation.definition.definition_kind == DefinitionKind::Agent {
    return Err(ProtocolError::StableApplication {
        code: AGENT_RUNTIME_UNAVAILABLE,
        message: "agent definitions require durable task and goal authority".to_owned(),
    });
}
```

Do not add an agent row, version, event, or tool binding. Keep workflow
publication response bytes unchanged. Log the `deprecated_workflow` warning at
`WARN` with source span and definition name, never source content.

- [ ] **Step 4: Freeze ACP v1/v2 and remote stable-error mappings**

For ACP, map the failure to server error `-32000`, fixed message
`"application request failed"`, and data
`{"kiteframe_code":"agent_runtime_unavailable"}`. For NDJSON/WebSocket remote
transport, extend `RpcError` with optional `data` and emit the same object.
Golden assertions prove both protocol versions and every transport expose the
same stable code without leaking compiler internals or source.

- [ ] **Step 5: Run protocol and store regression gates**

Run: `cargo test --test agent_publication --test acp_conformance --test transports --test session_store`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/protocol/application.rs src/protocol/transport.rs src/protocol/types.rs tests/agent_publication.rs tests/acp_conformance.rs tests/transports.rs
git commit -m "feat: guard compile-only agent publication"
```

---

## Task 8: Prove Legacy Runtime Compatibility

**Files:**

- Modify: `tests/dsl_compiler.rs`
- Modify: `tests/sequential_runtime.rs`
- Modify: `tests/structured_runtime.rs`
- Modify: `tests/e2e_restart.rs`
- Modify: `tests/repository_shape.rs`

**Interfaces:** All existing workflow execution remains bit-for-bit compatible
at the compiled boundary and behaviorally compatible through restart.

- [ ] **Step 1: Add an explicit legacy compatibility matrix**

Test that the existing support workflow still:

- compiles with DSL version 1 and `DefinitionKind::Workflow`;
- retains its previous definition hash and compiled golden;
- executes sequential calls, structured parallel calls, asks, goto, done, and
  fail;
- resumes from the same persisted frames after restart; and
- never emits agent-only events or fields.

- [ ] **Step 2: Run the focused runtime suites before changing assertions**

Run: `cargo test --test sequential_runtime --test structured_runtime --test e2e_restart`

Expected: PASS. If any existing test fails, stop and diagnose rather than
updating expected output.

- [ ] **Step 3: Add repository-shape assertions**

```rust
assert!(root.join("docs/reference/kite-language.md").is_file());
assert!(root.join("examples/babysit_pr.kite").is_file());
assert!(!root.join("extensions").exists());
assert!(!root.join("plugins").exists());
```

The negative assertions enforce the approved incremental boundary; remove them
only in the plan that introduces those directories.

- [ ] **Step 4: Run all Rust tests**

Run: `cargo test --all-features`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/dsl_compiler.rs tests/sequential_runtime.rs tests/structured_runtime.rs tests/e2e_restart.rs tests/repository_shape.rs
git commit -m "test: prove legacy workflow compatibility"
```

---

## Task 9: Publish the Authoring Example and Close Slice 1

**Files:**

- Create: `examples/babysit_pr.kite`
- Modify: `README.md`
- Modify: `docs/reference/kite-language.md`
- Modify: `docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md`
- Modify: `tests/repository_shape.rs`

**Interfaces:** Documentation presents `agent` as an authoring preview, labels
`workflow` deprecated but supported, and states that agent publication begins
only after durable user tasks and goals land.

- [ ] **Step 1: Copy the reviewed fixture into the authoring example**

The example and test fixture MUST be byte-identical. Add a repository-shape
test comparing them so documentation cannot drift from the compiler fixture.

- [ ] **Step 2: Update README language without overstating runtime support**

Document these exact status distinctions:

```text
Implemented: workflow parser/compiler/runtime; agent parser/compiler preview.
Deprecated: new workflow authoring; existing workflow publication and execution remain supported.
Not yet implemented: durable agent goals, agent publication/execution, plugins, channels, workers, budgets, connectors, and IDE integration.
```

- [ ] **Step 3: Mark Slice 1 evidence in the design spec**

Add a non-normative implementation-status note containing the final commit and
test commands. Do not edit the approved semantics or mark later slices done.

- [ ] **Step 4: Run format, lint, compatibility, and client gates**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cd clients/python && uv run ruff check . && uv run pyright && uv run pytest
cd clients/typescript && bun run typecheck && bun test && bun run build
```

Expected: every command PASS. The client gates prove the extended compiled IR
and stable publication failure did not break thin clients.

- [ ] **Step 5: Inspect the complete slice diff**

Run: `git diff --check && git status --short && git diff --stat`

Expected: no whitespace errors; only Slice 1 paths are modified; unrelated
`.worktrees/`, `fuzz/`, and `site/` remain untouched.

- [ ] **Step 6: Commit the slice evidence**

```bash
git add README.md examples/babysit_pr.kite docs/reference/kite-language.md docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md tests/repository_shape.rs
git commit -m "docs: publish declarative agent preview"
```

---

## Slice 1 Acceptance Gate

Slice 1 is complete only when:

- the reviewed grammar and canonical fixture are checked in;
- exactly one `agent` with nested tasks parses under existing resource bounds;
- semantic roles, event declarations, NXL-shaped traversal, and context modes
  compile into version-2 metadata;
- every legacy workflow remains version 1 with an unchanged identity and
  execution behavior;
- workflow parsing emits one bounded `deprecated_workflow` warning;
- agent publication fails before persistence with
  `agent_runtime_unavailable`;
- the full Rust, Python, and TypeScript gates pass; and
- documentation makes no claim that goals, agents, plugins, channels, workers,
  connectors, budgets, Codex, Cursor, or VS Code are operational yet.

The next planning artifact is Slice 2: canonical `UserTask` and immutable
`GoalContract`. It begins only after this acceptance gate is reviewed.
