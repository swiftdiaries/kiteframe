# Kiteframe Declarative Agents Slice 1 Foundation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task by task. Checkbox
> (`- [ ]`) syntax tracks progress.

**Goal:** Land the complete authoring and durable-intent foundation in one
coherent slice: reviewed NXL-shaped `agent` syntax, immutable tagged
publication, a bounded canonical `UserTask`, and one digest-bound immutable
`GoalContract` per accepted task, without changing any published workflow or
granting agent execution authority.

**Architecture:** The parser produces a tagged `DefinitionAst`. Both legacy
workflows and new agents compile once through a normalized definition view into
the versioned `CompiledDefinition` IR. PostgreSQL stores both kinds as immutable
published definitions. An authenticated submission atomically resolves one
exact agent version and persists the task and goal contract. Existing workflow
runs continue unchanged; the legacy run-start path rejects agents before
creating execution state. Goal phases and agent-runtime transitions begin in
consolidated Slice 2.

**Tech Stack:** Rust 1.97.1 / edition 2024, Serde/serde_json 1, UUID v7,
RFC 8785 canonical JSON and SHA-256 through Kiteframe's existing hash module,
SQLx 0.8 with PostgreSQL, ACP v1 and feature-gated ACP v2, existing Python and
TypeScript client compatibility gates; no new dependencies.

**Spec:**
`docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md`

## Global Constraints

- The approved design is normative. This plan implements consolidated V1
  Slice 1 only.
- Preserve every valid `workflow` source, version-1 compiled JSON contract,
  definition hash, store row, engine transition, ACP method, and Python and
  TypeScript client call.
- `workflow` is deprecated syntax, not interchangeable with `agent`. It cannot
  declare agent events, semantic model roles, context modes, or durable goals.
- Agents use `dsl_version = 2`; workflows remain `dsl_version = 1`. Existing
  version-1 compiled fixtures must deserialize and reserialize without
  agent-only keys.
- Agent definitions are immutable and publishable in this slice. They are not
  executable: `StartRun` rejects an agent with `agent_runtime_unavailable`
  before creating a run, frame, event, or tool-binding row.
- Every accepted task has exactly one immutable `GoalContract` and freezes one
  exact published agent definition. Idempotent replay returns that original
  selection even after a newer agent version is published.
- The authenticated connection principal is authoritative. Submission JSON
  cannot supply or override it. The kernel allocates the UUID v7 task id.
- `context isolated` is the default agent edge. `context kept` is represented
  in compiled metadata but has no runtime authority until consolidated Slice 2.
- `model reasoning` and `model coding` are semantic roles only. Source, task,
  and protocol types contain no provider, credential, endpoint, subscription,
  workspace-path, or raw-model fields.
- Agent event declarations are versioned semantic names. This slice freezes
  them in immutable definitions but does not register channels or deliver
  events.
- Goal text is declarative data, never executable policy or a model-evaluated
  completion predicate. Evidence types are syntax-checked but unresolved.
- Goal phase changes, evidence assertions, completion or blocker commands,
  runtime plugins, context artifacts, workers, approvals, budgets, connectors,
  IDE integration, and service-monitor behavior are outside this slice.
- Preserve bounded source, token, identifier, nesting, diagnostic, task, and
  goal limits. Warnings consume the existing diagnostic budget.
- Definition identity includes definition kind, normalized agent metadata, the
  exact source, and immutable tool descriptors. Legacy workflow identity stays
  byte-for-byte unchanged.
- Use TDD: add the named failing test first, make the minimum change, run the
  focused and cumulative gates, then commit only the listed paths.
- Preserve unrelated `.worktrees/`, `fuzz/`, and `site/` content.

## Reviewed contracts

### Agent syntax

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

The parser maps agent `return` to the existing tail-transition operation and
`fork { ... } join` to the existing structured parallel operation. Legacy
`goto` and `parallel` remain workflow-only. `done`, `fail`, and `ask` keep their
current semantics in both definition kinds.

### Task submission

```json
{
  "sessionId": "018f0f47-5d8b-7d5a-9cb1-3a09d0d4c902",
  "agent": { "name": "babysit_pr", "version": 1 },
  "target": {
    "type": "github_pull_request",
    "repository": "acme/payments",
    "number": 142
  },
  "preferences": { "harnessRole": "coding" },
  "goal": {
    "version": 1,
    "objective": "Address actionable review feedback and see the PR through merge.",
    "success": [
      {
        "id": "reviews_resolved",
        "statement": "All actionable review threads are resolved.",
        "evidence": "github.unresolved_reviews"
      },
      {
        "id": "checks_passed",
        "statement": "Required GitHub Actions checks pass.",
        "evidence": "github.required_checks"
      },
      {
        "id": "pull_request_merged",
        "statement": "The pull request reaches merged state.",
        "evidence": "github.pull_request_state"
      }
    ],
    "constraints": [
      "Never merge the pull request.",
      "Never bypass required checks.",
      "Repository mutation requires approval."
    ],
    "blockedWhen": [
      "Required user or reviewer action remains unavailable.",
      "No policy-permitted repair path remains."
    ]
  },
  "idempotencyKey": "submit-babysit-pr-142"
}
```

The agent version may be omitted. Acceptance then selects the latest published
version visible inside the transaction and freezes that exact version. Every
JSON object denies unknown fields.

### Validation ceilings

| Field | Limit |
|---|---:|
| agent name | 256 UTF-8 bytes |
| repository | 255 characters |
| objective | 4,096 characters |
| success criteria | 1-32 |
| criterion id | 64 ASCII characters |
| criterion statement | 2,048 characters |
| evidence type | 128 ASCII characters |
| constraints | 0-32 entries, 2,048 characters each |
| blocked conditions | 0-32 entries, 2,048 characters each |

Agent names match `^[A-Za-z_][A-Za-z0-9_]*$`. Criterion ids match
`^[a-z][a-z0-9_]{0,63}$`. Evidence types match
`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`. A repository is exactly two non-empty
GitHub path segments containing ASCII letters, digits, `.`, `_`, or `-`, with
no leading or trailing slash. Text is trimmed for validation but stored
byte-for-byte; invalid text fails instead of being rewritten.

## Final file structure

```text
docs/reference/
  kite-language.md
  user-task-and-goal-contract.md
examples/babysit_pr.kite
migrations/0012_user_tasks_and_goal_contracts.sql
src/
  goal.rs
  task.rs
  dsl/{ast,compiler,diagnostic,ir,lexer,mod,parser}.rs
  store/{models,postgres}.rs
  protocol/{application,transport,types,v1,v2}.rs
tests/
  agent_compiler.rs
  agent_parser.rs
  agent_publication.rs
  definition_store.rs
  goal_contract.rs
  task_store.rs
  fixtures/dsl/{agent_babysit_pr.kite,agent_babysit_pr.compiled.json,legacy_minimal.compiled.json}
  fixtures/task/babysit_pr.submit.json
```

## Dependency flow

```text
reviewed syntax + task fixtures
  -> tagged AST + definition-aware traversal
  -> one normalized, versioned compiled IR
  -> bounded task and goal domain types
  -> tagged definition and immutable task/goal persistence
  -> immutable agent publication + atomic task acceptance
  -> ACP and remote submit/get adapters
  -> recovery, compatibility, and non-authority evidence
```

## Task 1: Freeze the language and task contracts

**Files:**

- Create: `docs/reference/kite-language.md`
- Create: `docs/reference/user-task-and-goal-contract.md`
- Create: `tests/fixtures/dsl/agent_babysit_pr.kite`
- Create: `tests/fixtures/task/babysit_pr.submit.json`
- Create: `tests/agent_parser.rs`
- Create: `tests/goal_contract.rs`

- [ ] **Step 1: Add failing artifact tests**

Add these tests:

```rust
// tests/agent_parser.rs
use std::fs;

#[test]
fn reviewed_agent_language_contract_is_checked_in() {
    let reference = fs::read_to_string("docs/reference/kite-language.md")
        .expect("language reference exists");
    let fixture = fs::read_to_string("tests/fixtures/dsl/agent_babysit_pr.kite")
        .expect("agent fixture exists");

    assert!(reference.contains("agent-definition"));
    assert!(reference.contains("workflow-definition (deprecated)"));
    assert!(fixture.starts_with("agent babysit_pr("));
    assert!(fixture.ends_with("}\n"));
}
```

```rust
// tests/goal_contract.rs
use std::fs;

#[test]
fn reviewed_user_task_contract_is_checked_in() {
    let reference = fs::read_to_string(
        "docs/reference/user-task-and-goal-contract.md",
    )
    .expect("task and goal reference exists");
    let fixture = fs::read_to_string(
        "tests/fixtures/task/babysit_pr.submit.json",
    )
    .expect("canonical submission fixture exists");

    assert!(reference.contains("GoalContract is immutable"));
    assert!(reference.contains("Slice 2 owns goal phases"));
    let value: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    assert_eq!(value["target"]["type"], "github_pull_request");
    assert_eq!(value["goal"]["success"].as_array().unwrap().len(), 3);
    assert!(value.get("principal").is_none());
}
```

- [ ] **Step 2: Observe the missing artifacts**

Run:

```bash
cargo test --test agent_parser reviewed_agent_language_contract_is_checked_in -- --exact
cargo test --test goal_contract reviewed_user_task_contract_is_checked_in -- --exact
```

Expected: FAIL because the references and fixtures do not exist.

- [ ] **Step 3: Write the exact references and fixtures**

The language reference must define:

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

It also records the event-name expression
`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+\.v[1-9][0-9]*$`, current identifier
rules, optional-comma behavior, and that agent execution is unavailable until
Slice 2. The task reference copies the validation table and states:

```text
GoalContract is immutable after atomic task acceptance.
The authenticated connection supplies the principal.
The accepted task freezes one exact published agent definition.
Harness completion is invocation evidence, not goal completion authority.
Slice 2 owns goal phases, evidence assertions, blockers, and completion commands.
```

Write the complete `agent babysit_pr` and task-submission blocks from
**Reviewed contracts** into the named fixtures with one trailing newline. These
blocks are the single source in this plan; later tasks load them with
`include_str!` rather than restating them.

- [ ] **Step 4: Run both artifact tests**

Run:

```bash
cargo test --test agent_parser reviewed_agent_language_contract_is_checked_in -- --exact
cargo test --test goal_contract reviewed_user_task_contract_is_checked_in -- --exact
```

Expected: PASS.

- [ ] **Step 5: Commit the review boundary**

```bash
git add docs/reference/kite-language.md docs/reference/user-task-and-goal-contract.md tests/fixtures/dsl/agent_babysit_pr.kite tests/fixtures/task/babysit_pr.submit.json tests/agent_parser.rs tests/goal_contract.rs
git commit -m "docs: freeze declarative agent foundation contracts"
```

## Task 2: Parse tagged definitions and bounded diagnostics

**Files:**

- Modify: `src/dsl/{ast,compiler,diagnostic,lexer,mod,parser}.rs`
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

Move legacy top-level tasks into `WorkflowAst.tasks` so `SourceFile` owns only
records plus one tagged definition.

- [ ] **Step 1: Add failing tag and warning tests**

```rust
use kiteframe::dsl::{
    DefinitionAst, DiagnosticSeverity, ModelRoleAst, parse, parse_report,
};

const SUPPORT: &str = include_str!("fixtures/dsl/support.kite");
const BABYSIT: &str = include_str!("fixtures/dsl/agent_babysit_pr.kite");
const SUPPORT: &str = include_str!("fixtures/dsl/support.kite");

#[test]
fn legacy_workflow_parses_with_one_bounded_deprecation_warning() {
    let report = parse_report(SUPPORT, limits()).expect("workflow remains valid");
    assert!(matches!(report.source.definition, DefinitionAst::Workflow(_)));
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(report.diagnostics[0].code, "deprecated_workflow");
    assert!(parse(SUPPORT, limits()).is_ok());
}

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

- [ ] **Step 2: Observe unresolved interfaces**

Run `cargo test --test dsl_parser --test agent_parser`. Expected: FAIL because
the tagged AST and report API do not exist.

- [ ] **Step 3: Implement reports without breaking `parse`**

Add `Diagnostic::{error,warning}` constructors and stable snake-case codes.
Keep `Diagnostic::new` only as the existing compiler-error constructor during
this task. Implement:

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

impl Diagnostic {
    pub(crate) fn error(
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> Self {
        Self { code, message: message.into(), severity: DiagnosticSeverity::Error, span }
    }

    pub(crate) fn warning(
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> Self {
        Self { code, message: message.into(), severity: DiagnosticSeverity::Warning, span }
    }
}

pub struct ParseReport {
    pub source: SourceFile,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse_report(
    source: &str,
    limits: ParseLimits,
) -> Result<ParseReport, Vec<Diagnostic>> {
    if source.len() > limits.max_source_bytes {
        return Err(vec![Diagnostic::error(
            "source_too_large",
            "source exceeds configured byte limit",
            Span { start: limits.max_source_bytes, end: source.len() },
        )]);
    }
    let tokens = lex(
        source,
        limits.max_tokens,
        limits.max_identifier_bytes,
        limits.max_diagnostics,
    )?;
    let mut parser = Parser {
        tokens,
        position: 0,
        diagnostics: Vec::new(),
        limits,
        had_error: false,
    };
    let parsed = parser.source_file(source.len());
    if parser.had_error {
        return Err(parser.diagnostics);
    }
    let source = parsed.ok_or_else(Vec::new)?;
    if let DefinitionAst::Workflow(workflow) = &source.definition {
        parser.diagnostics.push(Diagnostic::warning(
            "deprecated_workflow",
            "workflow syntax is deprecated; use agent for new durable definitions",
            workflow.span,
        ));
    }
    Ok(ParseReport { source, diagnostics: parser.diagnostics })
}

pub fn parse(source: &str, limits: ParseLimits) -> Result<SourceFile, Vec<Diagnostic>> {
    parse_report(source, limits).map(|report| report.source)
}
```

Convert lexer errors to `Diagnostic::error` with their stable codes while
preserving current spans, messages, token ceilings, and truncation behavior.

Warnings and errors share the current bounded diagnostic count. Existing tests
continue to assert their current human-readable messages.

- [ ] **Step 4: Parse agent-only declarations**

Parse `agent`, nested `task`, `entry`, `events`, `limits`, and exactly one
`model` role per agent task. Add `model_role: Option<ModelRoleAst>` to
`TaskAst`; it is `None` for workflows. Reject `model` in workflows and
`elapsed_seconds` outside agents.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelRoleAst {
    Reasoning,
    Coding,
}

pub struct EventDeclarationAst {
    pub event_type: String,
    pub span: Span,
}

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
```

- [ ] **Step 5: Add the negative matrix**

Cover duplicate or missing entry, repeated event declarations, duplicate or
malformed event names, zero tasks, nested agents, a top-level task beside an
agent, missing model role, a workflow model role, and workflow
`elapsed_seconds`. Assert exact diagnostic code and bounded count.

```rust
const SIMPLE_AGENT: &str = r#"
agent example() -> string {
  entry work()
  events ["github.review.submitted.v1"]
  limits { active_execution_seconds: 1 elapsed_seconds: 2 }
  task work() -> string {
    model reasoning
    prompt """work"""
    tools []
    on ok() { done "ok" }
  }
}
"#;

#[test]
fn rejects_agent_declaration_cardinality_and_separation_errors() {
    let cases = vec![
        (SIMPLE_AGENT.replacen("  entry work()\n", "", 1), "missing_agent_entry"),
        (SIMPLE_AGENT.replacen("  entry work()", "  entry work()\n  entry work()", 1),
            "duplicate_agent_entry"),
        (SIMPLE_AGENT.replacen("  events [", "  events []\n  events [", 1),
            "duplicate_agent_events"),
        (SIMPLE_AGENT.replace(
            "events [\"github.review.submitted.v1\"]",
            "events [\"github.review.submitted.v1\", \"github.review.submitted.v1\"]",
        ), "duplicate_agent_event"),
        (SIMPLE_AGENT.replace(
            "\"github.review.submitted.v1\"",
            "\"GitHub review\"",
        ), "invalid_agent_event"),
        (SIMPLE_AGENT.replace(
            "  task work() -> string {\n    model reasoning\n    prompt \"\"\"work\"\"\"\n    tools []\n    on ok() { done \"ok\" }\n  }\n",
            "",
        ), "missing_agent_task"),
        (SIMPLE_AGENT.replace("  task work()", "  agent nested()"),
            "nested_agent_not_allowed"),
        (format!("{SIMPLE_AGENT}\ntask outside() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"x\" }} }}"),
            "unexpected_top_level_task"),
        (SIMPLE_AGENT.replacen("    model reasoning\n", "", 1),
            "missing_model_role"),
        (SUPPORT.replacen("  prompt", "  model reasoning\n  prompt", 1),
            "model_role_requires_agent"),
        (SUPPORT.replace(
            "    active_execution_seconds: 300",
            "    active_execution_seconds: 300\n    elapsed_seconds: 301",
        ), "elapsed_seconds_requires_agent"),
    ];
    for (source, code) in cases {
        let diagnostics = parse_report(&source, limits()).unwrap_err();
        assert_eq!(diagnostics[0].code, code, "source: {source}");
        assert!(diagnostics.len() <= limits().max_diagnostics);
    }
}
```

- [ ] **Step 6: Run and commit**

Run `cargo test --test dsl_parser --test agent_parser`; all pass.

```bash
git add src/dsl/ast.rs src/dsl/compiler.rs src/dsl/diagnostic.rs src/dsl/lexer.rs src/dsl/mod.rs src/dsl/parser.rs tests/dsl_parser.rs tests/agent_parser.rs
git commit -m "feat: parse tagged declarative agents"
```

## Task 3: Parse definition-aware traversal

**Files:**

- Modify: `src/dsl/ast.rs`
- Modify: `src/dsl/parser.rs`
- Modify: `tests/agent_parser.rs`

**Interfaces:**

```rust
pub enum ContextModeAst { Legacy, Isolated, Kept }

pub struct CallAst {
    pub binding: Option<String>,
    pub task: String,
    pub arguments: Vec<ArgumentAst>,
    pub context: ContextModeAst,
    pub span: Span,
}
```

`StatementAst` remains `Call`, `Parallel`, `Ask`, `Goto`, `Done`, and `Fail`.
Agent `fork` parses as `Parallel`; agent `return` parses as `Goto`.

- [ ] **Step 1: Add failing traversal tests**

```rust
use kiteframe::dsl::{
    ContextModeAst, DefinitionAst, StatementAst, parse_report,
};

const BABYSIT: &str = include_str!("fixtures/dsl/agent_babysit_pr.kite");

#[test]
fn traversal_retains_call_return_fork_and_context_modes() {
    let report = parse_report(BABYSIT, limits()).unwrap();
    let DefinitionAst::Agent(agent) = report.source.definition else {
        panic!("fixture must be an agent");
    };
    let inspect = agent.tasks.iter().find(|task| task.name == "inspect_pr").unwrap();

    let repair = inspect.outcomes.iter().find(|outcome| outcome.name == "repair").unwrap();
    let StatementAst::Call(repair_call) = &repair.statements[0] else {
        panic!("repair starts with call");
    };
    assert_eq!(repair_call.context, ContextModeAst::Kept);

    let refresh = inspect.outcomes.iter().find(|outcome| outcome.name == "refresh").unwrap();
    let StatementAst::Goto(refresh_return) = &refresh.statements[0] else {
        panic!("refresh is a tail return");
    };
    assert_eq!(refresh_return.context, ContextModeAst::Isolated);

    let compare = inspect.outcomes.iter().find(|outcome| outcome.name == "compare").unwrap();
    let StatementAst::Parallel(branches) = &compare.statements[0] else {
        panic!("compare starts with a fork");
    };
    assert_eq!(branches.len(), 2);
    assert!(branches.iter().all(|call| call.context == ContextModeAst::Isolated));
}

#[test]
fn rejects_invalid_definition_specific_traversal() {
    let cases = [
        (BABYSIT.replace("      } join", "      }"), "missing_fork_join"),
        (BABYSIT.replace(
            "      fork {\n        first = call inspect_branch(name = left) context isolated\n        second = call inspect_branch(name = right) context isolated\n      } join",
            "      fork { } join",
        ), "empty_fork"),
        (BABYSIT.replace(
            "first = call inspect_branch",
            "call inspect_branch",
        ), "fork_call_requires_binding"),
        (BABYSIT.replace("context kept", "context summarized"), "invalid_context_mode"),
        (BABYSIT.replace("return inspect_pr", "goto inspect_pr"), "goto_requires_workflow"),
        (SUPPORT.replace("goto route", "return route"), "return_requires_agent"),
    ];

    for (source, code) in cases {
        let diagnostics = parse_report(&source, limits()).unwrap_err();
        assert_eq!(diagnostics[0].code, code, "source: {source}");
    }
}
```

- [ ] **Step 2: Observe the legacy-only parser behavior**

Run `cargo test --test agent_parser traversal -- --nocapture`. Expected: FAIL
because only workflow `parallel` and `goto` are recognized.

- [ ] **Step 3: Implement syntax-aware parsing**

Pass a private `DefinitionSyntax::{Workflow,Agent}` through task, outcome, and
statement parsing. Agent calls default to `Isolated`; workflow calls use the
internal `Legacy` value so version-1 hashes and JSON remain unchanged. Reuse
the existing terminal-operation rules: `return`, `done`, and `fail` are
terminal; `fork` and `call` are not. Reject statements after a terminal.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DefinitionSyntax {
    Workflow,
    Agent,
}

impl DefinitionSyntax {
    const fn default_context(self) -> ContextModeAst {
        match self {
            Self::Workflow => ContextModeAst::Legacy,
            Self::Agent => ContextModeAst::Isolated,
        }
    }
}

fn parse_context_mode(
    parser: &mut Parser<'_>,
    syntax: DefinitionSyntax,
) -> Result<ContextModeAst, Diagnostic> {
    if !parser.consume_keyword("context") {
        return Ok(syntax.default_context());
    }
    match parser.expect_identifier()?.as_str() {
        "isolated" if syntax == DefinitionSyntax::Agent => Ok(ContextModeAst::Isolated),
        "kept" if syntax == DefinitionSyntax::Agent => Ok(ContextModeAst::Kept),
        _ => Err(Diagnostic::error(
            "invalid_context_mode",
            "agent context must be isolated or kept",
            parser.current_span(),
        )),
    }
}
```

In the existing statement dispatcher, accept `fork` and `return` only for
`Agent`, and accept `parallel` and `goto` only for `Workflow`; construct the
same `StatementAst::Parallel` and `StatementAst::Goto` variants after parsing.

- [ ] **Step 4: Run and commit**

Run `cargo test --test agent_parser --test dsl_parser`; all pass.

```bash
git add src/dsl/ast.rs src/dsl/parser.rs tests/agent_parser.rs
git commit -m "feat: parse typed agent traversal"
```

## Task 4: Compile once into the versioned definition IR

**Files:**

- Modify: `src/dsl/{compiler,ir,mod}.rs`
- Modify: `src/engine/{mod,transition}.rs`
- Modify: `src/lib.rs`
- Create: `tests/agent_compiler.rs`
- Modify: `tests/dsl_compiler.rs`
- Modify: `tests/transition_engine.rs`
- Create: `tests/fixtures/dsl/agent_babysit_pr.compiled.json`
- Create: `tests/fixtures/dsl/legacy_minimal.compiled.json`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionKind { #[default] Workflow, Agent }

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode { #[default] Legacy, Isolated, Kept }

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole { Reasoning, Coding }

pub struct CompiledCall {
    pub task: String,
    pub arguments: BTreeMap<String, CompiledValue>,
    #[serde(default, skip_serializing_if = "ContextMode::is_legacy")]
    pub context: ContextMode,
}

pub struct CompiledTask {
    pub inputs: BTreeMap<String, CompiledType>,
    pub result: CompiledType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_role: Option<ModelRole>,
    pub prompt: String,
    #[serde(default, skip_deserializing)]
    pub tools: Vec<CompiledToolBinding>,
    pub outcomes: BTreeMap<String, CompiledOutcome>,
    pub completion: CompletionContract,
}

pub struct CompiledDefinition {
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

#[deprecated(note = "use CompiledDefinition")]
pub type CompiledWorkflow = CompiledDefinition;

pub struct Compilation {
    pub definition: CompiledDefinition,
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
) -> Result<CompiledDefinition, Vec<Diagnostic>> {
    compile_report(source, catalog, ceilings).map(|value| value.definition)
}

// Test-local helpers in tests/agent_compiler.rs mirror tests/dsl_compiler.rs.
fn ceilings() -> Limits;
fn tool(qualified_name: &str) -> CatalogTool;
fn catalog(tools: impl IntoIterator<Item = CatalogTool>) -> ToolCatalogSnapshot;
```

`CompiledCall.context` defaults to `Legacy` and is omitted when legacy.
`CompiledTask.model_role` defaults to `None` and is omitted when absent.
`DefinitionKind` defaults to `Workflow`. These defaults preserve version-1
deserialization and serialized bytes.

- [ ] **Step 1: Add failing compatibility and agent tests**

```rust
use kiteframe::dsl::{
    ContextMode, DefinitionKind, ModelRole, compile_report,
};

const BABYSIT: &str = include_str!("fixtures/dsl/agent_babysit_pr.kite");

#[test]
fn version_one_json_defaults_to_workflow_and_legacy_context() {
    let json = include_str!("fixtures/dsl/legacy_minimal.compiled.json");
    let compiled: CompiledDefinition = serde_json::from_str(json).unwrap();
    assert_eq!(compiled.definition_kind, DefinitionKind::Workflow);
    assert_eq!(compiled.entry.context, ContextMode::Legacy);
    assert!(compiled.events.is_empty());
    assert!(compiled.tasks.values().all(|task| task.model_role.is_none()));

    let round_trip = serde_json::to_value(compiled).unwrap();
    assert!(round_trip.get("definition_kind").is_none());
    assert!(round_trip.get("events").is_none());
}

#[test]
fn compiles_agent_metadata_and_reuses_graph_safety_checks() {
    let result = compile_report(
        BABYSIT,
        &catalog([tool("github/context")]),
        &ceilings(),
    )
    .expect("reviewed agent compiles");
    let compiled = result.definition;
    assert!(result.diagnostics.is_empty());
    assert_eq!(compiled.dsl_version, 2);
    assert_eq!(compiled.definition_kind, DefinitionKind::Agent);
    assert_eq!(compiled.events.len(), 3);
    assert_eq!(
        compiled.tasks["repair_reviews"].model_role,
        Some(ModelRole::Coding),
    );
    let CompiledStatement::Call { call, .. } =
        &compiled.tasks["inspect_pr"].outcomes["repair"].statements[0]
    else {
        panic!("repair starts with a compiled call");
    };
    assert_eq!(call.context, ContextMode::Kept);
    assert_eq!(compiled.tool_bindings.len(), 1);
    assert_eq!(compiled.tool_bindings[0].qualified_name, "github/context");
}
```

- [ ] **Step 2: Observe missing IR types**

Run `cargo test --test agent_compiler --test dsl_compiler`. Expected: FAIL to
compile.

- [ ] **Step 3: Rename and extend the IR in one pass**

Rename `CompiledWorkflow` to `CompiledDefinition` throughout Rust code and add
the deprecated source alias above. Do not revisit the type name in a later
task. Update every struct literal in compiler, engine, and tests now.

```rust
impl DefinitionKind {
    pub const fn is_workflow(&self) -> bool {
        matches!(self, Self::Workflow)
    }
}

impl ContextMode {
    pub const fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }
}
```

Mark `Workflow` and `Legacy` with `#[default]` in their enums. Every workflow
constructor explicitly sets version 1, workflow kind, legacy call contexts,
empty events, and absent model roles even though Serde supplies defaults when
reading historical JSON.

- [ ] **Step 4: Normalize both AST tags once**

Compile through this private view:

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

Reuse current field, record, tool, reachability, recursion, result-type,
parallel-limit, completion-contract, and canonical-hash checks. Map agent
`fork`/`return` to compiled `Parallel`/`Goto` while retaining context. Extend
`Limits` with `elapsed_seconds`, default `u64::MAX`; require agent elapsed time
to exceed active time and reject the field in workflows.

```rust
let view = match &ast.definition {
    DefinitionAst::Workflow(workflow) => DefinitionView {
        kind: DefinitionKind::Workflow,
        dsl_version: 1,
        name: &workflow.name,
        inputs: &workflow.inputs,
        result: &workflow.result,
        entry: &workflow.entry,
        limits: &workflow.limits,
        events: Vec::new(),
        tasks: &workflow.tasks,
        span: workflow.span,
    },
    DefinitionAst::Agent(agent) => DefinitionView {
        kind: DefinitionKind::Agent,
        dsl_version: 2,
        name: &agent.name,
        inputs: &agent.inputs,
        result: &agent.result,
        entry: &agent.entry,
        limits: &agent.limits,
        events: agent.events.iter().map(|event| event.event_type.as_str()).collect(),
        tasks: &agent.tasks,
        span: agent.span,
    },
};

if view.kind == DefinitionKind::Agent
    && compiled_limits.elapsed_seconds <= compiled_limits.active_execution_seconds
{
    return Err(vec![Diagnostic::error(
        "invalid_elapsed_limit",
        "elapsed_seconds must exceed active_execution_seconds",
        view.span,
    )]);
}
```

- [ ] **Step 5: Freeze both golden files**

Create the legacy golden from a no-tool workflow and prove round-trip output
does not add agent keys. Freeze the agent kind, version, events, roles,
contexts, descriptors, and definition hash. The existing support fixture and
hash remain unchanged.

- [ ] **Step 6: Run and commit**

Run:

```bash
cargo test --test agent_compiler --test dsl_compiler --test transition_engine
```

```bash
git add src/dsl/compiler.rs src/dsl/ir.rs src/dsl/mod.rs src/engine/mod.rs src/engine/transition.rs src/lib.rs tests/agent_compiler.rs tests/dsl_compiler.rs tests/transition_engine.rs tests/fixtures/dsl/agent_babysit_pr.compiled.json tests/fixtures/dsl/legacy_minimal.compiled.json
git commit -m "feat: compile versioned agent definitions"
```

## Task 5: Add bounded task and goal domain types

**Files:**

- Create: `src/goal.rs`
- Create: `src/task.rs`
- Modify: `src/domain.rs`
- Modify: `src/lib.rs`
- Modify: `tests/goal_contract.rs`

**Interfaces:**

```rust
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UserTaskId(uuid::Uuid);

impl UserTaskId {
    pub const fn new(value: uuid::Uuid) -> Self { Self(value) }
    pub const fn as_uuid(&self) -> &uuid::Uuid { &self.0 }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalContract {
    pub version: u16,
    pub objective: String,
    pub success: Vec<SuccessCriterion>,
    pub constraints: Vec<String>,
    pub blocked_when: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuccessCriterion {
    pub id: String,
    pub statement: String,
    pub evidence: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskTarget {
    GithubPullRequest { repository: String, number: u64 },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessRole { Reasoning, Coding }

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_role: Option<HarnessRole>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSelector {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitUserTask {
    pub session_id: SessionId,
    pub agent: AgentSelector,
    pub target: TaskTarget,
    pub preferences: TaskPreferences,
    pub goal: GoalContract,
    pub idempotency_key: IdempotencyKey,
}

impl GoalContract {
    pub fn validate(&self) -> Result<(), GoalContractError>;
    pub fn digest(&self) -> Result<String, GoalContractError>;
}

impl SubmitUserTask {
    pub fn validate(&self) -> Result<(), UserTaskError>;
    pub fn request_digest(&self) -> Result<String, UserTaskError>;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum GoalContractError {
    #[error("unsupported goal contract version")]
    UnsupportedVersion,
    #[error("goal objective must be non-empty and at most 4096 characters")]
    InvalidObjective,
    #[error("goal must contain between 1 and 32 success criteria")]
    InvalidSuccessCount,
    #[error("goal success criterion id is invalid: {0}")]
    InvalidCriterionId(String),
    #[error("goal success criterion id is duplicated: {0}")]
    DuplicateCriterionId(String),
    #[error("goal success criterion statement must be non-empty and at most 2048 characters")]
    InvalidCriterionStatement,
    #[error("goal evidence type is invalid: {0}")]
    InvalidEvidenceType(String),
    #[error("goal constraints exceed the count or text limit")]
    InvalidConstraints,
    #[error("goal blocked conditions exceed the count or text limit")]
    InvalidBlockedConditions,
    #[error("goal canonicalization failed: {0}")]
    Canonicalization(String),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum UserTaskError {
    #[error(transparent)]
    InvalidGoal(#[from] GoalContractError),
    #[error("agent name must be a valid DSL identifier of at most 256 bytes")]
    InvalidAgentName,
    #[error("agent version must be greater than zero when supplied")]
    InvalidAgentVersion,
    #[error("GitHub repository must be an owner/name path of at most 255 characters")]
    InvalidRepository,
    #[error("GitHub pull request number must be greater than zero")]
    InvalidPullRequestNumber,
    #[error("task canonicalization failed: {0}")]
    Canonicalization(String),
}
```

Every struct uses `deny_unknown_fields`; wire fields use camel case. The target
is internally tagged with `type` and snake-case values. `GoalContract::digest`
hashes every goal field. `SubmitUserTask::request_digest` hashes every semantic
request field except the idempotency key.

- [ ] **Step 1: Add failing validation and digest tests**

```rust
use kiteframe::task::SubmitUserTask;

fn valid_request() -> SubmitUserTask {
    serde_json::from_str(include_str!("fixtures/task/babysit_pr.submit.json"))
        .unwrap()
}

#[test]
fn canonical_fixture_validates_and_has_order_stable_digests() {
    let first = valid_request();
    first.validate().unwrap();

    let reordered: SubmitUserTask = serde_json::from_value(serde_json::json!({
        "idempotencyKey": "submit-babysit-pr-142",
        "goal": first.goal.clone(),
        "preferences": first.preferences.clone(),
        "target": first.target.clone(),
        "agent": first.agent.clone(),
        "sessionId": first.session_id.clone(),
    }))
    .unwrap();

    assert_eq!(first.request_digest().unwrap(), reordered.request_digest().unwrap());
    assert_eq!(first.goal.digest().unwrap(), reordered.goal.digest().unwrap());
}
```

- [ ] **Step 2: Observe missing domain types**

Run `cargo test --test goal_contract`. Expected: FAIL to compile.

- [ ] **Step 3: Implement closed types and exact validation**

Use typed `GoalContractError` variants for unsupported version, objective,
success count, criterion id, duplicate id, statement, evidence type,
constraints, blocked conditions, and canonicalization. Use typed
`UserTaskError` variants for invalid goal, agent name/version, repository, PR
number, and canonicalization. Use ASCII byte predicates rather than adding
`regex`; use `.chars().count()` for text ceilings.

```rust
fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= maximum
}

fn lower_segment(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn criterion_id(value: &str) -> bool {
    value.len() <= 64 && lower_segment(value)
}

fn evidence_type(value: &str) -> bool {
    value.len() <= 128
        && value.split('.').count() >= 2
        && value.split('.').all(lower_segment)
}

impl GoalContract {
    pub fn validate(&self) -> Result<(), GoalContractError> {
        if self.version != 1 {
            return Err(GoalContractError::UnsupportedVersion);
        }
        if !bounded_text(&self.objective, 4_096) {
            return Err(GoalContractError::InvalidObjective);
        }
        if !(1..=32).contains(&self.success.len()) {
            return Err(GoalContractError::InvalidSuccessCount);
        }

        let mut ids = BTreeSet::new();
        for criterion in &self.success {
            if !criterion_id(&criterion.id) {
                return Err(GoalContractError::InvalidCriterionId(criterion.id.clone()));
            }
            if !ids.insert(criterion.id.clone()) {
                return Err(GoalContractError::DuplicateCriterionId(criterion.id.clone()));
            }
            if !bounded_text(&criterion.statement, 2_048) {
                return Err(GoalContractError::InvalidCriterionStatement);
            }
            if !evidence_type(&criterion.evidence) {
                return Err(GoalContractError::InvalidEvidenceType(
                    criterion.evidence.clone(),
                ));
            }
        }

        if self.constraints.len() > 32
            || self.constraints.iter().any(|value| !bounded_text(value, 2_048))
        {
            return Err(GoalContractError::InvalidConstraints);
        }
        if self.blocked_when.len() > 32
            || self.blocked_when.iter().any(|value| !bounded_text(value, 2_048))
        {
            return Err(GoalContractError::InvalidBlockedConditions);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, GoalContractError> {
        let value = serde_json::to_value(self)
            .map_err(|error| GoalContractError::Canonicalization(error.to_string()))?;
        canonical_sha256(&value)
            .map_err(|error| GoalContractError::Canonicalization(error.to_string()))
    }
}

fn valid_agent_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 256
        && matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_repository(value: &str) -> bool {
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
            })
    };
    value.chars().count() <= 255
        && value.split('/').count() == 2
        && value.split('/').all(valid_segment)
}

impl SubmitUserTask {
    pub fn validate(&self) -> Result<(), UserTaskError> {
        self.goal.validate()?;
        if !valid_agent_name(&self.agent.name) {
            return Err(UserTaskError::InvalidAgentName);
        }
        if self.agent.version == Some(0) {
            return Err(UserTaskError::InvalidAgentVersion);
        }
        let TaskTarget::GithubPullRequest { repository, number } = &self.target;
        if !valid_repository(repository) {
            return Err(UserTaskError::InvalidRepository);
        }
        if *number == 0 {
            return Err(UserTaskError::InvalidPullRequestNumber);
        }
        Ok(())
    }

    pub fn request_digest(&self) -> Result<String, UserTaskError> {
        let mut value = serde_json::to_value(self)
            .map_err(|error| UserTaskError::Canonicalization(error.to_string()))?;
        value.as_object_mut().unwrap().remove("idempotencyKey");
        canonical_sha256(&value)
            .map_err(|error| UserTaskError::Canonicalization(error.to_string()))
    }
}
```

- [ ] **Step 4: Add the complete negative matrix**

Cover goal versions 0 and 2; blank and 4,097-character objectives; zero and 33
criteria; invalid and duplicate ids; blank and 2,049-character statements;
invalid and 129-character evidence types; 33 constraints; blank constraints;
33 blocked conditions; blank blocked conditions; unknown keys at each nesting
level; PR number zero; malformed repository; blank agent name; and version
zero. Assert exact error variants.

```rust
fn assert_goal_error(
    mutate: impl FnOnce(&mut GoalContract),
    expected: GoalContractError,
) {
    let mut goal = valid_request().goal;
    mutate(&mut goal);
    assert_eq!(goal.validate(), Err(expected));
}

#[test]
fn rejects_every_goal_boundary_with_the_typed_error() {
    for version in [0, 2] {
        assert_goal_error(|goal| goal.version = version, GoalContractError::UnsupportedVersion);
    }
    assert_goal_error(|goal| goal.objective = " ".into(), GoalContractError::InvalidObjective);
    assert_goal_error(
        |goal| goal.objective = "x".repeat(4_097),
        GoalContractError::InvalidObjective,
    );
    assert_goal_error(|goal| goal.success.clear(), GoalContractError::InvalidSuccessCount);
    assert_goal_error(
        |goal| goal.success = vec![goal.success[0].clone(); 33],
        GoalContractError::InvalidSuccessCount,
    );
    assert_goal_error(
        |goal| goal.success[0].id = "Bad-ID".into(),
        GoalContractError::InvalidCriterionId("Bad-ID".into()),
    );
    assert_goal_error(
        |goal| goal.success[1].id = goal.success[0].id.clone(),
        GoalContractError::DuplicateCriterionId("reviews_resolved".into()),
    );
    assert_goal_error(
        |goal| goal.success[0].statement = " ".into(),
        GoalContractError::InvalidCriterionStatement,
    );
    assert_goal_error(
        |goal| goal.success[0].statement = "x".repeat(2_049),
        GoalContractError::InvalidCriterionStatement,
    );
    assert_goal_error(
        |goal| goal.success[0].evidence = "invalid".into(),
        GoalContractError::InvalidEvidenceType("invalid".into()),
    );
    let long_evidence = format!("a.{}", "b".repeat(127));
    assert_goal_error(
        |goal| goal.success[0].evidence = long_evidence.clone(),
        GoalContractError::InvalidEvidenceType(long_evidence),
    );
    assert_goal_error(
        |goal| goal.constraints = vec!["valid".into(); 33],
        GoalContractError::InvalidConstraints,
    );
    assert_goal_error(
        |goal| goal.constraints = vec![" ".into()],
        GoalContractError::InvalidConstraints,
    );
    assert_goal_error(
        |goal| goal.blocked_when = vec!["valid".into(); 33],
        GoalContractError::InvalidBlockedConditions,
    );
    assert_goal_error(
        |goal| goal.blocked_when = vec![" ".into()],
        GoalContractError::InvalidBlockedConditions,
    );
}

#[test]
fn rejects_unknown_keys_at_every_request_object() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/task/babysit_pr.submit.json"
    ))
    .unwrap();
    for pointer in ["", "/agent", "/target", "/preferences", "/goal", "/goal/success/0"] {
        let mut value = fixture.clone();
        let object = if pointer.is_empty() {
            value.as_object_mut().unwrap()
        } else {
            value.pointer_mut(pointer).unwrap().as_object_mut().unwrap()
        };
        object.insert("unknown".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<SubmitUserTask>(value).is_err(), "{pointer}");
    }
}

#[test]
fn rejects_invalid_task_selector_and_target() {
    let mut request = valid_request();
    request.agent.name = "".into();
    assert_eq!(request.validate(), Err(UserTaskError::InvalidAgentName));

    let mut request = valid_request();
    request.agent.version = Some(0);
    assert_eq!(request.validate(), Err(UserTaskError::InvalidAgentVersion));

    let mut request = valid_request();
    request.target = TaskTarget::GithubPullRequest {
        repository: "/acme/payments".into(),
        number: 142,
    };
    assert_eq!(request.validate(), Err(UserTaskError::InvalidRepository));

    let mut request = valid_request();
    request.target = TaskTarget::GithubPullRequest {
        repository: "acme/payments".into(),
        number: 0,
    };
    assert_eq!(request.validate(), Err(UserTaskError::InvalidPullRequestNumber));
}
```

- [ ] **Step 5: Run and commit**

Run `cargo test --test goal_contract`; all pass.

```bash
git add src/domain.rs src/goal.rs src/task.rs src/lib.rs tests/goal_contract.rs
git commit -m "feat: define bounded user task goals"
```

## Task 6: Persist tagged definitions and immutable task intent

**Files:**

- Create: `migrations/0012_user_tasks_and_goal_contracts.sql`
- Modify: `src/store/{models,postgres}.rs`
- Create: `tests/definition_store.rs`
- Create: `tests/task_store.rs`

**Interfaces:**

```rust
pub struct PublishedDefinition {
    pub id: uuid::Uuid,
    pub kind: DefinitionKind,
    pub name: String,
    pub version: u32,
    pub definition_hash: String,
}

pub struct AcceptedUserTask {
    pub id: UserTaskId,
    pub session_id: SessionId,
    pub principal: PrincipalId,
    pub definition: PublishedDefinition,
    pub target: TaskTarget,
    pub preferences: TaskPreferences,
    pub goal: GoalContract,
    pub goal_digest: String,
    pub revision: u64,
    pub created_at: time::OffsetDateTime,
}

pub async fn submit_user_task(
    &self,
    principal: &PrincipalId,
    request: SubmitUserTask,
) -> Result<AcceptedUserTask, StoreError>;

pub async fn get_user_task(
    &self,
    session_id: SessionId,
    task_id: UserTaskId,
) -> Result<AcceptedUserTask, StoreError>;

// Integration-test helpers local to tests/task_store.rs.
async fn setup_agent_store() -> Option<(
    Arc<PostgresStore>,
    PgPool,
    SessionView,
    PrincipalId,
    PublishedDefinition,
)>;

async fn task_count(pool: &PgPool, session_id: &SessionId) -> i64;
async fn goal_count(pool: &PgPool, session_id: &SessionId) -> i64;
async fn migrated_store_through_0011() -> Option<(PostgresStore, PgPool)>;
fn compiled_babysit_agent() -> CompiledDefinition;
fn changed_babysit_agent() -> CompiledDefinition;
fn compiled_support_workflow() -> CompiledDefinition;
fn submit_request(
    session_id: SessionId,
    agent_name: String,
    version: Option<u32>,
) -> SubmitUserTask;
async fn resolve_agent_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    name: &str,
    version: Option<u32>,
) -> Result<PublishedDefinition, StoreError>;
async fn get_user_task_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: SessionId,
    task_id: uuid::Uuid,
) -> Result<AcceptedUserTask, StoreError>;
fn accepted_user_task_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<AcceptedUserTask, StoreError>;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

// StoreError additions
#[error("user task was not found")]
UserTaskNotFound,
#[error("user task requires a published agent definition")]
AgentDefinitionRequired,
#[error("user task request is invalid: {0}")]
InvalidUserTask(String),
#[error("agent definitions cannot start through the workflow runner")]
AgentRuntimeUnavailable,
```

- [ ] **Step 1: Add a failing migration-shape test**

```rust
#[test]
fn slice_one_schema_tags_definitions_and_freezes_goals() {
    let migration = std::fs::read_to_string(
        "migrations/0012_user_tasks_and_goal_contracts.sql",
    )
    .expect("foundation migration exists");

    assert!(migration.contains("ADD COLUMN definition_kind"));
    assert!(migration.contains("DEFAULT 'workflow'"));
    assert!(migration.contains("CREATE TABLE user_tasks"));
    assert!(migration.contains("CREATE TABLE goal_contracts"));
    assert!(migration.contains("goal_contracts_reject_update"));
    assert!(migration.contains("goal_contracts_reject_delete"));
    assert!(migration.contains("ERRCODE = '55000'"));
}
```

- [ ] **Step 2: Observe the missing migration**

Run `cargo test --test task_store slice_one_schema_tags_definitions_and_freezes_goals -- --exact`.
Expected: FAIL because migration `0012` does not exist.

- [ ] **Step 3: Write the schema**

Keep the historical `workflow_definitions` table name. Add
`definition_kind TEXT NOT NULL DEFAULT 'workflow'` constrained to `workflow`
or `agent`. Add `user_tasks` with UUID primary key, session/principal and exact
definition foreign keys, submission idempotency key, request digest, JSONB
target/preferences, state constrained to `accepted`, revision constrained to
zero, timestamps, and unique `(session_id, submission_idempotency_key)`. Add
`goal_contracts` with one row per task, version constrained to 1, objective,
success/constraint/block JSONB, digest, timestamp, and the immutable triggers.

```sql
ALTER TABLE workflow_definitions
    ADD COLUMN definition_kind TEXT NOT NULL DEFAULT 'workflow'
        CHECK (definition_kind IN ('workflow', 'agent'));

CREATE TABLE user_tasks (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id),
    principal TEXT NOT NULL,
    definition_id UUID NOT NULL REFERENCES workflow_definitions(id),
    submission_idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    target JSONB NOT NULL,
    preferences JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'accepted' CHECK (state = 'accepted'),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision = 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (session_id, submission_idempotency_key)
);

CREATE TABLE goal_contracts (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL UNIQUE REFERENCES user_tasks(id),
    contract_version INTEGER NOT NULL CHECK (contract_version = 1),
    objective TEXT NOT NULL,
    success_criteria JSONB NOT NULL,
    constraints JSONB NOT NULL,
    blocked_when JSONB NOT NULL,
    contract_digest TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, contract_digest)
);

CREATE FUNCTION reject_goal_contract_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'goal contracts are immutable' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER goal_contracts_reject_update
    BEFORE UPDATE ON goal_contracts
    FOR EACH ROW EXECUTE FUNCTION reject_goal_contract_mutation();

CREATE TRIGGER goal_contracts_reject_delete
    BEFORE DELETE ON goal_contracts
    FOR EACH ROW EXECUTE FUNCTION reject_goal_contract_mutation();
```

- [ ] **Step 4: Tag every definition-store query**

Add `kind` to published-definition models and select/insert
`definition_kind` in every query. Migrate a database from `0001` through
`0011`, apply `0012`, and assert every existing row reads as a workflow.
Publish one workflow and one agent and verify immutable tags and versioning.
Keep the existing global unique `(name, version)` rule; two definition kinds
cannot share one name/version pair.

```rust
#[tokio::test]
async fn migrated_definitions_are_tagged_without_changing_versions() {
    let Some((store, pool)) = migrated_store_through_0011().await else {
        return;
    };
    MIGRATOR.run(&pool).await.unwrap();

    let old_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT definition_kind FROM workflow_definitions ORDER BY name, version",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(old_kinds.iter().all(|kind| kind == "workflow"));

    let workflow = store.publish(compiled_support_workflow()).await.unwrap();
    let agent = store.publish(compiled_babysit_agent()).await.unwrap();
    assert_eq!(workflow.kind, DefinitionKind::Workflow);
    assert_eq!(agent.kind, DefinitionKind::Agent);
}
```

- [ ] **Step 5: Implement atomic acceptance in this lock order**

1. Validate and digest the request and goal before opening SQL.
2. Lock the session and reject wrong principal or closed state.
3. Resolve an existing `(session_id, idempotency_key)` before agent lookup.
4. Return its joined snapshot when the digest matches; otherwise return
   `IdempotencyConflict`.
5. Resolve the exact or latest definition with `definition_kind = 'agent'`.
6. Allocate UUID v7, insert the task, then insert its goal contract.
7. Read the joined typed snapshot, commit, and return it.

Map joined rows through one `accepted_user_task_from_row` function and
deserialize stored JSON through the same closed domain types. Malformed stored
data returns `StoreError::InvalidData`.

```rust
pub async fn submit_user_task(
    &self,
    principal: &PrincipalId,
    request: SubmitUserTask,
) -> Result<AcceptedUserTask, StoreError> {
    request
        .validate()
        .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
    let request_digest = request
        .request_digest()
        .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
    let goal_digest = request
        .goal
        .digest()
        .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
    let mut tx = self.pool.begin().await?;

    let session: Option<(String, String)> = sqlx::query_as(
        "SELECT principal, state FROM sessions WHERE id = $1 FOR UPDATE",
    )
    .bind(request.session_id.as_uuid())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((session_principal, state)) = session else {
        return Err(StoreError::SessionNotFound);
    };
    if session_principal != principal.as_str() {
        return Err(StoreError::PrincipalMismatch);
    }
    if state != "active" {
        return Err(StoreError::SessionClosed);
    }

    let replay: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id, request_digest FROM user_tasks \
         WHERE session_id = $1 AND submission_idempotency_key = $2",
    )
    .bind(request.session_id.as_uuid())
    .bind(request.idempotency_key.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((task_id, stored_digest)) = replay {
        if stored_digest != request_digest {
            return Err(StoreError::IdempotencyConflict);
        }
        let accepted = get_user_task_in_tx(&mut tx, request.session_id, task_id).await?;
        tx.commit().await?;
        return Ok(accepted);
    }

    let definition = resolve_agent_in_tx(
        &mut tx,
        &request.agent.name,
        request.agent.version,
    )
    .await?;
    let task_id = UserTaskId::new(uuid::Uuid::now_v7());
    sqlx::query(
        "INSERT INTO user_tasks \
         (id, session_id, principal, definition_id, submission_idempotency_key, \
          request_digest, target, preferences) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(task_id.as_uuid())
    .bind(request.session_id.as_uuid())
    .bind(principal.as_str())
    .bind(definition.id)
    .bind(request.idempotency_key.as_str())
    .bind(&request_digest)
    .bind(serde_json::to_value(&request.target).unwrap())
    .bind(serde_json::to_value(&request.preferences).unwrap())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO goal_contracts \
         (id, task_id, contract_version, objective, success_criteria, constraints, \
          blocked_when, contract_digest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(task_id.as_uuid())
    .bind(i32::from(request.goal.version))
    .bind(&request.goal.objective)
    .bind(serde_json::to_value(&request.goal.success).unwrap())
    .bind(serde_json::to_value(&request.goal.constraints).unwrap())
    .bind(serde_json::to_value(&request.goal.blocked_when).unwrap())
    .bind(&goal_digest)
    .execute(&mut *tx)
    .await?;

    let accepted = get_user_task_in_tx(
        &mut tx,
        request.session_id.clone(),
        *task_id.as_uuid(),
    )
    .await?;
    tx.commit().await?;
    Ok(accepted)
}
```

`resolve_agent_in_tx` uses `definition_kind = 'agent'` in both exact-version
and latest-version queries. `get_user_task_in_tx` joins `user_tasks`,
`goal_contracts`, and `workflow_definitions`, then delegates to
`accepted_user_task_from_row`.

- [ ] **Step 6: Add persistence and race coverage**

Test exact version, latest version, replay after a newer publication, a fresh
key selecting the newer version, two concurrent identical submissions yielding
one task and goal, changed-content replay conflict, workflow-as-agent, missing
definition, closed session, wrong principal, byte-preserving goal round-trip,
digest recomputation, and direct SQL update/delete rejection. Every failure
must leave no partial task or goal row.

```rust
#[tokio::test]
async fn submission_atomically_freezes_agent_and_goal() {
    let Some((store, _pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let request = submit_request(session.id.clone(), agent.name.clone(), None);
    let accepted = store.submit_user_task(&principal, request.clone()).await.unwrap();

    assert_eq!(accepted.definition.id, agent.id);
    assert_eq!(accepted.definition.kind, DefinitionKind::Agent);
    assert_eq!(accepted.revision, 0);
    assert_eq!(accepted.goal_digest, request.goal.digest().unwrap());
    assert_eq!(
        store.get_user_task(session.id, accepted.id.clone()).await.unwrap(),
        accepted,
    );
}

#[tokio::test]
async fn concurrent_replay_creates_one_task_and_one_goal() {
    let Some((store, pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let request = submit_request(session.id.clone(), agent.name, None);
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let submit = |store: Arc<PostgresStore>, barrier: Arc<Barrier>| {
        let principal = principal.clone();
        let request = request.clone();
        async move {
            barrier.wait().await;
            store.submit_user_task(&principal, request).await.unwrap()
        }
    };
    let first = tokio::spawn(submit(store.clone(), barrier.clone()));
    let second = tokio::spawn(submit(store.clone(), barrier.clone()));
    barrier.wait().await;
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.unwrap().id, second.unwrap().id);
    assert_eq!(task_count(&pool, &session.id).await, 1);
    assert_eq!(goal_count(&pool, &session.id).await, 1);
}

#[tokio::test]
async fn database_rejects_goal_update_and_delete() {
    let Some((store, pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let accepted = store
        .submit_user_task(&principal, submit_request(session.id, agent.name, None))
        .await
        .unwrap();
    for statement in [
        "UPDATE goal_contracts SET objective = 'changed' WHERE task_id = $1",
        "DELETE FROM goal_contracts WHERE task_id = $1",
    ] {
        let error = sqlx::query(statement)
            .bind(accepted.id.as_uuid())
            .execute(&pool)
            .await
            .unwrap_err();
        assert_eq!(error.as_database_error().unwrap().code().as_deref(), Some("55000"));
    }
    assert_eq!(store.get_user_task(accepted.session_id, accepted.id).await.unwrap().goal,
        accepted.goal);
}
```

```rust
#[tokio::test]
async fn same_key_with_changed_semantics_conflicts_before_definition_resolution() {
    let Some((store, pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let base = submit_request(session.id, agent.name, None);
    let accepted = store.submit_user_task(&principal, base.clone()).await.unwrap();
    let mut changed = Vec::new();

    let mut request = base.clone();
    request.target = TaskTarget::GithubPullRequest {
        repository: "acme/other".into(), number: 142,
    };
    changed.push(request);
    let mut request = base.clone();
    request.preferences.harness_role = Some(HarnessRole::Reasoning);
    changed.push(request);
    let mut request = base.clone();
    request.agent.version = Some(agent.version + 99);
    changed.push(request);
    let mut request = base;
    request.goal.objective = "Changed objective".into();
    changed.push(request);

    for request in changed {
        assert!(matches!(
            store.submit_user_task(&principal, request).await,
            Err(StoreError::IdempotencyConflict),
        ));
    }
    assert_eq!(task_count(&pool, &accepted.session_id).await, 1);
    assert_eq!(goal_count(&pool, &accepted.session_id).await, 1);
}

#[tokio::test]
async fn replay_keeps_the_original_version_while_a_fresh_key_selects_latest() {
    let Some((store, _pool, session, principal, version_one)) = setup_agent_store().await else {
        return;
    };
    let original_request = submit_request(session.id.clone(), version_one.name.clone(), None);
    let original = store
        .submit_user_task(&principal, original_request.clone())
        .await
        .unwrap();
    let version_two = store.publish(changed_babysit_agent()).await.unwrap();

    let replay = store.submit_user_task(&principal, original_request).await.unwrap();
    assert_eq!(replay.definition.version, version_one.version);
    let mut fresh = submit_request(session.id, version_one.name, None);
    fresh.idempotency_key = IdempotencyKey::new("fresh-version-selection").unwrap();
    let latest = store.submit_user_task(&principal, fresh).await.unwrap();
    assert_eq!(latest.definition.version, version_two.version);
    assert_eq!(original.id, replay.id);
}
```

- [ ] **Step 7: Run and commit**

Run:

```bash
cargo test --test definition_store --test task_store --test dsl_compiler --test agent_compiler
```

PostgreSQL cases may use the repository's explicit skip only when
`DATABASE_URL` is absent; a configured database is required for release
evidence.

```bash
git add migrations/0012_user_tasks_and_goal_contracts.sql src/store/models.rs src/store/postgres.rs tests/definition_store.rs tests/task_store.rs
git commit -m "feat: persist tagged definitions and immutable goals"
```

## Task 7: Publish agents and keep execution fail-closed

**Files:**

- Modify: `src/lib.rs`
- Modify: `src/protocol/{application,transport,types,v1,v2}.rs`
- Modify: `src/store/postgres.rs`
- Create: `tests/agent_publication.rs`
- Modify: `tests/{acp_conformance,execution_store,transports}.rs`

**Interfaces:**

```rust
pub const AGENT_RUNTIME_UNAVAILABLE: &str = "agent_runtime_unavailable";

pub struct PublishDefinition {
    pub expected_kind: Option<DefinitionKind>,
    pub source: String,
}

pub enum ProtocolError {
    // existing variants remain unchanged
    StableApplication {
        code: &'static str,
        message: String,
    },
}

// Test-local helpers.
async fn application_fixture() -> Option<(Application, ApplicationContext, PgPool)>;
async fn execution_fixture_with_agent() -> Option<(
    PostgresStore,
    PgPool,
    SessionView,
    PublishedDefinition,
)>;
async fn resolve_definition_for_start(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &StartRun,
) -> Result<PublishedDefinition, StoreError>;
```

- [ ] **Step 1: Add failing publication and start-boundary tests**

```rust
#[tokio::test]
async fn publication_persists_agent_as_an_immutable_tagged_definition() {
    let Some((application, bound_context, _pool)) = application_fixture().await else {
        return;
    };
    let result = application
        .dispatch(
            &bound_context,
            ApplicationCommand::Publish(PublishDefinition {
                expected_kind: None,
                source: BABYSIT.to_owned(),
            }),
        )
        .await
        .expect("foundation slice publishes compiled agents");
    let ApplicationResult::Published(definition) = result else {
        panic!("expected published definition");
    };
    assert_eq!(definition.kind, DefinitionKind::Agent);
}

#[tokio::test]
async fn legacy_run_start_rejects_agent_without_execution_rows() {
    let Some((store, pool, session, agent)) = execution_fixture_with_agent().await else {
        return;
    };
    let key = IdempotencyKey::new("must-not-run-agent").unwrap();
    let error = store
        .start_run(StartRun {
            session_id: session.id,
            workflow_name: agent.name,
            version: Some(agent.version),
            input: serde_json::json!({
                "repository": "acme/payments",
                "pull_request": 142
            }),
            idempotency_key: key,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::AgentRuntimeUnavailable));

    for table in ["runs", "frames", "events", "run_tool_bindings"] {
        let query = format!("SELECT count(*) FROM {table}");
        let count: i64 = sqlx::query_scalar(&query).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "unexpected row in {table}");
    }
}
```

- [ ] **Step 2: Observe missing tagged publication**

Run `cargo test --test agent_publication --test execution_store agent -- --nocapture`.
Expected: FAIL because the application does not yet publish tagged agents.

- [ ] **Step 3: Publish both kinds without a temporary rejection**

Compile once, validate `expected_kind` before persistence, and use the same
immutable publication path for workflows and agents. Identical source replays
the same row; changed source advances the version; exact old versions remain
readable. `_kiteframe/workflow/publish` requires workflow kind and preserves
its response bytes. `_kiteframe/definition/publish` accepts either kind and
includes `kind` in its generic response. Kind mismatch returns stable code
`definition_kind_mismatch` before persistence. Log `deprecated_workflow` at
`WARN` with definition name and source span, never source content.

```rust
let compilation = compile_report(&request.source, &catalog, &self.limits)
    .map_err(ProtocolError::Compilation)?;
if request.expected_kind.is_some_and(|expected| {
    expected != compilation.definition.definition_kind
}) {
    return Err(ProtocolError::StableApplication {
        code: "definition_kind_mismatch",
        message: "published definition kind did not match the method".to_owned(),
    });
}
for diagnostic in &compilation.diagnostics {
    if diagnostic.code == "deprecated_workflow" {
        tracing::warn!(
            definition = %compilation.definition.name,
            span_start = diagnostic.span.start,
            span_end = diagnostic.span.end,
            code = diagnostic.code,
            "deprecated definition syntax"
        );
    }
}
let published = self.store.publish(compilation.definition).await?;
Ok(ApplicationResult::Published(published))
```

- [ ] **Step 4: Guard the actual execution boundary**

In `PostgresStore::start_run`, read `definition_kind` and reject an agent after
definition resolution but before idempotency lookup, quota checks, or any
execution insert. Map it to JSON-RPC server error `-32000`, fixed message
`application request failed`, and data
`{"kiteframe_code":"agent_runtime_unavailable"}`. Do not expose source or
compiler diagnostics.

```rust
let definition = resolve_definition_for_start(&mut tx, &request).await?;
if definition.kind == DefinitionKind::Agent {
    return Err(StoreError::AgentRuntimeUnavailable);
}

// Only workflow definitions reach the existing idempotency, quota, run,
// tool-binding, frame, and run.started event code below this guard.
```

```rust
StoreError::AgentRuntimeUnavailable => ProtocolError::StableApplication {
    code: AGENT_RUNTIME_UNAVAILABLE,
    message: "agent definitions require durable goal lifecycle and runtime authority"
        .to_owned(),
},
```

- [ ] **Step 5: Prove workflow compatibility and transport parity**

Assert the old publication method and result bytes are unchanged, the support
workflow publishes and starts with its existing identity, and ACP v1/v2,
NDJSON, and WebSocket expose identical stable codes.

```rust
#[test]
fn agent_runtime_failure_has_the_same_public_shape_on_every_adapter() {
    let error = ProtocolError::StableApplication {
        code: AGENT_RUNTIME_UNAVAILABLE,
        message: "internal actionable detail".to_owned(),
    };
    for encoded in [
        V1Adapter::encode_error(&error),
        V2Adapter::new(true).encode_error(&error),
        encode_remote_error(&error),
    ] {
        assert_eq!(encoded.code, -32000);
        assert_eq!(encoded.message, "application request failed");
        assert_eq!(
            encoded.data,
            Some(serde_json::json!({
                "kiteframe_code": "agent_runtime_unavailable"
            })),
        );
    }
}
```

- [ ] **Step 6: Run and commit**

Run:

```bash
cargo test --all-features --test agent_publication --test definition_store --test execution_store --test acp_conformance --test transports --test sequential_runtime --test structured_runtime
```

```bash
git add src/lib.rs src/protocol/application.rs src/protocol/transport.rs src/protocol/types.rs src/protocol/v1.rs src/protocol/v2.rs src/store/postgres.rs tests/agent_publication.rs tests/acp_conformance.rs tests/execution_store.rs tests/transports.rs
git commit -m "feat: publish immutable agent definitions"
```

## Task 8: Expose authenticated task submit and get

**Files:**

- Modify: `src/lib.rs`
- Modify: `src/protocol/{application,transport,types,v1,v2}.rs`
- Modify: `tests/{acp_conformance,transports}.rs`

**Interfaces:**

```rust
pub struct GetUserTask {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
}

// ApplicationCommand
SubmitTask(SubmitUserTask),
GetTask(GetUserTask),

// ApplicationResult
Task(AcceptedUserTask),
```

Wire methods are `_kiteframe/task/submit` and `_kiteframe/task/get`.

Test-local transport helpers use the production `TransportKind::Stdio` for
NDJSON framing and `TransportKind::WebSocket` for WebSocket framing:

```rust
struct TaskTransportFixture;

impl TaskTransportFixture {
    async fn call(&self, method: &str, params: serde_json::Value)
        -> serde_json::Value;
    fn other_session_id(&self) -> SessionId;
    fn captured_logs(&self) -> String;
}

async fn transport_fixture_for(kind: TransportKind) -> TaskTransportFixture;
fn accepted_task_fixture() -> AcceptedUserTask;
```

- [ ] **Step 1: Add failing v1/v2 decoding tests**

```rust
#[test]
fn task_submission_decodes_without_a_wire_principal() {
    let payload: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/task/babysit_pr.submit.json"
    ))
    .unwrap();
    for command in [
        V1Adapter::decode_extension("_kiteframe/task/submit", payload.clone()).unwrap(),
        V2Adapter::new(true)
            .decode_extension("_kiteframe/task/submit", payload.clone())
            .unwrap(),
    ] {
        let ApplicationCommand::SubmitTask(request) = command else {
            panic!("expected SubmitTask");
        };
        assert_eq!(request.agent.name, "babysit_pr");
        assert_eq!(request.goal.version, 1);
    }

    let mut invalid = payload;
    invalid.as_object_mut().unwrap().insert(
        "principal".to_owned(),
        serde_json::json!("spoofed-user"),
    );
    assert!(matches!(
        V1Adapter::decode_extension("_kiteframe/task/submit", invalid),
        Err(ProtocolError::InvalidPayload(_)),
    ));
}
```

- [ ] **Step 2: Observe unsupported methods**

Run `cargo test --all-features --test acp_conformance task_submission -- --nocapture`.
Expected: FAIL with unsupported `_kiteframe/task/submit`.

- [ ] **Step 3: Bind both commands to the connection**

Deserialize `SubmitUserTask` directly. Add a closed camel-case `GetTaskDto`
with session id, task id, and optional `_meta`. Extend `command_session` so
submit/get must match the bound session. Dispatch submission with
`context.connection_principal`; retrieve through the session-scoped joined
read.

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetTaskDto {
    session_id: SessionId,
    task_id: UserTaskId,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

match method {
    "_kiteframe/task/submit" => {
        let request = serde_json::from_value::<SubmitUserTask>(params)
            .map_err(invalid_payload)?;
        Ok(ApplicationCommand::SubmitTask(request))
    }
    "_kiteframe/task/get" => {
        let dto = serde_json::from_value::<GetTaskDto>(params)
            .map_err(invalid_payload)?;
        Ok(ApplicationCommand::GetTask(GetUserTask {
            session_id: dto.session_id,
            task_id: dto.task_id,
        }))
    }
    _ => decode_existing_method(method, params),
}
```

Add `SubmitTask(request) => &request.session_id` and
`GetTask(request) => &request.session_id` to `command_session`. Dispatch with:

```rust
ApplicationCommand::SubmitTask(request) => self
    .store
    .submit_user_task(&context.connection_principal, request)
    .await
    .map(ApplicationResult::Task)
    .map_err(ProtocolError::from),
ApplicationCommand::GetTask(request) => self
    .store
    .get_user_task(request.session_id, request.task_id)
    .await
    .map(ApplicationResult::Task)
    .map_err(ProtocolError::from),
```

- [ ] **Step 4: Use one result and error shape**

Both adapters call shared result encoding. The accepted task contains id,
session, authenticated principal, tagged exact definition, target,
preferences, full goal, goal digest, revision, and creation time. It never
contains the idempotency key or database state name.

Map validation to JSON-RPC `-32602` with `Invalid params`. Map missing or wrong
definition, missing task, and conflicting replay to `-32000` with fixed
`application request failed` and stable data codes
`agent_definition_required`, `definition_not_found`, `user_task_not_found`,
and `idempotency_conflict`. Errors and logs contain no objective, constraints,
repository, source, or idempotency text.

```rust
fn stable_store_error(error: &StoreError) -> Option<&'static str> {
    match error {
        StoreError::AgentDefinitionRequired => Some("agent_definition_required"),
        StoreError::DefinitionNotFound => Some("definition_not_found"),
        StoreError::UserTaskNotFound => Some("user_task_not_found"),
        StoreError::IdempotencyConflict => Some("idempotency_conflict"),
        _ => None,
    }
}

#[test]
fn accepted_task_encoding_is_shared_and_omits_retry_state() {
    let encoded = encode_application_result(ApplicationResult::Task(
        accepted_task_fixture(),
    ))
    .unwrap();
    assert_eq!(encoded["principal"], "user_456");
    assert_eq!(encoded["agent"]["kind"], "agent");
    assert_eq!(encoded["goal"]["version"], 1);
    assert_eq!(encoded["goal"]["success"].as_array().unwrap().len(), 3);
    assert_eq!(encoded["revision"], 0);
    assert!(encoded.get("idempotencyKey").is_none());
    assert!(encoded.get("state").is_none());
}

#[test]
fn task_errors_use_fixed_messages_and_stable_codes() {
    for (error, stable) in [
        (StoreError::AgentDefinitionRequired, "agent_definition_required"),
        (StoreError::DefinitionNotFound, "definition_not_found"),
        (StoreError::UserTaskNotFound, "user_task_not_found"),
        (StoreError::IdempotencyConflict, "idempotency_conflict"),
    ] {
        let encoded = encode_remote_error(&ProtocolError::from(error));
        assert_eq!(encoded.code, -32000);
        assert_eq!(encoded.message, "application request failed");
        assert_eq!(encoded.data.unwrap()["kiteframe_code"], stable);
    }
}
```

- [ ] **Step 5: Add transport and isolation coverage**

Test ACP v1/v2, NDJSON, and WebSocket submit/get success, identical replay,
session mismatch, principal isolation, unknown nested keys, invalid goal, and
missing task. Add the four extension method names from Tasks 7 and 8 to the
logged-method allowlist.

```rust
#[tokio::test]
async fn remote_submit_replays_and_get_is_session_scoped() {
    let fixture = transport_fixture_for(TransportKind::Stdio).await;
    let payload: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/task/babysit_pr.submit.json"
    ))
    .unwrap();
    let first = fixture.call("_kiteframe/task/submit", payload.clone()).await;
    let replay = fixture.call("_kiteframe/task/submit", payload).await;
    assert_eq!(first["result"]["id"], replay["result"]["id"]);

    let foreign = fixture
        .call("_kiteframe/task/get", serde_json::json!({
            "sessionId": fixture.other_session_id(),
            "taskId": first["result"]["id"],
        }))
        .await;
    assert_eq!(foreign["error"]["data"]["kiteframe_code"], "user_task_not_found");
}
```

```rust
#[tokio::test]
async fn every_remote_transport_redacts_invalid_task_content() {
    for kind in [TransportKind::Stdio, TransportKind::WebSocket] {
        let fixture = transport_fixture_for(kind).await;
        let mut payload: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/task/babysit_pr.submit.json"
        ))
        .unwrap();
        payload["goal"]["objective"] = serde_json::json!(" ");
        let response = fixture.call("_kiteframe/task/submit", payload).await;
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["message"], "Invalid params");

        let logs = fixture.captured_logs();
        for secret in [
            "Address actionable review feedback",
            "acme/payments",
            "Never merge the pull request",
            "submit-babysit-pr-142",
        ] {
            assert!(!logs.contains(secret), "log leaked: {secret}");
        }
    }
}
```

- [ ] **Step 6: Run and commit**

Run `cargo test --all-features --test acp_conformance --test transports`; all
pass.

```bash
git add src/lib.rs src/protocol/application.rs src/protocol/transport.rs src/protocol/types.rs src/protocol/v1.rs src/protocol/v2.rs tests/acp_conformance.rs tests/transports.rs
git commit -m "feat: expose durable user task submission"
```

## Task 9: Prove compatibility, recovery, and non-authority

**Files:**

- Create: `examples/babysit_pr.kite`
- Modify: `README.md`
- Modify: `docs/reference/{kite-language,user-task-and-goal-contract}.md`
- Modify: `docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md`
- Modify: `tests/{agent_compiler,dsl_compiler,e2e_restart,repository_shape,sequential_runtime,structured_runtime,task_store}.rs`

**Test-local helpers:**

```rust
fn support_catalog() -> ToolCatalogSnapshot;
fn github_catalog() -> ToolCatalogSnapshot;
```

- [ ] **Step 1: Add the failing restart test**

```rust
#[tokio::test]
async fn accepted_task_and_goal_survive_restart_without_becoming_runnable() {
    let Some(fixture) = RestartFixture::new().await else {
        return;
    };
    let accepted = fixture.submit_canonical_task().await;
    let database_url = fixture.database_url.clone();
    let verification_pool = fixture.verification_pool.clone();
    drop(fixture.store);

    let reopened = PostgresStore::connect(&database_url).await.unwrap();
    let restored = reopened
        .get_user_task(accepted.session_id.clone(), accepted.id.clone())
        .await
        .unwrap();

    assert_eq!(restored, accepted);
    let run_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runs WHERE session_id = $1",
    )
    .bind(accepted.session_id.as_uuid())
    .fetch_one(&verification_pool)
    .await
    .unwrap();
    assert_eq!(run_count, 0);
}
```

- [ ] **Step 2: Observe missing restart fixture support**

Run:

```bash
cargo test --test e2e_restart accepted_task_and_goal_survive_restart_without_becoming_runnable -- --exact
```

Expected: FAIL to compile because `RestartFixture` has not been added.

- [ ] **Step 3: Prove restart, isolation, and failure atomicity**

After reopening, verify every accepted field and digest. Another session for
the same principal and a session for another principal both receive
`UserTaskNotFound`. Invalid goal, missing agent, workflow-as-agent, closed
session, wrong principal, and conflicting replay leave no orphan task or goal.
Acceptance creates no runs, frames, events, model items, tool calls, or
permission requests. `repository_shape` also proves the application command
surface has no `CompleteGoal`, `BlockGoal`, `PauseTask`, or `ResumeTask`
variant in this slice.

```rust
struct RestartFixture {
    store: PostgresStore,
    verification_pool: sqlx::PgPool,
    database_url: String,
    session: SessionView,
    principal: PrincipalId,
    agent: PublishedDefinition,
}

impl RestartFixture {
    async fn new() -> Option<Self> {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return None;
        };
        let store = PostgresStore::connect(&database_url).await.unwrap();
        let verification_pool = PgPool::connect(&database_url).await.unwrap();
        let principal = PrincipalId::new(format!("restart-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/restart-agent-foundation".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let agent = store.publish(compiled_babysit_agent()).await.unwrap();
        Some(Self {
            store,
            verification_pool,
            database_url,
            session,
            principal,
            agent,
        })
    }

    async fn submit_canonical_task(&self) -> AcceptedUserTask {
        self.store
            .submit_user_task(
                &self.principal,
                submit_request(
                    self.session.id.clone(),
                    self.agent.name.clone(),
                    Some(self.agent.version),
                ),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn task_reads_are_session_and_principal_scoped_after_restart() {
    let Some(fixture) = RestartFixture::new().await else {
        return;
    };
    let accepted = fixture.submit_canonical_task().await;
    let same_principal_session = fixture
        .store
        .create_session(CreateSession {
            principal: fixture.principal.clone(),
            working_directory: "/restart-same-principal".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let other_principal = PrincipalId::new(format!("other-{}", Uuid::now_v7())).unwrap();
    let other_session = fixture
        .store
        .create_session(CreateSession {
            principal: other_principal.clone(),
            working_directory: "/restart-other-principal".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();

    for session_id in [same_principal_session.id, other_session.id] {
        assert!(matches!(
            fixture.store.get_user_task(session_id, accepted.id.clone()).await,
            Err(StoreError::UserTaskNotFound),
        ));
    }
}

#[test]
fn foundation_exposes_no_goal_lifecycle_commands() {
    let source = std::fs::read_to_string("src/protocol/types.rs").unwrap();
    for forbidden in ["CompleteGoal", "BlockGoal", "PauseTask", "ResumeTask"] {
        assert!(!source.contains(forbidden), "unexpected command: {forbidden}");
    }
}
```

```rust
async fn task_and_goal_counts(pool: &PgPool, session_id: &SessionId) -> (i64, i64) {
    let tasks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_tasks WHERE session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    let goals: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM goal_contracts g \
         JOIN user_tasks t ON t.id = g.task_id WHERE t.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    (tasks, goals)
}

#[tokio::test]
async fn rejected_submissions_leave_no_partial_task_or_goal() {
    let Some(fixture) = RestartFixture::new().await else {
        return;
    };

    let mut invalid_goal = submit_request(
        fixture.session.id.clone(), fixture.agent.name.clone(), None,
    );
    invalid_goal.goal.success.clear();
    assert!(matches!(
        fixture.store.submit_user_task(&fixture.principal, invalid_goal).await,
        Err(StoreError::InvalidUserTask(_)),
    ));

    let missing = submit_request(
        fixture.session.id.clone(), "missing_agent".into(), None,
    );
    assert!(matches!(
        fixture.store.submit_user_task(&fixture.principal, missing).await,
        Err(StoreError::DefinitionNotFound),
    ));

    let workflow = fixture.store.publish(compiled_support_workflow()).await.unwrap();
    let workflow_request = submit_request(
        fixture.session.id.clone(), workflow.name, Some(workflow.version),
    );
    assert!(matches!(
        fixture.store.submit_user_task(&fixture.principal, workflow_request).await,
        Err(StoreError::AgentDefinitionRequired),
    ));

    let wrong_principal = PrincipalId::new("wrong-principal").unwrap();
    let wrong_principal_request = submit_request(
        fixture.session.id.clone(), fixture.agent.name.clone(), None,
    );
    assert!(matches!(
        fixture.store.submit_user_task(&wrong_principal, wrong_principal_request).await,
        Err(StoreError::PrincipalMismatch),
    ));

    fixture.store
        .close_session(fixture.session.id.clone(), &fixture.principal)
        .await
        .unwrap();
    let closed = submit_request(
        fixture.session.id.clone(), fixture.agent.name.clone(), None,
    );
    assert!(matches!(
        fixture.store.submit_user_task(&fixture.principal, closed).await,
        Err(StoreError::SessionClosed),
    ));

    assert_eq!(
        task_and_goal_counts(&fixture.verification_pool, &fixture.session.id).await,
        (0, 0),
    );
}
```

- [ ] **Step 4: Prove the legacy matrix**

The support workflow retains DSL version 1, workflow kind, existing hash and
golden bytes; executes call, parallel, ask, goto, done, and fail; and resumes
the same frames after restart. It emits no agent-only fields or events. Run
these tests before changing any expected legacy output; diagnose any failure
instead of updating the golden.

```rust
#[test]
fn legacy_workflow_identity_and_json_remain_version_one() {
    let compiled = compile(SUPPORT, &support_catalog(), &ceilings()).unwrap();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/dsl/support.compiled.json"
    ))
    .unwrap();
    assert_eq!(compiled.dsl_version, 1);
    assert_eq!(compiled.definition_kind, DefinitionKind::Workflow);
    assert_eq!(compiled.definition_hash, expected["definition_hash"]);
    assert_eq!(serde_json::to_value(&compiled).unwrap(), expected);
    let json = serde_json::to_value(compiled).unwrap();
    assert!(json.get("definition_kind").is_none());
    assert!(json.get("events").is_none());
}
```

Extend the existing focused tests in place instead of introducing a combined
runtime fixture:

- `permitted_model_tool_call_replays_output_then_completes` keeps its current
  `done` assertion and adds a child `call` before completion, asserting the
  child frame completes before the root output is committed.
- `parallel_children_complete_before_the_parent_resumes` remains the
  `parallel` compatibility proof.
- `scoped_answer_resumes_a_durable_question` remains the `ask` compatibility
  proof.
- `durable_step_ceiling_fails_a_goto_loop_across_model_turns` remains the
  durable `goto` proof.
- Add `legacy_fail_statement_durably_fails_the_run` beside the other
  `sequential_runtime` tests by reusing that file's existing store/session
  setup and this source:

  ```rust
  let source = format!(
      "workflow {name}() -> string {{ start t() }} \
       task t() -> string {{ prompt \"\"\"x\"\"\" tools [] \
       on rejected() {{ fail \"legacy_failure\" \"expected\" }} }}"
  );
  ```

  Script one `complete_task` call for outcome `rejected`, tick the engine, and
  assert persisted run failure equals
  `{"code":"legacy_failure","message":"expected"}`.
- `restart_reconstructs_principal_scoped_run_and_cursor_replay` remains the
  restart/frame/cursor proof and gains an assertion that the restored
  definition kind is `Workflow`.

Run the focused matrix directly:

```bash
cargo test --test sequential_runtime permitted_model_tool_call_replays_output_then_completes -- --exact
cargo test --test sequential_runtime scoped_answer_resumes_a_durable_question -- --exact
cargo test --test sequential_runtime legacy_fail_statement_durably_fails_the_run -- --exact
cargo test --test structured_runtime parallel_children_complete_before_the_parent_resumes -- --exact
cargo test --test structured_runtime durable_step_ceiling_fails_a_goto_loop_across_model_turns -- --exact
cargo test --test e2e_restart restart_reconstructs_principal_scoped_run_and_cursor_replay -- --exact
```

- [ ] **Step 5: Publish accurate documentation**

Copy the reviewed agent source to `examples/babysit_pr.kite` and add a compiler
test that loads that file and asserts `DefinitionKind::Agent`. Review README
and both references for three truthful distinctions: workflow execution is
implemented; agent publication and immutable task intent are implemented but
inert; lifecycle, agent execution, plugins, channels, workers, budgets,
connectors, Codex, Cursor, and VS Code remain unimplemented. Document the two
task methods and generic definition publication as private extension methods,
not stable plugin protocol. Add only final commit ids and executed verification
commands to the design's non-normative implementation-status note.

```rust
#[test]
fn published_authoring_example_compiles_as_an_agent() {
    let source = std::fs::read_to_string("examples/babysit_pr.kite").unwrap();
    let compiled = compile(&source, &github_catalog(), &ceilings()).unwrap();
    assert_eq!(compiled.definition_kind, DefinitionKind::Agent);
}
```

- [ ] **Step 6: Run the complete gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cd clients/python && uv run ruff check . && uv run pyright && uv run pytest
cd clients/typescript && bun run typecheck && bun test && bun run build
git diff --check
git status --short
git diff --stat
```

All commands pass. PostgreSQL tests run against a disposable configured
database. Only listed Slice 1 paths change; `.worktrees/`, `fuzz/`, and `site/`
remain untouched.

- [ ] **Step 7: Commit the evidence**

```bash
git add README.md examples/babysit_pr.kite docs/reference/kite-language.md docs/reference/user-task-and-goal-contract.md docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md tests/agent_compiler.rs tests/dsl_compiler.rs tests/e2e_restart.rs tests/repository_shape.rs tests/sequential_runtime.rs tests/structured_runtime.rs tests/task_store.rs
git commit -m "docs: close declarative agent foundation"
```

## Slice 1 acceptance gate

Slice 1 is complete only when:

- the reviewed syntax, request fixture, validation ceilings, and references
  agree;
- exactly one agent with nested tasks, typed transitions, context modes,
  semantic roles, and event declarations compiles as version 2;
- every legacy workflow remains version 1 with unchanged identity, publication,
  wire output, and execution behavior;
- workflow parsing emits one bounded `deprecated_workflow` warning;
- agent definitions publish immutably, while legacy run start rejects them
  before any execution row with `agent_runtime_unavailable`;
- an authenticated principal can submit the bounded V1 GitHub pull-request
  target and optional semantic harness role without a wire principal;
- acceptance atomically freezes one exact agent version and inserts exactly one
  immutable goal contract;
- idempotent replay, concurrency, invalid input, isolation, and restart tests
  preserve the original snapshot and leave no partial rows;
- PostgreSQL rejects direct goal update and deletion;
- ACP v1/v2, NDJSON, WebSocket, Rust, PostgreSQL, Python, and TypeScript gates
  pass; and
- documentation makes no claim that goal lifecycle, evidence-backed
  completion, agent execution, plugins, channels, workers, connectors, budgets,
  Codex, Cursor, or VS Code are operational.

The next planning artifact is consolidated Slice 2: authoritative goal
lifecycle and the agent-runtime transition contract. It starts only after this
gate is reviewed.
