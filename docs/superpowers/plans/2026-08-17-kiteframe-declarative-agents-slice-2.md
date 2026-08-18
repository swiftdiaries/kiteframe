# Kiteframe Declarative Agents Slice 2 Authoritative Lifecycle Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn Slice 1's inert `AcceptedUserTask` into one durable, revisioned
agent execution behind an internal `AgentRuntime` state-machine contract, with
the Rust kernel alone validating and committing task lifecycle, goal phase,
runtime snapshots, commands, evidence, policy decisions, revisions, and
ordered events.

**Architecture:** A pure, deterministic built-in `.kite` runtime validates the
exact immutable `CompiledDefinition`, replaces a complete canonical runtime
snapshot on each transition, and proposes an ordered closed command list. A
new agent kernel validates the proposal and commits it through PostgreSQL
compare-and-set transactions; current-state rows remain authoritative while
the transition, command, blocker, assertion, and event journals are immutable
audit evidence. The current direct model loop is reachable only through a
narrow internal harness-executor seam, so Slice 3 can replace that seam with
the versioned plugin boundary without moving lifecycle authority out of Rust.

**Tech Stack:** Rust 1.97.1 / edition 2024, Serde/serde_json 1, UUID v7,
RFC 8785 canonical JSON and SHA-256 through Kiteframe's existing hash module,
SQLx 0.8 with PostgreSQL, Tokio 1, async-trait 0.1, ACP v1 and feature-gated
ACP v2, the existing Responses-compatible `ModelClient`, and the existing
Python and TypeScript compatibility gates; no new dependency.

**Spec:**
`docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md`

**Planning status (non-normative, 2026-08-18):** Consolidated Slice 1 is
actively being implemented in Codex task `01a010d4-422a-7750-8e2c-a942a29f0d7f`.
That work is not a satisfied dependency until its complete acceptance gate is
independently reviewed, including configured PostgreSQL runtime evidence, and
its final contracts are present in the target checkout. The task id is
coordination metadata only; source, tests, persistence, and protocols must not
depend on it.

## Global Constraints

- The approved design is normative. This plan implements consolidated V1
  Slice 2 only.
- Slice 1 is a hard prerequisite. Its active implementation is progress, not
  acceptance evidence for this checkout. Begin Task 1 only after every Slice 1
  acceptance item—including configured PostgreSQL cases—passes review here and
  the exact promised `CompiledDefinition`, `DefinitionKind`, `ModelRole`,
  `ContextMode`, `PublishedDefinition`, `AcceptedUserTask`, `GoalContract`,
  `UserTaskId`, `user_tasks`, `goal_contracts`, immutable publication, and
  private submit/get interfaces exist.
- Start an agent only from the exact `PublishedDefinition` already frozen in
  `AcceptedUserTask`. Never resolve the latest version during start or any
  later transition.
- `GoalPhase` is exactly `Active | Paused | Blocked | Complete`. Operational
  waits never become goal phases. Task failure and cancellation never become
  goal completion.
- Task lifecycle is exactly `Accepted | Running | Succeeded | Failed |
  Cancelled`. Operational status is a separate axis and is exactly
  `NotStarted | Runnable | WaitingForContext | WaitingForEvent |
  WaitingForHarness | WaitingForApproval | Paused | Blocked | Finished`.
- Root `done` ends `.kite` traversal and may make the task `Succeeded`; it MUST
  NOT infer, propose, or commit `GoalPhase::Complete`. Goal completion requires
  an explicit `CompleteGoal` command and exact evidence for every criterion.
- Agent-only `complete goal` and indexed `block goal when` statements
  syntax is the sole authored route to goal completion and blocking in this
  slice. The blocker index is zero-based into immutable `blocked_when`.
- `current_evidence` is a reserved, immutable string binding available only
  while reducing a validated `HarnessResult`; the runtime sets it to that
  event's kernel-minted evidence UUID after result-schema validation. Source,
  outcome, task input, and handler fields cannot declare or shadow it. A task
  may `done current_evidence` so structured parent calls can collect distinct
  evidence ids for explicit completion.
- The runtime is deterministic and side-effect free. It never calls a model,
  tool, connector, store, clock, random-number generator, policy service, or
  transport.
- Every `RuntimeTransition` carries current expected task and goal revisions,
  one full replacement snapshot, and an ordered list of closed
  `KernelCommand`s. PostgreSQL commits the snapshot, commands, transition,
  revisions, lifecycle effects, and task event atomically.
- Snapshot schema version is `1`, runtime id is `kite.v1`, maximum canonical
  snapshot size is 262,144 bytes, maximum commands per transition is 64,
  maximum canonical command size is 65,536 bytes, maximum frames is 256, and
  maximum projected history items per frame is 256. Deployment configuration
  may lower, never raise, these Slice 2 ceilings.
- The current snapshot row's `task_revision` always equals the current task
  revision. Principal/session lifecycle actions that do not run the runtime
  advance that column atomically while leaving snapshot payload, applied
  ceiling, canonical byte count, and digest unchanged.
- Runtime state is normalized with existing RFC 8785 canonical JSON and bound
  to a SHA-256 digest before persistence. Stored payload, byte count, schema
  version, runtime id, and digest must agree on every read and write.
- `RequestContext`, `WaitForEvent`, `InvokeHarness`, `RequestApproval`,
  `CompleteGoal`, `BlockGoal`, and `FailTask` are the only command variants.
  Unknown variants, fields, schema versions, and incompatible payloads fail
  closed.
- `RequestContext`, `WaitForEvent`, and `RequestApproval` have no downstream
  executor in Slice 2. If they reach dispatch, the kernel atomically fails the
  command and task with `RuntimeCommandUnavailable`; it does not leave an
  ambiguous pending wait or invent a fallback.
- Slice 2 dispatches persisted `InvokeHarness` only through the internal direct
  loop bridge. A non-completion model tool call is unavailable in this bridge
  and fails closed; Slice 3 owns full harness extraction and tool-result
  conformance.
- Durable evidence metadata is minimal and exactly: evidence id, owning task,
  evidence type, authorized source identity, and content digest. Slice 2 does
  not add content collection, provenance documents, classification, expiry,
  connector storage, or provider payloads.
- The built-in bridge may mint only `harness.invocation_result` evidence, with
  source identity bound to the completed `InvokeHarness` command. It cannot
  mint GitHub or other connector evidence types.
- `CompleteGoal` requires current revisions, exact criterion coverage, one
  task-owned evidence reference per criterion, matching evidence type, an
  authorized source, and current policy permission. Missing, extra, duplicate,
  foreign, stale, wrong-type, or unauthorized evidence leaves every row and
  revision unchanged.
- `BlockGoal` requires a current zero-based `blocked_when` index, a stable
  reason code, non-empty explanation, at least one task-owned authorized
  evidence reference, and at least one non-empty unlock condition. Blocker
  history is append-only.
- Pause, resume, and cancel are authenticated principal actions with expected
  task and goal revisions and idempotency keys. `AgentRuntime` has no variant
  that can impersonate those actions.
- All agent mutations lock in the order session -> user task -> goal state;
  commands and subordinate rows are locked afterward in stable UUID/order
  order. Session close uses the same prefix before legacy run -> frame locks.
- Recovery loads the current snapshot and pending or claimed commands exactly
  as committed. It never reconstructs authority by replaying runtime events or
  rerunning an already committed transition.
- Preserve every valid legacy workflow byte, hash, SQL row, state transition,
  frame history, run event, ACP response, stable failure, Python call, and
  TypeScript call. The legacy `StartRun` agent rejection remains intact.
- Slice 3 receives a durable `InvokeHarness` payload containing semantic
  `ModelRole`, prompt, typed input and result schemas, tools, projected
  history, and structural limits; `RuntimeEvent::HarnessResult` contains no
  task or goal phase and cannot masquerade as completion.
- Slice 3 also receives revisioned, idempotent pending-command list, claim,
  complete, and fail APIs. `.kite` source and every Slice 2 payload contain no
  runtime choice, provider identity, model name, credential, endpoint, or
  subscription identity.
- Plugin registry, capability negotiation, common plugin message envelope,
  out-of-process runtime transport, harness plugin extraction, ACP adapter,
  Codex, context artifacts, semantic event ingress, channels, bindings,
  connectors, GitHub, IDE work items, leases, fencing, mutation approval,
  `outcome_unknown`, budgets, profiles, residency, secrets, editor code, and
  the `babysit_pr` product loop are excluded.
- Use TDD: add each named failing test first, observe the stated failure, make
  the minimum implementation, run the focused and cumulative gates, then
  commit only the listed paths.
- Preserve unrelated `.worktrees/`, `fuzz/`, and `site/` content. Never stage,
  delete, reset, clean, or reformat them.

## Slice 1 prerequisite gate

Before implementation, run:

```bash
git log -1 --oneline -- docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-1.md
test -f migrations/0012_user_tasks_and_goal_contracts.sql
rg -n "pub struct AcceptedUserTask|pub struct GoalContract|pub enum DefinitionKind|pub struct CompiledDefinition" src
test -n "${DATABASE_URL:-}" || { echo "DATABASE_URL must target disposable PostgreSQL" >&2; exit 1; }
cargo test --all-features --test agent_compiler --test agent_publication --test goal_contract --test task_store
git status --short
```

Expected: migration `0012` and every promised type exist; all named tests pass;
the PostgreSQL cases execute rather than skip; the only unrelated status
entries remain the user's pre-existing paths. If the gate fails, stop Slice 2
and finish/review Slice 1 first. Do not recreate a second foundation inside
this plan.

## Reviewed Slice 2 language additions

The grammar review resolves the design's completion-authority ambiguity with
two terminal, agent-only operations:

```kite
on verify_goal() {
  fork {
    reviews = call collect_reviews() context isolated
    checks = call collect_checks() context isolated
    merged = call collect_merge() context isolated
  } join
  complete goal {
    reviews_resolved: reviews
    checks_passed: checks
    pull_request_merged: merged
  }
}

task collect_reviews() -> string {
  model reasoning
  prompt """Collect one bounded harness result for review status."""
  tools []
  on observed() { done current_evidence }
}

task collect_checks() -> string {
  model reasoning
  prompt """Collect one bounded harness result for check status."""
  tools []
  on observed() { done current_evidence }
}

task collect_merge() -> string {
  model reasoning
  prompt """Collect one bounded harness result for merge status."""
  tools []
  on observed() { done current_evidence }
}

on no_safe_path(explanation: string) {
  block goal when 0 {
    code: "required_actor_unavailable"
    explanation: explanation
    evidence: [current_evidence]
    unlock: ["Required user or reviewer action becomes available."]
  }
}
```

`complete goal` requires one or more unique criterion ids matching
`^[a-z][a-z0-9_]{0,63}$`; each value is a string binding containing an evidence
UUID. `block goal` requires a decimal zero-based index that fits `u16`, a
lower-snake-case reason code of at most 64 bytes, string-valued explanation,
one or more string evidence UUID bindings, and one or more string unlock
conditions. The compiler adds only the reserved `current_evidence: string` to
an agent outcome-handler scope and rejects any source declaration with that
name. Both statements are terminal. They are rejected in `workflow`
syntax. Compiler checks prove only the closed shape and string types; the
kernel checks the accepted goal contract, evidence rows, source authority,
policy, and revisions at execution time.
The structured-call example can complete only criteria whose declared evidence
type is `harness.invocation_result` in Slice 2. A source author may carry
distinct returned evidence UUIDs, but cannot relabel them as GitHub evidence.

## File responsibility map

| File | Responsibility in Slice 2 |
|---|---|
| `README.md` | Truthful implemented/prerequisite/deferred Slice 2 status and private lifecycle entry points. |
| `docs/reference/agent-runtime-and-goal-lifecycle.md` | Normative internal state-machine, revision, command, evidence, and recovery contract. |
| `docs/reference/kite-language.md` | Exact `complete goal` and `block goal` grammar and the rule that root `done` is not goal completion. |
| `docs/reference/user-task-and-goal-contract.md` | Goal/task/operational axes, principal actions, evidence authority, and terminal semantics. |
| `docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md` | Non-normative implementation evidence only after the Slice 2 implementation commits exist. |
| `examples/babysit_pr.kite` | Reviewed explicit goal syntax without claiming unavailable GitHub evidence authority. |
| `migrations/0013_agent_lifecycle.sql` | Goal state, runtime snapshot, transition, command, policy-decision, evidence, assertion, blocker, action, and task-event persistence. |
| `src/agent/mod.rs` | Public module boundary and exports. |
| `src/agent/types.rs` | Closed version-1 runtime snapshot, event, command, harness payload, evidence, action, and read-model types. |
| `src/agent/runtime.rs` | Pure `AgentRuntime` trait, canonicalization/ceiling validation, transition validation, and stable runtime errors. |
| `src/agent/kite.rs` | Deterministic built-in `.kite` stack/fork/join/return/context state machine. |
| `src/agent/kernel.rs` | Runtime orchestration, PostgreSQL CAS handoff, command completion, recovery, and unavailable-command closure. |
| `src/engine/harness.rs` | Extractable internal `HarnessCommandExecutor` and direct `ModelClient` bridge. |
| `src/engine/mod.rs` | Shared legacy/direct completion parsing without changing legacy model-loop semantics. |
| `src/goal.rs` | Slice 1 contract plus `GoalPhase`, evidence ids/records, criterion assertions, and blocker views. |
| `src/lib.rs` | Public crate exports for the new internal lifecycle modules and types. |
| `src/main.rs` | Startup recovery and shutdown-cancellable scheduling for legacy and agent work. |
| `src/task.rs` | Slice 1 request plus lifecycle, operational status, start/pause/resume/cancel requests, and task read model. |
| `src/dsl/ast.rs` | AST nodes for the two explicit agent-only goal statements. |
| `src/dsl/parser.rs` | Definition-aware parsing and exact diagnostics for the reviewed grammar. |
| `src/dsl/compiler.rs` | String/type/cardinality checks and lowering into closed goal-command IR. |
| `src/dsl/ir.rs` | `CompiledStatement::CompleteGoal` and `CompiledStatement::BlockGoal`; legacy serialization remains unchanged. |
| `src/dsl/mod.rs` | DSL module exports for the reviewed AST/IR additions. |
| `src/store/agent.rs` | Agent-specific SQL mapping, lock helpers, atomic start/transition/actions, command APIs, evidence checks, and recovery reads. |
| `src/store/mod.rs` | Store module registration and public re-exports. |
| `src/store/models.rs` | Store-facing agent execution, command, transition, evidence, action, and event models plus stable errors. |
| `src/store/postgres.rs` | Existing store construction and exact compiled-definition loading used by `src/store/agent.rs`. |
| `src/store/session_close.rs` | Session -> task -> goal -> run -> frame cancellation lock order. |
| `src/config.rs` | Bounded environment parsing for deployers lowering, never raising, runtime ceilings. |
| `src/protocol/types.rs` | Private start/lifecycle/event application commands, shared result encoding, and coarse stable errors. |
| `src/protocol/application.rs` | Authenticated principal binding and dispatch into `AgentKernel`. |
| `src/protocol/v1.rs` | ACP v1 private extension decoding. |
| `src/protocol/v2.rs` | Feature-gated ACP v2 private extension decoding with v1 parity. |
| `src/protocol/transport.rs` | NDJSON/WebSocket parity, connection binding, fixed error shapes, and redaction. |
| `tests/agent_runtime_contract.rs` | Closed contract, canonical digest, ceiling, and deterministic transition tests. |
| `tests/agent_lifecycle_store.rs` | Migration, mapping, lifecycle, evidence, blocker, and failure-atomicity tests. |
| `tests/agent_start.rs` | Exact frozen-definition start, idempotency, race, and lock-order tests. |
| `tests/kite_agent_runtime.rs` | Pure traversal, context, fork/join, explicit goal, root-done, and forged-event tests. |
| `tests/agent_transition_store.rs` | CAS, journal, duplicate, command ordering, command API, and restart tests. |
| `tests/agent_goal_authority.rs` | Exhaustive completion/block/evidence/pause/resume/cancel authority matrix. |
| `tests/agent_direct_bridge.rs` | Persist-before-model, result validation, harness evidence, and unavailable behavior. |
| `tests/agent_protocol.rs` | ACP v1/v2 and remote start/read/action/event conformance. |
| `tests/e2e_agent_restart.rs` | Exact snapshot and pending-command recovery without replay. |
| `tests/agent_compiler.rs` | Agent goal-authority lowering and unchanged agent-v2 definition hashing. |
| `tests/agent_parser.rs` | Reviewed syntax, reserved evidence binding, and exact diagnostics. |
| `tests/goal_contract.rs` | Contract/evidence identifier compatibility and goal-phase separation. |
| `tests/definition_store.rs` | Exact frozen compiled-definition loading and publication compatibility. |
| `tests/task_store.rs` | Slice 1 task acceptance plus atomic initial goal-state insertion. |
| `tests/session_store.rs` | Store-limit/config and session-close lifecycle compatibility. |
| `tests/postgres_lock_order.rs` | Session -> task -> goal -> command/run/frame concurrency order. |
| `tests/execution_store.rs` | Legacy run/frame rows remain independent from agent execution rows. |
| `tests/e2e_restart.rs` | Legacy restart bytes and state remain unchanged. |
| `tests/sequential_runtime.rs` | Legacy sequential direct-loop request/result compatibility. |
| `tests/structured_runtime.rs` | Legacy structured/fork request/history compatibility. |
| `tests/acp_conformance.rs` | Existing ACP methods and stable agent extension parity. |
| `tests/repository_shape.rs` | Later-slice path/field exclusions and runtime/provider-free `.kite` sources. |
| `tests/responses_conformance.rs` | Responses request compatibility after completion-helper extraction. |
| `tests/transports.rs` | NDJSON/WebSocket lifecycle parity and legacy transport stability. |
| `tests/fixtures/dsl/agent_babysit_pr.kite` | Source fixture for reviewed explicit goal operations. |
| `tests/fixtures/dsl/agent_babysit_pr.compiled.json` | Final version-2 compiled agent fixture including closed goal-command IR. |

## Dependency flow

```text
Slice 1 immutable task + goal + exact published definition
  -> closed lifecycle/runtime/command/evidence contracts + explicit goal IR
  -> lifecycle schema and typed current-state mapping
  -> atomic exact-definition StartAgent
  -> pure deterministic .kite snapshot transitions
  -> PostgreSQL CAS transition + command journal + exact recovery
  -> kernel-only complete/block and principal-only pause/resume/cancel
  -> persisted InvokeHarness direct-loop bridge
  -> private protocol/read model + compatibility and Slice 3 handoff
```

## Stable internal and external failures

The store/runtime variants are exact and remain actionable inside Rust:

```rust
pub enum StoreError {
    // Slice 1 and legacy variants remain unchanged.
    AgentTransitionInvalid(String),
    TaskRevisionConflict,
    GoalRevisionConflict,
    GoalAlreadyTerminal,
    GoalEvidenceIncomplete,
    GoalEvidenceNotFound,
    GoalEvidenceTypeMismatch,
    GoalEvidenceUnauthorized,
    GoalEvidenceStale,
    RuntimeStateInvalid(String),
    RuntimeCommandUnavailable,
    AgentTaskAlreadyTerminal,
}
```

Private transports expose only fixed JSON-RPC `-32000` message
`application request failed` with these coarse codes:

| Internal variants | `kiteframe_code` |
|---|---|
| `AgentTransitionInvalid`, `RuntimeStateInvalid` | `agent_transition_invalid` |
| `TaskRevisionConflict`, `GoalRevisionConflict` | `revision_conflict` |
| `GoalAlreadyTerminal`, `AgentTaskAlreadyTerminal` | `task_transition_invalid` |
| all five `GoalEvidence*` variants | `goal_evidence_invalid` |
| `RuntimeCommandUnavailable` | `runtime_command_unavailable` |

No error or log includes source, prompt, model output, evidence id, digest,
objective, constraint, blocker explanation, repository, or idempotency key.

## Cross-task review protocol

Slice 2 uses cumulative independent review after every four implementation
tasks. These reviews supplement each task's focused test/review cycle; they do
not replace it and do not create extra implementation tasks or commits.

The reviewer for a gate MUST be fresh to the reviewed task block: they did not
implement or fix any task in that block. Give the reviewer the complete design
spec, this complete plan, the accepted Slice 1 contracts, every reviewed task
report and commit in the block, and the exact base-to-head diff. The reviewer
must reason in both directions:

1. **Past-spec coverage:** map every applicable normative design requirement
   and Global Constraint to an implementation path, an executed test, and
   durable evidence. A passing test name without inspected behavior is not
   evidence.
2. **Cross-task consistency:** compare public/internal signatures, wire names,
   schema constraints, revision rules, lock order, stable errors, hashes,
   recovery behavior, and authority boundaries across all completed tasks.
3. **Remaining-task readiness:** verify that every interface consumed by the
   next task block exists with the exact type and semantics promised in its
   `Interfaces` blocks. Do not let completed work silently redefine a future
   task.
4. **Future-slice compatibility:** compare the reviewed work with the Slice 3
   handoff and the Slice 3-8 outcomes in the design. Reject both scope leakage
   into a future slice and a local choice that makes a future normative outcome
   impossible without breaking the reviewed contract.
5. **Compatibility and evidence:** inspect legacy workflow invariants,
   principal isolation, PostgreSQL execution rather than skipped database
   cases, deterministic fixtures, failure atomicity, restart behavior, and
   repository scope.

Each gate writes a durable Markdown report under
`.superpowers/sdd/2026-08-17-kiteframe-declarative-agents-slice-2/`. The report
must include:

- reviewed commit range and exact files;
- a requirement -> implementation -> test/evidence -> status matrix;
- cross-task signature and persistence checks;
- remaining-task and future-slice compatibility checks;
- commands actually executed and whether PostgreSQL cases ran;
- findings classified as Critical, Important, or Minor; and
- an explicit `approved` or `changes_required` verdict.

Any normative-spec gap, authority leak, cross-task contract mismatch,
compatibility regression, unexecuted required database case, or future-slice
scope violation blocks the gate regardless of severity. Other Critical or
Important findings also block. A non-normative Minor finding may be deferred
only when the report records the rationale, owner task/gate, and proof that it
cannot change a reviewed interface or acceptance outcome.

For a blocked gate, implement the smallest scoped fix, rerun the focused and
cumulative commands, record the fix commit and evidence in a sibling
`*-fix-report.md`, and dispatch a fresh re-review. Execution cannot cross the
gate until the latest report says `approved` with no blocking finding.

---

### Task 1: Freeze lifecycle, command, evidence, and explicit goal contracts

**Files:**

- Create: `docs/reference/agent-runtime-and-goal-lifecycle.md`
- Create: `src/agent/mod.rs`
- Create: `src/agent/types.rs`
- Create: `src/agent/runtime.rs`
- Modify: `docs/reference/kite-language.md`
- Modify: `docs/reference/user-task-and-goal-contract.md`
- Modify: `src/goal.rs`
- Modify: `src/lib.rs`
- Modify: `src/task.rs`
- Modify: `src/dsl/ast.rs`
- Modify: `src/dsl/compiler.rs`
- Modify: `src/dsl/ir.rs`
- Modify: `src/dsl/mod.rs`
- Modify: `src/dsl/parser.rs`
- Create: `tests/agent_runtime_contract.rs`
- Modify: `tests/agent_compiler.rs`
- Modify: `tests/agent_parser.rs`
- Modify: `tests/goal_contract.rs`
- Modify: `tests/fixtures/dsl/agent_babysit_pr.kite`
- Modify: `tests/fixtures/dsl/agent_babysit_pr.compiled.json`

**Interfaces:**

- Consumes: Slice 1 `CompiledDefinition`, `CompiledStatement`,
  `CompiledValue`, `CompiledType`, `CompiledToolBinding`, `DefinitionKind`,
  `ContextMode`, `ModelRole`, `AcceptedUserTask`, `GoalContract`,
  `SuccessCriterion`, `UserTaskId`, `SessionId`, `PrincipalId`,
  `IdempotencyKey`, `ClosedSchema`, `canonical_json`, and
  `canonical_sha256`.
- Produces: every exact public type and method in the following blocks. Tasks
  2-8 must import these definitions rather than introducing aliases.

```rust
// src/goal.rs
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPhase { Active, Paused, Blocked, Complete }

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct EvidenceId(uuid::Uuid);

impl EvidenceId {
    pub const fn new(value: uuid::Uuid) -> Self { Self(value) }
    pub const fn as_uuid(&self) -> &uuid::Uuid { &self.0 }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum EvidenceSourceIdentity {
    BuiltinHarness { command_id: AgentCommandId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRecord {
    pub id: EvidenceId,
    pub task_id: UserTaskId,
    pub evidence_type: String,
    pub source: EvidenceSourceIdentity,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriterionAssertion {
    pub criterion_id: String,
    pub evidence_id: EvidenceId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalBlocker {
    pub blocked_when_index: u16,
    pub reason_code: String,
    pub explanation: String,
    pub evidence_ids: Vec<EvidenceId>,
    pub unlock_conditions: Vec<String>,
    pub goal_revision: u64,
}

// src/task.rs
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLifecycle { Accepted, Running, Succeeded, Failed, Cancelled }

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalStatus {
    NotStarted,
    Runnable,
    WaitingForContext,
    WaitingForEvent,
    WaitingForHarness,
    WaitingForApproval,
    Paused,
    Blocked,
    Finished,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartAgent {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PauseUserTask {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub reason: String,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeUserTask {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelUserTask {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub reason: Option<String>,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentExecutionView {
    pub task: AcceptedUserTask,
    pub lifecycle: TaskLifecycle,
    pub operational_status: OperationalStatus,
    pub task_revision: u64,
    pub goal_phase: GoalPhase,
    pub goal_revision: u64,
    pub blocker: Option<GoalBlocker>,
    pub runtime: Option<RuntimeStateSnapshot>,
}

// src/agent/types.rs
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AgentCommandId(uuid::Uuid);

impl AgentCommandId {
    pub const fn new(value: uuid::Uuid) -> Self { Self(value) }
    pub const fn as_uuid(&self) -> &uuid::Uuid { &self.0 }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RuntimeCommandKey(String);

impl RuntimeCommandKey {
    pub fn new(value: impl Into<String>) -> Result<Self, AgentRuntimeError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDisposition { Running, Returned, Blocked, Terminating }

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeStateSnapshot {
    pub schema_version: u16,
    pub runtime_id: String,
    pub disposition: RuntimeDisposition,
    pub payload: serde_json::Value,
    pub ceiling_bytes: u32,
    pub canonical_byte_count: u32,
    pub snapshot_digest: String,
}

impl RuntimeStateSnapshot {
    pub fn new(
        disposition: RuntimeDisposition,
        payload: serde_json::Value,
        ceilings: &RuntimeCeilings,
    ) -> Result<Self, AgentRuntimeError>;
    pub fn canonical_bytes(&self, ceilings: &RuntimeCeilings)
        -> Result<Vec<u8>, AgentRuntimeError>;
    pub fn digest(&self, ceilings: &RuntimeCeilings)
        -> Result<String, AgentRuntimeError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCeilings {
    pub max_snapshot_bytes: usize,
    pub max_command_bytes: usize,
    pub max_commands_per_transition: usize,
    pub max_frames: usize,
    pub max_history_items_per_frame: usize,
}

impl Default for RuntimeCeilings {
    fn default() -> Self {
        Self {
            max_snapshot_bytes: 262_144,
            max_command_bytes: 65_536,
            max_commands_per_transition: 64,
            max_frames: 256,
            max_history_items_per_frame: 256,
        }
    }
}

impl RuntimeCeilings {
    pub fn validate(&self) -> Result<(), AgentRuntimeError>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessStructuralLimits {
    pub model_turns_remaining: u64,
    pub tool_calls_per_response: u64,
    pub response_item_bytes: u64,
    pub active_execution_millis_remaining: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHistoryKind {
    HarnessInput,
    HarnessOutput,
    ToolResult,
    TaskReturn,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeHistoryItem {
    pub sequence: u64,
    pub kind: RuntimeHistoryKind,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessTool {
    pub qualified_name: String,
    pub descriptor_digest: String,
    pub descriptor: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalCriterionProjection {
    pub id: String,
    pub evidence_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalProjection {
    pub objective: String,
    pub success: Vec<GoalCriterionProjection>,
    pub constraints: Vec<String>,
}

impl GoalProjection {
    pub fn from_contract(contract: &GoalContract) -> Self;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvokeHarness {
    pub role: ModelRole,
    pub prompt: String,
    pub goal: GoalProjection,
    pub input: serde_json::Value,
    pub input_schema: ClosedSchema,
    pub result_schema: ClosedSchema,
    pub tools: Vec<HarnessTool>,
    pub projected_history: Vec<RuntimeHistoryItem>,
    pub limits: HarnessStructuralLimits,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestContext {
    pub evidence_type: String,
    pub subject: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitForEvent {
    pub event_type: String,
    pub correlation: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestApproval {
    pub question: String,
    pub answer_schema: ClosedSchema,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteGoal { pub assertions: Vec<CriterionAssertion> }

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockGoal {
    pub blocked_when_index: u16,
    pub reason_code: String,
    pub explanation: String,
    pub evidence_ids: Vec<EvidenceId>,
    pub unlock_conditions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailTask { pub code: String, pub message: String }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    rename_all = "snake_case",
    tag = "type",
    content = "payload",
    deny_unknown_fields
)]
pub enum KernelCommandPayload {
    RequestContext(RequestContext),
    WaitForEvent(WaitForEvent),
    InvokeHarness(InvokeHarness),
    RequestApproval(RequestApproval),
    CompleteGoal(CompleteGoal),
    BlockGoal(BlockGoal),
    FailTask(FailTask),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KernelCommand {
    pub schema_version: u16,
    pub key: RuntimeCommandKey,
    pub payload: KernelCommandPayload,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeTransition {
    pub idempotency_key: String,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub snapshot: RuntimeStateSnapshot,
    pub commands: Vec<KernelCommand>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    rename_all = "snake_case",
    tag = "type",
    content = "payload",
    deny_unknown_fields
)]
pub enum RuntimeEvent {
    HarnessResult {
        event_id: String,
        command_key: RuntimeCommandKey,
        outcome: String,
        fields: serde_json::Value,
        evidence_id: EvidenceId,
        history: Vec<RuntimeHistoryItem>,
    },
    ContextResolved {
        event_id: String,
        command_key: RuntimeCommandKey,
        evidence_ids: Vec<EvidenceId>,
    },
    SemanticEvent {
        event_id: String,
        command_key: RuntimeCommandKey,
        event_type: String,
        data: serde_json::Value,
    },
    ApprovalResult {
        event_id: String,
        command_key: RuntimeCommandKey,
        answer: serde_json::Value,
    },
    CommandFailed {
        event_id: String,
        command_key: RuntimeCommandKey,
        code: String,
    },
    PrincipalResumed {
        event_id: String,
    },
}

impl RuntimeEvent {
    pub fn event_id(&self) -> &str;
    pub fn command_key(&self) -> Option<&RuntimeCommandKey>;
}

// src/agent/runtime.rs
pub struct RuntimeStart<'a> {
    pub execution: &'a AgentExecutionView,
    pub definition: &'a CompiledDefinition,
}

pub struct RuntimeHandle<'a> {
    pub execution: &'a AgentExecutionView,
    pub definition: &'a CompiledDefinition,
    pub event: &'a RuntimeEvent,
}

pub trait AgentRuntime: Send + Sync {
    fn validate_agent(&self, definition: &CompiledDefinition)
        -> Result<(), AgentRuntimeError>;
    fn start(&self, input: RuntimeStart<'_>)
        -> Result<RuntimeTransition, AgentRuntimeError>;
    fn handle(&self, input: RuntimeHandle<'_>)
        -> Result<RuntimeTransition, AgentRuntimeError>;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AgentRuntimeError {
    #[error("agent definition is invalid: {0}")]
    InvalidDefinition(String),
    #[error("runtime state is invalid: {0}")]
    InvalidState(String),
    #[error("runtime transition is invalid: {0}")]
    InvalidTransition(String),
    #[error("runtime value is invalid: {0}")]
    InvalidValue(String),
    #[error("runtime ceiling exceeded: {0}")]
    CeilingExceeded(&'static str),
    #[error("runtime canonicalization failed: {0}")]
    Canonicalization(String),
}

pub fn validate_transition(
    transition: &RuntimeTransition,
    execution: &AgentExecutionView,
    ceilings: &RuntimeCeilings,
) -> Result<(), AgentRuntimeError>;
```

The IR additions are exact:

```rust
// src/dsl/ast.rs
pub struct CriterionEvidenceAst {
    pub criterion_id: String,
    pub evidence: ValueRefAst,
    pub span: Span,
}

pub struct BlockGoalAst {
    pub blocked_when_index: u16,
    pub reason_code: ValueRefAst,
    pub explanation: ValueRefAst,
    pub evidence: Vec<ValueRefAst>,
    pub unlock_conditions: Vec<ValueRefAst>,
    pub span: Span,
}

// Added StatementAst variants:
CompleteGoal(Vec<CriterionEvidenceAst>),
BlockGoal(BlockGoalAst),

// src/dsl/ir.rs additions to CompiledStatement:
CompleteGoal {
    criterion_evidence: BTreeMap<String, CompiledValue>,
},
BlockGoal {
    blocked_when_index: u16,
    reason_code: CompiledValue,
    explanation: CompiledValue,
    evidence: Vec<CompiledValue>,
    unlock_conditions: Vec<CompiledValue>,
},
```

- [ ] **Step 1: Add failing contract and grammar tests**

```rust
// tests/agent_runtime_contract.rs
use kiteframe::{
    AgentRuntimeError, KernelCommand, KernelCommandPayload, RuntimeCeilings,
    RuntimeCommandKey, RuntimeDisposition, RuntimeStateSnapshot,
};
use serde_json::json;

#[test]
fn snapshot_digest_is_canonical_and_ceiling_bound() {
    let ceilings = RuntimeCeilings::default();
    let first = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        json!({"frames": [], "nextFrame": 1}),
        &ceilings,
    ).unwrap();
    let second = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        json!({"nextFrame": 1, "frames": []}),
        &ceilings,
    ).unwrap();
    assert_eq!(first.digest(&ceilings).unwrap(), second.digest(&ceilings).unwrap());
    assert_eq!(first.canonical_byte_count, second.canonical_byte_count);
    assert_eq!(first.ceiling_bytes, 262_144);

    let tiny = RuntimeCeilings { max_snapshot_bytes: 8, ..ceilings };
    assert_eq!(
        first.digest(&tiny),
        Err(AgentRuntimeError::CeilingExceeded("snapshot_bytes")),
    );
}

#[test]
fn command_payload_is_closed_and_version_one() {
    let value = json!({
        "schemaVersion": 1,
        "key": "r1-f0-harness",
        "payload": {
            "type": "fail_task",
            "payload": {"code": "no_path", "message": "stopped"}
        }
    });
    let command: KernelCommand = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(command.payload, KernelCommandPayload::FailTask(_)));
    let mut unknown = value;
    unknown["extra"] = json!(true);
    assert!(serde_json::from_value::<KernelCommand>(unknown).is_err());
    assert!(RuntimeCommandKey::new("").is_err());
}
```

```rust
// tests/agent_parser.rs
#[test]
fn parses_explicit_agent_goal_authority_statements() {
    let source = r#"
agent goal_agent(blocker: string) -> string {
  entry work(blocker: blocker)
  task work(blocker: string) -> string {
    model reasoning
    prompt """Choose a declared terminal path."""
    tools []
    on complete_it() {
      complete goal { criterion_one: current_evidence }
    }
    on block_it() {
      block goal when 0 {
        code: "required_actor_unavailable"
        explanation: blocker
        evidence: [current_evidence]
        unlock: ["The required actor responds."]
      }
    }
  }
}
"#;
    let report = parse_report(source, limits()).unwrap();
    let DefinitionAst::Agent(agent) = report.source.definition else { panic!() };
    assert!(matches!(
        agent.tasks[0].outcomes[0].statements[0],
        StatementAst::CompleteGoal(_),
    ));
    assert!(matches!(
        agent.tasks[0].outcomes[1].statements[0],
        StatementAst::BlockGoal(_),
    ));
}
```

- [ ] **Step 2: Run the new tests and observe missing interfaces**

Run:

```bash
cargo test --test agent_runtime_contract --test agent_parser explicit_agent_goal -- --nocapture
```

Expected: FAIL to compile because `src/agent`, lifecycle types, runtime types,
and goal statement AST variants do not exist.

- [ ] **Step 3: Implement the closed domain and runtime contracts**

Add the exact interfaces above. `RuntimeCommandKey::new` accepts 1-128 ASCII
bytes containing only lowercase letters, digits, `-`, `_`, `.`, `/`, or `:`.
`RuntimeStateSnapshot::new` canonicalizes and reparses the payload, then hashes
the non-recursive content envelope `{schemaVersion,runtimeId,disposition,payload}`.
It records the applied deployment ceiling, exact content byte count, and
lowercase SHA-256 digest. `canonical_bytes` rejects schema versions other than
1, runtime ids other than `kite.v1`, non-object payloads, an applied ceiling of
zero or above the Slice 2 maximum, bytes above either the stored or current
ceiling, or any stored byte-count/digest mismatch.
`RuntimeCeilings::validate` requires every value to be nonzero and no greater
than its `Default` value; snapshot construction, transition validation, and
snapshot reads call it before accepting data.

```rust
impl RuntimeCommandKey {
    pub fn new(value: impl Into<String>) -> Result<Self, AgentRuntimeError> {
        let value = value.into();
        let valid = (1..=128).contains(&value.len())
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':')
            });
        valid.then_some(Self(value)).ok_or_else(|| {
            AgentRuntimeError::InvalidTransition("command key is invalid".into())
        })
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

impl RuntimeCeilings {
    pub fn validate(&self) -> Result<(), AgentRuntimeError> {
        let defaults = Self::default();
        let valid = (1..=defaults.max_snapshot_bytes).contains(&self.max_snapshot_bytes)
            && (1..=defaults.max_command_bytes).contains(&self.max_command_bytes)
            && (1..=defaults.max_commands_per_transition)
                .contains(&self.max_commands_per_transition)
            && (1..=defaults.max_frames).contains(&self.max_frames)
            && (1..=defaults.max_history_items_per_frame)
                .contains(&self.max_history_items_per_frame);
        valid.then_some(()).ok_or(
            AgentRuntimeError::CeilingExceeded("configured_ceilings"),
        )
    }
}

impl RuntimeStateSnapshot {
    pub fn new(
        disposition: RuntimeDisposition,
        payload: serde_json::Value,
        ceilings: &RuntimeCeilings,
    ) -> Result<Self, AgentRuntimeError> {
        ceilings.validate()?;
        let normalized_payload: serde_json::Value = serde_json::from_slice(
            &canonical_json(&payload)
                .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?,
        ).map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?;
        let content = json!({
            "schemaVersion": 1,
            "runtimeId": "kite.v1",
            "disposition": disposition,
            "payload": normalized_payload,
        });
        let bytes = canonical_json(&content)
            .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?;
        if bytes.len() > ceilings.max_snapshot_bytes {
            return Err(AgentRuntimeError::CeilingExceeded("snapshot_bytes"));
        }
        Ok(Self {
            schema_version: 1,
            runtime_id: "kite.v1".into(),
            disposition,
            payload: content["payload"].clone(),
            ceiling_bytes: u32::try_from(ceilings.max_snapshot_bytes)
                .map_err(|_| AgentRuntimeError::CeilingExceeded("snapshot_bytes"))?,
            canonical_byte_count: bytes.len() as u32,
            snapshot_digest: canonical_sha256(&content)
                .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?,
        })
    }

    pub fn canonical_bytes(
        &self,
        ceilings: &RuntimeCeilings,
    ) -> Result<Vec<u8>, AgentRuntimeError> {
        ceilings.validate()?;
        if self.schema_version != 1 || self.runtime_id != "kite.v1" {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot envelope is unsupported".into(),
            ));
        }
        if !self.payload.is_object() {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot payload must be an object".into(),
            ));
        }
        if self.ceiling_bytes == 0 || self.ceiling_bytes > 262_144 {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot ceiling is invalid".into(),
            ));
        }
        let content = json!({
            "schemaVersion": self.schema_version,
            "runtimeId": &self.runtime_id,
            "disposition": &self.disposition,
            "payload": &self.payload,
        });
        let bytes = canonical_json(&content)
            .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?;
        if bytes.len() > ceilings.max_snapshot_bytes
            || bytes.len() > self.ceiling_bytes as usize
        {
            return Err(AgentRuntimeError::CeilingExceeded("snapshot_bytes"));
        }
        if bytes.len() != self.canonical_byte_count as usize
            || canonical_sha256(&content)
                .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?
                != self.snapshot_digest
        {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot integrity metadata does not match".into(),
            ));
        }
        Ok(bytes)
    }

    pub fn digest(&self, ceilings: &RuntimeCeilings) -> Result<String, AgentRuntimeError> {
        self.canonical_bytes(ceilings)?;
        Ok(self.snapshot_digest.clone())
    }
}

impl GoalProjection {
    pub fn from_contract(contract: &GoalContract) -> Self {
        Self {
            objective: contract.objective.clone(),
            success: contract.success.iter().map(|criterion| {
                GoalCriterionProjection {
                    id: criterion.id.clone(),
                    evidence_type: criterion.evidence.clone(),
                }
            }).collect(),
            constraints: contract.constraints.clone(),
        }
    }
}
```

`validate_transition` verifies exact expected revisions, 1-128 byte
idempotency key, command schema version 1, unique command keys, command
count/size ceilings, the full snapshot digestability, and these disposition
rules:

```rust
match transition.snapshot.disposition {
    RuntimeDisposition::Running if transition.commands.is_empty() => {
        return Err(AgentRuntimeError::InvalidTransition(
            "running snapshot requires a command".into(),
        ));
    }
    RuntimeDisposition::Returned if !transition.commands.is_empty() => {
        return Err(AgentRuntimeError::InvalidTransition(
            "returned snapshot cannot emit commands".into(),
        ));
    }
    RuntimeDisposition::Blocked => {
        if !matches!(
            transition.commands.as_slice(),
            [KernelCommand { payload: KernelCommandPayload::BlockGoal(_), .. }]
        ) {
            return Err(AgentRuntimeError::InvalidTransition(
                "blocked snapshot requires one BlockGoal".into(),
            ));
        }
    }
    RuntimeDisposition::Terminating => {
        if !matches!(
            transition.commands.as_slice(),
            [KernelCommand { payload: KernelCommandPayload::CompleteGoal(_) | KernelCommandPayload::FailTask(_), .. }]
        ) {
            return Err(AgentRuntimeError::InvalidTransition(
                "terminating snapshot requires one authority command".into(),
            ));
        }
    }
    _ => {}
}
```

- [ ] **Step 4: Parse and compile the exact goal operations**

Pass Slice 1's `DefinitionSyntax` into statement parsing. Parse exact field
names and cardinalities, reject commas only where the language reference
rejects them, and mark both statements terminal. Use these diagnostics:

```text
complete_goal_requires_agent
empty_goal_completion
duplicate_goal_criterion
invalid_goal_criterion
block_goal_requires_agent
invalid_blocked_when_index
duplicate_block_goal_field
missing_block_goal_field
empty_block_evidence
empty_unlock_conditions
goal_authority_value_must_be_string
reserved_current_evidence_binding
```

Compile every value against `CompiledType::String`, retain source order only
for block evidence/unlock lists, and normalize completion criteria into a
`BTreeMap`. Synthesize immutable `current_evidence: string` only in an agent
outcome-handler scope; reject that name in definition/task inputs and outcome
fields so source/model data cannot shadow the kernel binding. Lower a reference
to the ordinary closed binding form—the runtime, not a new IR opcode, supplies
its value. Do not inspect a `GoalContract` in the compiler.

```rust
StatementAst::CompleteGoal(criteria) => {
    let mut criterion_evidence = BTreeMap::new();
    for criterion in criteria {
        self.expect_value_type(
            &criterion.evidence,
            scope,
            &CompiledType::String,
            criterion.span,
        );
        if criterion_evidence.insert(
            criterion.criterion_id.clone(),
            self.compiled_value(&criterion.evidence),
        ).is_some() {
            self.error("goal completion criterion is duplicated", criterion.span);
        }
    }
    compiled.push(CompiledStatement::CompleteGoal { criterion_evidence });
}
StatementAst::BlockGoal(block) => {
    for value in std::iter::once(&block.reason_code)
        .chain(std::iter::once(&block.explanation))
        .chain(block.evidence.iter())
        .chain(block.unlock_conditions.iter())
    {
        self.expect_value_type(value, scope, &CompiledType::String, block.span);
    }
    compiled.push(CompiledStatement::BlockGoal {
        blocked_when_index: block.blocked_when_index,
        reason_code: self.compiled_value(&block.reason_code),
        explanation: self.compiled_value(&block.explanation),
        evidence: block.evidence.iter().map(|value| self.compiled_value(value)).collect(),
        unlock_conditions: block.unlock_conditions.iter()
            .map(|value| self.compiled_value(value)).collect(),
    });
}
```

- [ ] **Step 5: Add the negative grammar and serialization matrix**

Add table-driven tests for zero completion criteria; duplicate/invalid
criterion ids; unknown, duplicate, or missing block fields; block indexes
`65536` and non-decimal text; empty evidence/unlock lists; integer-valued
reason, explanation, evidence, or unlock; either operation in a workflow; and
a statement after either terminal operation. Assert the exact diagnostic code.

```rust
#[test]
fn goal_authority_syntax_fails_closed() {
    let cases = [
        (AGENT.replace("criterion_one: evidence", ""), "empty_goal_completion"),
        (AGENT.replace(
            "criterion_one: evidence",
            "criterion_one: evidence criterion_one: evidence",
        ), "duplicate_goal_criterion"),
        (AGENT.replace("criterion_one", "Bad-ID"), "invalid_goal_criterion"),
        (AGENT.replace("when 0", "when 65536"), "invalid_blocked_when_index"),
        (AGENT.replace("evidence: [evidence]", "evidence: []"), "empty_block_evidence"),
        (AGENT.replace(
            "unlock: [\"The required actor responds.\"]",
            "unlock: []",
        ), "empty_unlock_conditions"),
        (AGENT.replace("explanation: blocker", "explanation: 1"),
            "goal_authority_value_must_be_string"),
    ];
    for (source, expected) in cases {
        assert_eq!(parse_report(&source, limits()).unwrap_err()[0].code, expected);
    }
}
```

Prove legacy version-1 JSON contains neither new statement variant and retains
its exact existing golden. Update the agent golden and definition hash only
because its reviewed Slice 2 source now contains explicit goal statements.

- [ ] **Step 6: Write the three reference contracts**

The runtime reference copies the exact types, ceilings, disposition rules,
lock order, command availability table, and evidence metadata from this task.
The language reference copies the reviewed syntax and diagnostics. The task
and goal reference includes this exact state matrix:

| Trigger | Task lifecycle | Operational status | Goal phase |
|---|---|---|---|
| accepted | `accepted` | `not_started` | `active` |
| StartAgent | `running` | derived from pending command | unchanged |
| root `done` | `succeeded` | `finished` | unchanged |
| `CompleteGoal` accepted | `succeeded` | `finished` | `complete` |
| `BlockGoal` accepted | `running` | `blocked` | `blocked` |
| principal pause | unchanged nonterminal lifecycle | `paused` | `paused` |
| principal resume | unchanged nonterminal lifecycle | derived | `active` |
| `FailTask` | `failed` | `finished` | unchanged |
| principal cancel | `cancelled` | `finished` | unchanged |

State that a `HarnessResult` supplies an invocation outcome only; even a
successful root outcome needs explicit `complete goal` plus kernel-validated
evidence to change the goal phase.

- [ ] **Step 7: Run and commit the contract boundary**

Run:

```bash
cargo fmt --all -- --check
cargo test --test agent_runtime_contract --test agent_parser --test agent_compiler --test goal_contract --test dsl_compiler
git diff --check
```

Expected: PASS; the legacy compiled fixture remains byte-identical.

```bash
git add docs/reference/agent-runtime-and-goal-lifecycle.md docs/reference/kite-language.md docs/reference/user-task-and-goal-contract.md src/agent/mod.rs src/agent/types.rs src/agent/runtime.rs src/goal.rs src/lib.rs src/task.rs src/dsl/ast.rs src/dsl/compiler.rs src/dsl/ir.rs src/dsl/mod.rs src/dsl/parser.rs tests/agent_runtime_contract.rs tests/agent_compiler.rs tests/agent_parser.rs tests/goal_contract.rs tests/fixtures/dsl/agent_babysit_pr.kite tests/fixtures/dsl/agent_babysit_pr.compiled.json
git commit -m "feat: define authoritative agent runtime contracts"
```

### Task 2: Add lifecycle schema and typed current-state mapping

**Files:**

- Create: `migrations/0013_agent_lifecycle.sql`
- Create: `src/store/agent.rs`
- Modify: `src/config.rs`
- Modify: `src/store/mod.rs`
- Modify: `src/store/models.rs`
- Modify: `src/store/postgres.rs`
- Modify: `src/store/session_close.rs`
- Modify: `src/lib.rs`
- Create: `tests/agent_lifecycle_store.rs`
- Modify: `tests/definition_store.rs`
- Modify: `tests/execution_store.rs`
- Modify: `tests/postgres_lock_order.rs`
- Modify: `tests/session_store.rs`
- Modify: `tests/task_store.rs`

**Interfaces:**

- Consumes: Task 1 `AgentCommandId`, `AgentExecutionView`, `GoalPhase`,
  `OperationalStatus`, `RuntimeStateSnapshot`, `TaskLifecycle`,
  `RuntimeCeilings`; Slice 1 `AcceptedUserTask`, `CompiledDefinition`,
  `DefinitionKind`, `UserTaskId`, and `accepted_user_task_from_row` semantics.
- Produces:

```rust
// src/store/models.rs: add to the existing StoreLimits.
pub struct StoreLimits {
    pub max_active_sessions_per_principal: usize,
    pub max_active_runs_per_session: usize,
    pub max_open_permission_requests_per_session: usize,
    pub agent_runtime: RuntimeCeilings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentCommandState { Pending, Claimed, Completed, Failed, Cancelled }

#[derive(Clone, Debug)]
pub struct AgentCommandView {
    pub id: AgentCommandId,
    pub task_id: UserTaskId,
    pub key: RuntimeCommandKey,
    pub ordinal: u16,
    pub schema_version: u16,
    pub payload: KernelCommandPayload,
    pub payload_digest: String,
    pub state: AgentCommandState,
    pub revision: u64,
    pub claimant: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentTaskEventKind {
    AgentStarted,
    CommandClaimed,
    RuntimeTransitionCommitted,
    RuntimeCommandUnavailable,
    GoalCompleted,
    GoalBlocked,
    TaskPaused,
    TaskResumed,
    TaskCancelled,
    TaskFailed,
    TaskSucceeded,
}

impl AgentTaskEventKind {
    pub const fn as_str(self) -> &'static str;
    pub fn parse(value: &str) -> Result<Self, StoreError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentTaskEvent {
    pub sequence: u64,
    pub kind: AgentTaskEventKind,
    pub metadata: serde_json::Value,
    pub task_revision: u64,
    pub goal_revision: u64,
    pub created_at: time::OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct AgentStartSeed {
    pub execution: AgentExecutionView,
    pub definition: CompiledDefinition,
}

struct LockedAgentAggregate {
    session_principal: String,
    session_state: String,
    session_policy_revision: PolicyRevision,
    task_principal: String,
    task_lifecycle: TaskLifecycle,
    operational_status: OperationalStatus,
    task_revision: u64,
    definition_id: uuid::Uuid,
    goal_phase: GoalPhase,
    goal_revision: u64,
    current_blocker_id: Option<uuid::Uuid>,
}

impl PostgresStore {
    pub const fn agent_runtime_ceilings(&self) -> &RuntimeCeilings;

    pub async fn get_agent_execution(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn load_agent_start_seed(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
    ) -> Result<AgentStartSeed, StoreError>;

    pub async fn agent_task_events_after(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<AgentTaskEvent>, StoreError>;
}

// Private to src/store/agent.rs; all later transactions reuse them.
async fn lock_agent_aggregate(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: &SessionId,
    task_id: &UserTaskId,
) -> Result<LockedAgentAggregate, StoreError>;

fn agent_execution_from_row(
    row: &sqlx::postgres::PgRow,
    ceilings: &RuntimeCeilings,
) -> Result<AgentExecutionView, StoreError>;

fn definition_from_agent_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CompiledDefinition, StoreError>;

async fn append_agent_task_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    task_id: &UserTaskId,
    kind: AgentTaskEventKind,
    metadata: serde_json::Value,
    task_revision: u64,
    goal_revision: u64,
) -> Result<(), StoreError>;

// Test-local support in tests/agent_lifecycle_store.rs.
struct AgentStoreFixture {
    store: Arc<PostgresStore>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    task: AcceptedUserTask,
}

impl AgentStoreFixture {
    async fn accepted() -> Option<Self>;
    async fn with_history_rows() -> Option<Self>;
    async fn current(&self) -> AgentExecutionView;
}
```

`PostgresStore` keeps the complete `StoreLimits` it already receives and every
strict agent reader passes `self.agent_runtime_ceilings()` to
`agent_execution_from_row`. Parse these optional environment variables in
`src/config.rs`; each defaults to the corresponding `RuntimeCeilings::default`
value and accepts only `1..=default`:

```text
KITEFRAME_AGENT_MAX_SNAPSHOT_BYTES
KITEFRAME_AGENT_MAX_COMMAND_BYTES
KITEFRAME_AGENT_MAX_COMMANDS_PER_TRANSITION
KITEFRAME_AGENT_MAX_FRAMES
KITEFRAME_AGENT_MAX_HISTORY_ITEMS_PER_FRAME
```

Malformed, zero, or raised values return the new fixed
`ConfigError::InvalidAgentRuntimeCeiling`. Extend the existing config unit test
to cover one lowered value for every field plus zero, above-default, and
non-numeric rejection. Do not add an unbounded fallback.

- [ ] **Step 1: Add the failing migration-shape and initial mapping tests**

```rust
// tests/agent_lifecycle_store.rs
#[test]
fn slice_two_schema_separates_contract_goal_and_runtime_authority() {
    let sql = std::fs::read_to_string("migrations/0013_agent_lifecycle.sql")
        .expect("Slice 2 migration exists");
    for required in [
        "CREATE TABLE goal_states",
        "CREATE TABLE agent_runtime_snapshots",
        "CREATE TABLE agent_transitions",
        "CREATE TABLE agent_commands",
        "CREATE TABLE agent_policy_decisions",
        "CREATE TABLE goal_evidence",
        "CREATE TABLE goal_criterion_assertions",
        "CREATE TABLE goal_blockers",
        "CREATE TABLE agent_task_actions",
        "CREATE TABLE agent_task_events",
        "reject_agent_history_mutation",
    ] {
        assert!(sql.contains(required), "missing schema: {required}");
    }
    assert!(!sql.contains("ALTER TABLE goal_contracts ADD COLUMN phase"));
    assert!(!sql.contains("provider"));
}

#[tokio::test]
async fn migrated_accepted_task_has_independent_initial_axes() {
    let Some(fixture) = AgentStoreFixture::accepted().await else { return };
    let view = fixture.store
        .get_agent_execution(fixture.session.id, fixture.task.id.clone())
        .await
        .unwrap();
    assert_eq!(view.lifecycle, TaskLifecycle::Accepted);
    assert_eq!(view.operational_status, OperationalStatus::NotStarted);
    assert_eq!(view.task_revision, 0);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(view.goal_revision, 0);
    assert!(view.runtime.is_none());
    assert!(view.blocker.is_none());
}
```

```rust
// src/config.rs tests
#[test]
fn agent_runtime_ceilings_can_only_lower_the_hard_maxima() {
    let mut values = vec![
        ("DATABASE_URL", "postgres://localhost/kiteframe"),
        ("KITEFRAME_PRINCIPAL", "local-user"),
        ("KITEFRAME_AGENT_MAX_SNAPSHOT_BYTES", "131072"),
        ("KITEFRAME_AGENT_MAX_COMMAND_BYTES", "32768"),
        ("KITEFRAME_AGENT_MAX_COMMANDS_PER_TRANSITION", "32"),
        ("KITEFRAME_AGENT_MAX_FRAMES", "128"),
        ("KITEFRAME_AGENT_MAX_HISTORY_ITEMS_PER_FRAME", "128"),
    ];
    values.extend(model_values());
    assert_eq!(
        config(&values).unwrap().store_limits.agent_runtime,
        RuntimeCeilings {
            max_snapshot_bytes: 131_072,
            max_command_bytes: 32_768,
            max_commands_per_transition: 32,
            max_frames: 128,
            max_history_items_per_frame: 128,
        },
    );

    for (name, invalid) in [
        ("KITEFRAME_AGENT_MAX_SNAPSHOT_BYTES", "262145"),
        ("KITEFRAME_AGENT_MAX_COMMAND_BYTES", "65537"),
        ("KITEFRAME_AGENT_MAX_COMMANDS_PER_TRANSITION", "65"),
        ("KITEFRAME_AGENT_MAX_FRAMES", "257"),
        ("KITEFRAME_AGENT_MAX_HISTORY_ITEMS_PER_FRAME", "257"),
        ("KITEFRAME_AGENT_MAX_FRAMES", "0"),
        ("KITEFRAME_AGENT_MAX_FRAMES", "not-a-number"),
    ] {
        let mut invalid_values = vec![
            ("DATABASE_URL", "postgres://localhost/kiteframe"),
            ("KITEFRAME_PRINCIPAL", "local-user"),
            (name, invalid),
        ];
        invalid_values.extend(model_values());
        assert!(matches!(
            config(&invalid_values),
            Err(ConfigError::InvalidAgentRuntimeCeiling),
        ));
    }
}
```

- [ ] **Step 2: Observe the absent migration and store module**

Run:

```bash
cargo test --test agent_lifecycle_store slice_two_schema -- --nocapture
cargo test --test agent_lifecycle_store migrated_accepted_task -- --nocapture
cargo test --lib agent_runtime_ceilings_can_only_lower -- --nocapture
```

Expected: the first test FAILS because migration `0013` does not exist; the
second test FAILS to compile because `get_agent_execution` does not exist; the
config test FAILS because `StoreLimits::agent_runtime` does not exist.

- [ ] **Step 3: Write the complete migration**

Use the exact state names and constraints below. `goal_contracts` remains
protected by Slice 1's update/delete triggers; mutable phase is held only in
`goal_states`.

```sql
ALTER TABLE user_tasks DROP CONSTRAINT user_tasks_state_check;
ALTER TABLE user_tasks DROP CONSTRAINT user_tasks_revision_check;
ALTER TABLE user_tasks
    ADD CONSTRAINT user_tasks_state_check
        CHECK (state IN ('accepted', 'running', 'succeeded', 'failed', 'cancelled')),
    ADD CONSTRAINT user_tasks_revision_check CHECK (revision >= 0),
    ADD COLUMN operational_status TEXT NOT NULL DEFAULT 'not_started'
        CHECK (operational_status IN (
            'not_started', 'runnable', 'waiting_for_context', 'waiting_for_event',
            'waiting_for_harness', 'waiting_for_approval', 'paused', 'blocked',
            'finished'
        )),
    ADD COLUMN failure JSONB,
    ADD COLUMN cancellation_reason TEXT,
    ADD COLUMN started_at TIMESTAMPTZ,
    ADD COLUMN finished_at TIMESTAMPTZ;

CREATE TABLE goal_states (
    task_id UUID PRIMARY KEY REFERENCES user_tasks(id),
    phase TEXT NOT NULL DEFAULT 'active'
        CHECK (phase IN ('active', 'paused', 'blocked', 'complete')),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    current_blocker_id UUID,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO goal_states (task_id)
SELECT id FROM user_tasks;

CREATE TABLE agent_runtime_snapshots (
    task_id UUID PRIMARY KEY REFERENCES user_tasks(id),
    task_revision BIGINT NOT NULL CHECK (task_revision > 0),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    runtime_id TEXT NOT NULL CHECK (runtime_id = 'kite.v1'),
    disposition TEXT NOT NULL
        CHECK (disposition IN ('running', 'returned', 'blocked', 'terminating')),
    payload JSONB NOT NULL,
    ceiling_bytes INTEGER NOT NULL CHECK (ceiling_bytes BETWEEN 1 AND 262144),
    canonical_bytes INTEGER NOT NULL CHECK (canonical_bytes BETWEEN 1 AND 262144),
    snapshot_digest TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE agent_transitions (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    source_event_id TEXT,
    expected_task_revision BIGINT NOT NULL CHECK (expected_task_revision >= 0),
    expected_goal_revision BIGINT NOT NULL CHECK (expected_goal_revision >= 0),
    committed_task_revision BIGINT NOT NULL CHECK (committed_task_revision > 0),
    committed_goal_revision BIGINT NOT NULL CHECK (committed_goal_revision >= 0),
    snapshot_digest TEXT NOT NULL,
    command_digests JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, idempotency_key),
    UNIQUE (task_id, id),
    UNIQUE (task_id, committed_task_revision)
);

CREATE TABLE agent_commands (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    transition_id UUID NOT NULL REFERENCES agent_transitions(id),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 63),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    runtime_key TEXT NOT NULL,
    command_kind TEXT NOT NULL CHECK (command_kind IN (
        'request_context', 'wait_for_event', 'invoke_harness',
        'request_approval', 'complete_goal', 'block_goal', 'fail_task'
    )),
    payload JSONB NOT NULL,
    payload_digest TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'claimed', 'completed', 'failed', 'cancelled')),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    claimant TEXT,
    claim_idempotency_key TEXT,
    claim_request_digest TEXT,
    completion_idempotency_key TEXT,
    completion_request_digest TEXT,
    result JSONB,
    failure JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (transition_id, ordinal),
    UNIQUE (task_id, id),
    UNIQUE (task_id, runtime_key)
);

CREATE TABLE agent_policy_decisions (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    transition_id UUID NOT NULL UNIQUE REFERENCES agent_transitions(id),
    action_kind TEXT NOT NULL
        CHECK (action_kind IN ('start', 'complete_goal', 'block_goal')),
    allowed BOOLEAN NOT NULL CHECK (allowed),
    policy_revision BIGINT NOT NULL CHECK (policy_revision >= 0),
    reason_code TEXT NOT NULL
        CHECK (reason_code ~ '^[a-z][a-z0-9_]{0,63}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (task_id, transition_id)
        REFERENCES agent_transitions(task_id, id)
);

CREATE TABLE goal_evidence (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    evidence_type TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind = 'builtin_harness'),
    source_identity UUID NOT NULL,
    content_digest TEXT NOT NULL,
    UNIQUE (task_id, id),
    FOREIGN KEY (task_id, source_identity)
        REFERENCES agent_commands(task_id, id)
);

CREATE TABLE goal_criterion_assertions (
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    criterion_id TEXT NOT NULL,
    evidence_id UUID NOT NULL,
    goal_revision BIGINT NOT NULL CHECK (goal_revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (task_id, criterion_id),
    FOREIGN KEY (task_id, evidence_id)
        REFERENCES goal_evidence(task_id, id)
);

CREATE TABLE goal_blockers (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    blocked_when_index INTEGER NOT NULL CHECK (blocked_when_index BETWEEN 0 AND 65535),
    reason_code TEXT NOT NULL,
    explanation TEXT NOT NULL,
    evidence_ids JSONB NOT NULL,
    unlock_conditions JSONB NOT NULL,
    goal_revision BIGINT NOT NULL CHECK (goal_revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, id),
    UNIQUE (task_id, goal_revision)
);

ALTER TABLE goal_states
    ADD CONSTRAINT goal_states_current_blocker_fk
    FOREIGN KEY (task_id, current_blocker_id)
    REFERENCES goal_blockers(task_id, id);

CREATE TABLE agent_task_actions (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    action_kind TEXT NOT NULL CHECK (action_kind IN ('start', 'pause', 'resume', 'cancel')),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    result_task_revision BIGINT NOT NULL CHECK (result_task_revision >= 0),
    result_goal_revision BIGINT NOT NULL CHECK (result_goal_revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, action_kind, idempotency_key)
);

CREATE TABLE agent_task_events (
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    kind TEXT NOT NULL CHECK (kind IN (
        'agent.started', 'command.claimed', 'runtime.transition_committed',
        'runtime.command_unavailable', 'goal.completed', 'goal.blocked',
        'task.paused', 'task.resumed', 'task.cancelled', 'task.failed',
        'task.succeeded'
    )),
    metadata JSONB NOT NULL,
    task_revision BIGINT NOT NULL CHECK (task_revision >= 0),
    goal_revision BIGINT NOT NULL CHECK (goal_revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (task_id, sequence)
);

CREATE FUNCTION reject_agent_history_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'agent history is immutable' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER agent_transitions_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_transitions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER agent_policy_decisions_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_policy_decisions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER goal_evidence_reject_update_delete
    BEFORE UPDATE OR DELETE ON goal_evidence
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER goal_assertions_reject_update_delete
    BEFORE UPDATE OR DELETE ON goal_criterion_assertions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER goal_blockers_reject_update_delete
    BEFORE UPDATE OR DELETE ON goal_blockers
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER agent_actions_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_task_actions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER agent_events_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_task_events
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
```

Extend Slice 1's `submit_user_task` transaction immediately after the immutable
`goal_contracts` insert so every task accepted after migration receives its
initial mutable goal state in the same commit:

```rust
sqlx::query(
    "INSERT INTO goal_states (task_id, phase, revision) VALUES ($1, 'active', 0)",
)
.bind(task_id.as_uuid())
.execute(&mut *tx)
.await?;
```

Add a failure-injection test on this insert and prove task, goal contract, and
goal state all roll back together. The migration backfill query covers
only tasks accepted before `0013`; it is not the normal write path.

- [ ] **Step 4: Map accepted and running rows through one strict reader**

Create `src/store/agent.rs`; add `mod agent;` in `src/store/mod.rs`. The joined
query selects Slice 1 task/goal/definition columns plus lifecycle,
`goal_states`, current blocker, and optional current runtime snapshot. Reuse
the Slice 1 JSON deserializers for target, preferences, goal, and exact
published definition. Parse every SQL state with closed match functions.

```rust
fn goal_phase(value: &str) -> Result<GoalPhase, StoreError> {
    match value {
        "active" => Ok(GoalPhase::Active),
        "paused" => Ok(GoalPhase::Paused),
        "blocked" => Ok(GoalPhase::Blocked),
        "complete" => Ok(GoalPhase::Complete),
        _ => Err(StoreError::InvalidData("goal phase is malformed".into())),
    }
}

fn task_lifecycle(value: &str) -> Result<TaskLifecycle, StoreError> {
    match value {
        "accepted" => Ok(TaskLifecycle::Accepted),
        "running" => Ok(TaskLifecycle::Running),
        "succeeded" => Ok(TaskLifecycle::Succeeded),
        "failed" => Ok(TaskLifecycle::Failed),
        "cancelled" => Ok(TaskLifecycle::Cancelled),
        _ => Err(StoreError::InvalidData("task lifecycle is malformed".into())),
    }
}
```

When a snapshot exists, reconstruct the `RuntimeStateSnapshot`, canonicalize
it with the supplied ceilings, and compare schema version, runtime id,
disposition, stored canonical byte count, and digest. Any mismatch returns
`StoreError::RuntimeStateInvalid("stored snapshot integrity check failed")`.
Never silently repair the row.
Also require the snapshot row's `task_revision` to equal the joined user-task
revision; a mismatch is the same fixed `RuntimeStateInvalid` error.

```rust
let snapshot = RuntimeStateSnapshot {
    schema_version: row.get::<i32, _>("runtime_schema_version") as u16,
    runtime_id: row.get("runtime_id"),
    disposition: runtime_disposition(&row.get::<String, _>("runtime_disposition"))?,
    payload: row.get("runtime_payload"),
    ceiling_bytes: row.get::<i32, _>("runtime_ceiling_bytes") as u32,
    canonical_byte_count: row.get::<i32, _>("runtime_canonical_bytes") as u32,
    snapshot_digest: row.get("runtime_snapshot_digest"),
};
let bytes = snapshot.canonical_bytes(ceilings)
    .map_err(|_| StoreError::RuntimeStateInvalid(
        "stored snapshot integrity check failed".into(),
    ))?;
let digest = snapshot.digest(ceilings)
    .map_err(|_| StoreError::RuntimeStateInvalid(
        "stored snapshot integrity check failed".into(),
    ))?;
if bytes.len() != snapshot.canonical_byte_count as usize
    || digest.as_str() != snapshot.snapshot_digest.as_str()
{
    return Err(StoreError::RuntimeStateInvalid(
        "stored snapshot integrity check failed".into(),
    ));
}
```

`load_agent_start_seed` reads `compiled_definition` from the same frozen
`definition_id` joined through `user_tasks`; it rejects a non-agent kind, a
hash mismatch between the compiled value and `PublishedDefinition`, or a DSL
version other than 2.

- [ ] **Step 5: Enforce the global agent lock order**

`lock_agent_aggregate` first locks the session, then the selected task, then
its goal state. It checks ownership and session state only after all three
rows are present. Later mutation methods use only this helper.

```rust
async fn lock_agent_aggregate(
    tx: &mut Transaction<'_, Postgres>,
    session_id: &SessionId,
    task_id: &UserTaskId,
) -> Result<LockedAgentAggregate, StoreError> {
    let session = sqlx::query(
        "SELECT principal, state, current_policy_revision \
         FROM sessions WHERE id = $1 FOR UPDATE",
    )
    .bind(session_id.as_uuid())
    .fetch_optional(&mut **tx).await?
    .ok_or(StoreError::SessionNotFound)?;
    let task = sqlx::query(
        "SELECT principal, state, operational_status, revision, definition_id \
         FROM user_tasks WHERE id = $1 AND session_id = $2 FOR UPDATE",
    )
    .bind(task_id.as_uuid()).bind(session_id.as_uuid())
    .fetch_optional(&mut **tx).await?
    .ok_or(StoreError::UserTaskNotFound)?;
    let goal = sqlx::query(
        "SELECT phase, revision, current_blocker_id FROM goal_states \
         WHERE task_id = $1 FOR UPDATE",
    )
    .bind(task_id.as_uuid())
    .fetch_optional(&mut **tx).await?
    .ok_or_else(|| StoreError::InvalidData("goal state is absent".into()))?;
    Ok(LockedAgentAggregate::from_rows(session, task, goal)?)
}
```

Change `close_session_and_cancel_work` to lock active user tasks ordered by id,
then their goal states ordered by task id, before its existing run/frame locks.
It marks each nonterminal task cancelled and finished, increments only the
task revision, advances any snapshot row's `task_revision` without changing
its integrity fields, cancels pending/claimed agent commands, appends
`task.cancelled`, and leaves the goal phase/revision unchanged.

- [ ] **Step 6: Add malformed-row, immutability, and lock-order tests**

Cover every invalid SQL state string, snapshot schema/runtime/digest/byte
mismatch, a compiled-definition hash mismatch, missing goal state, direct
update/delete attempts on every immutable table, session-close cancellation,
and concurrent session close versus a transaction holding task/goal locks.

```rust
#[tokio::test]
async fn session_close_cancels_task_without_completing_goal() {
    let Some(fixture) = AgentStoreFixture::accepted().await else { return };
    fixture.store.close_session_and_cancel_work(
        fixture.session.id.clone(),
        &fixture.principal,
    ).await.unwrap();
    let view = fixture.store
        .get_agent_execution(fixture.session.id, fixture.task.id)
        .await.unwrap();
    assert_eq!(view.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(view.operational_status, OperationalStatus::Finished);
    assert_eq!(view.task_revision, 1);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(view.goal_revision, 0);
}

#[tokio::test]
async fn immutable_agent_history_rejects_direct_mutation() {
    let Some(fixture) = AgentStoreFixture::with_history_rows().await else { return };
    for statement in [
        "UPDATE agent_transitions SET request_digest = 'x' WHERE task_id = $1",
        "DELETE FROM agent_policy_decisions WHERE task_id = $1",
        "DELETE FROM goal_evidence WHERE task_id = $1",
        "DELETE FROM goal_criterion_assertions WHERE task_id = $1",
        "UPDATE goal_blockers SET explanation = 'x' WHERE task_id = $1",
        "DELETE FROM agent_task_actions WHERE task_id = $1",
        "DELETE FROM agent_task_events WHERE task_id = $1",
    ] {
        let error = sqlx::query(statement)
            .bind(fixture.task.id.as_uuid())
            .execute(&fixture.pool).await.unwrap_err();
        assert_eq!(error.as_database_error().unwrap().code().as_deref(), Some("55000"));
    }
}
```

- [ ] **Step 7: Run and commit the persistence boundary**

Run:

```bash
cargo test --test agent_lifecycle_store --test task_store --test definition_store --test session_store --test execution_store --test postgres_lock_order
cargo test --test execution_store --test e2e_restart
cargo test --lib agent_runtime_ceilings_can_only_lower
git diff --check
```

Expected: PASS against a configured disposable PostgreSQL database; legacy
store and restart suites remain unchanged.

```bash
git add migrations/0013_agent_lifecycle.sql src/config.rs src/store/agent.rs src/store/mod.rs src/store/models.rs src/store/postgres.rs src/store/session_close.rs src/lib.rs tests/agent_lifecycle_store.rs tests/definition_store.rs tests/execution_store.rs tests/postgres_lock_order.rs tests/session_store.rs tests/task_store.rs
git commit -m "feat: persist authoritative agent lifecycle state"
```

### Task 3: Start the exact frozen agent atomically and idempotently

**Files:**

- Create: `src/agent/kernel.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/store/agent.rs`
- Modify: `src/store/mod.rs`
- Modify: `src/store/models.rs`
- Modify: `src/lib.rs`
- Create: `tests/agent_start.rs`
- Modify: `tests/agent_lifecycle_store.rs`
- Modify: `tests/postgres_lock_order.rs`
- Modify: `tests/task_store.rs`

**Interfaces:**

- Consumes: Task 1 `AgentRuntime::{validate_agent,start}`,
  `RuntimeCeilings`, `RuntimeStart`, `RuntimeTransition`,
  `validate_transition`; Task 2 `AgentStartSeed`, `AgentExecutionView`,
  `lock_agent_aggregate`, `append_agent_task_event`; Slice 1 exact frozen
  `PublishedDefinition` and compiled-definition row.
- Produces:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentLifecycleAction { Start, CompleteGoal, BlockGoal }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPolicyDecision {
    pub allowed: bool,
    pub policy_revision: PolicyRevision,
    pub reason_code: String,
}

#[async_trait::async_trait]
pub trait AgentLifecycleAuthorizer: Send + Sync {
    async fn authorize(
        &self,
        principal: &PrincipalId,
        task: &AcceptedUserTask,
        action: AgentLifecycleAction,
    ) -> Result<AgentPolicyDecision, StoreError>;
}

pub struct TaskOwnerAuthorizer { store: Arc<PostgresStore> }

impl TaskOwnerAuthorizer {
    pub fn new(store: Arc<PostgresStore>) -> Self;
}

pub struct AgentKernel {
    store: Arc<PostgresStore>,
    runtime: Arc<dyn AgentRuntime>,
    authorizer: Arc<dyn AgentLifecycleAuthorizer>,
    ceilings: RuntimeCeilings,
}

impl AgentKernel {
    pub fn new(
        store: Arc<PostgresStore>,
        runtime: Arc<dyn AgentRuntime>,
        authorizer: Arc<dyn AgentLifecycleAuthorizer>,
    ) -> Self;

    pub async fn start_agent(
        &self,
        principal: &PrincipalId,
        request: StartAgent,
    ) -> Result<AgentExecutionView, StoreError>;
}

pub struct AgentStartCommit<'a> {
    pub principal: &'a PrincipalId,
    pub request: &'a StartAgent,
    pub frozen_definition_id: uuid::Uuid,
    pub frozen_definition_hash: &'a str,
    pub policy: &'a AgentPolicyDecision,
    pub transition: &'a RuntimeTransition,
    pub ceilings: &'a RuntimeCeilings,
}

impl PostgresStore {
    pub async fn replay_agent_start(
        &self,
        principal: &PrincipalId,
        request: &StartAgent,
    ) -> Result<Option<AgentExecutionView>, StoreError>;

    pub async fn commit_agent_start(
        &self,
        commit: AgentStartCommit<'_>,
    ) -> Result<AgentExecutionView, StoreError>;
}

struct AgentActionReplay {
    request_digest: String,
    result_task_revision: u64,
    result_goal_revision: u64,
}

async fn load_action_replay(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    task_id: &UserTaskId,
    action_kind: &str,
    idempotency_key: &str,
) -> Result<Option<AgentActionReplay>, StoreError>;

fn start_request_digest(
    request: &StartAgent,
) -> Result<String, StoreError>;

fn operational_status_for_commands(
    disposition: RuntimeDisposition,
    commands: &[KernelCommand],
) -> Result<OperationalStatus, StoreError>;

// Test-local support in tests/agent_start.rs.
struct StartFixture {
    kernel: Arc<AgentKernel>,
    runtime: Arc<RecordingRuntime>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    accepted: AcceptedUserTask,
}

impl StartFixture {
    async fn accepted_version_one() -> Option<Self>;
    async fn publish_changed_version(&self) -> PublishedDefinition;
    fn start_request(&self, key: &str) -> StartAgent;
    async fn current(&self) -> AgentExecutionView;
    async fn count(&self, table: &str) -> i64;
    async fn reject_agent_command_insert(&self);
}

struct RecordingRuntime;

impl RecordingRuntime {
    fn validated_hashes(&self) -> Vec<String>;
}
```

`TaskOwnerAuthorizer` is the narrow Slice 2 policy implementation: it reads
the current session policy revision, permits only when the authenticated
principal equals the accepted task owner, and returns reason
`authenticated_task_owner`. It has no source-configurable allow fallback.
Later policy expansion replaces this implementation behind the trait.
`AgentKernel::new` copies `*store.agent_runtime_ceilings()` into its private
`ceilings` field, so store reads and kernel validation use one deployment
value. `Application::new` constructs `KiteAgentRuntime` from that same value;
tests using a custom runtime still pass through the kernel/store ceiling.
`replay_agent_start` authenticates the session/task owner and compares the
stored start action digest with a digest of every request field except the
idempotency key. It returns `Some(current_execution)` only for an exact prior
receipt, `IdempotencyConflict` for changed content under the same key, and
`None` when no receipt exists. It performs no mutation; the commit method
repeats the receipt check under locks to close the race.

- [ ] **Step 1: Add failing exact-definition, replay, and race tests**

```rust
// tests/agent_start.rs
#[tokio::test]
async fn start_uses_the_frozen_definition_after_a_newer_publication() {
    let Some(fixture) = StartFixture::accepted_version_one().await else { return };
    let newer = fixture.publish_changed_version().await;
    assert!(newer.version > fixture.accepted.definition.version);

    let started = fixture.kernel.start_agent(
        &fixture.principal,
        StartAgent {
            session_id: fixture.session.id.clone(),
            task_id: fixture.accepted.id.clone(),
            expected_task_revision: 0,
            expected_goal_revision: 0,
            idempotency_key: IdempotencyKey::new("start-frozen-v1").unwrap(),
        },
    ).await.unwrap();

    assert_eq!(started.task.definition.id, fixture.accepted.definition.id);
    assert_eq!(started.task.definition.version, 1);
    assert_eq!(fixture.runtime.validated_hashes(), vec![
        fixture.accepted.definition.definition_hash.clone(),
    ]);
}

#[tokio::test]
async fn concurrent_identical_start_commits_one_snapshot_command_and_event() {
    let Some(fixture) = StartFixture::accepted_version_one().await else { return };
    let request = fixture.start_request("same-start");
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let launch = |barrier: Arc<Barrier>| {
        let kernel = fixture.kernel.clone();
        let principal = fixture.principal.clone();
        let request = request.clone();
        async move { barrier.wait().await; kernel.start_agent(&principal, request).await }
    };
    let first = tokio::spawn(launch(barrier.clone()));
    let second = tokio::spawn(launch(barrier.clone()));
    barrier.wait().await;
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap().unwrap();
    let second = second.unwrap().unwrap();
    assert_eq!(first.task.id, second.task.id);
    assert_eq!(fixture.count("agent_runtime_snapshots").await, 1);
    assert_eq!(fixture.count("agent_transitions").await, 1);
    assert_eq!(fixture.count("agent_commands").await, 1);
    assert_eq!(fixture.count("agent_policy_decisions").await, 1);
    assert_eq!(fixture.count("agent_task_events").await, 1);
}
```

- [ ] **Step 2: Run the focused tests and observe missing kernel start**

Run:

```bash
cargo test --test agent_start start_uses_the_frozen -- --nocapture
cargo test --test agent_start concurrent_identical_start -- --nocapture
```

Expected: FAIL to compile because `AgentKernel` and `commit_agent_start` do not
exist.

- [ ] **Step 3: Implement pure orchestration before the SQL commit**

`AgentKernel::start_agent` first performs the authenticated read-only replay
probe. When no receipt exists, it loads the seed by task id, rejects a runtime
already present, verifies the task's frozen definition id/hash against the
compiled row, validates the agent, requests an owner-policy decision, asks the
pure runtime for the transition, and validates the transition. No write occurs
before `commit_agent_start`.

```rust
pub async fn start_agent(
    &self,
    principal: &PrincipalId,
    request: StartAgent,
) -> Result<AgentExecutionView, StoreError> {
    if let Some(replay) = self.store.replay_agent_start(principal, &request).await? {
        return Ok(replay);
    }
    let seed = self.store.load_agent_start_seed(
        request.session_id.clone(),
        request.task_id.clone(),
    ).await?;
    self.runtime.validate_agent(&seed.definition)
        .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    let policy = self.authorizer.authorize(
        principal,
        &seed.execution.task,
        AgentLifecycleAction::Start,
    ).await?;
    if !policy.allowed {
        return Err(StoreError::AgentTransitionInvalid(
            "current policy denied agent start".into(),
        ));
    }
    let transition = self.runtime.start(RuntimeStart {
        execution: &seed.execution,
        definition: &seed.definition,
    }).map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    validate_transition(&transition, &seed.execution, &self.ceilings)
        .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    self.store.commit_agent_start(AgentStartCommit {
        principal,
        request: &request,
        frozen_definition_id: seed.execution.task.definition.id,
        frozen_definition_hash: &seed.execution.task.definition.definition_hash,
        policy: &policy,
        transition: &transition,
        ceilings: &self.ceilings,
    }).await
}
```

- [ ] **Step 4: Commit start under session -> task -> goal locks**

Inside `commit_agent_start`:

1. validate/canonicalize the request and transition before SQL;
2. begin a transaction and call `lock_agent_aggregate`;
3. verify authenticated principal, active session, an allowed start decision,
   and exact equality between the locked session policy revision and the
   decision policy revision;
4. resolve an existing `(task_id, 'start', idempotency_key)` action before
   revision or lifecycle checks;
5. return the current joined execution when its request digest matches, else
   return `IdempotencyConflict`;
6. require `Accepted`, `NotStarted`, `GoalPhase::Active`, no snapshot, and exact
   request/transition task revision 0 and goal revision 0;
7. compare the locked definition id/hash with the frozen values supplied by
   the seed;
8. insert transition revision 1, its immutable allowed `start` policy decision,
   the full snapshot, and commands in ordinal order;
9. update task to `running`, increment task revision to 1, and leave goal
   phase/revision at `active`/0;
10. append `agent.started` with definition id/hash, policy revision/reason,
    snapshot digest, and command count;
11. insert the start action receipt and commit.

```rust
let snapshot_bytes = commit.transition.snapshot.canonical_bytes(commit.ceilings)
    .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
let snapshot_digest = commit.transition.snapshot.digest(commit.ceilings)
    .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
let command_digests = commit.transition.commands.iter()
    .map(canonical_sha256)
    .collect::<Result<Vec<_>, _>>()
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
let request_digest = start_request_digest(commit.request)?;

let mut tx = self.pool().begin().await?;
let aggregate = lock_agent_aggregate(
    &mut tx,
    &commit.request.session_id,
    &commit.request.task_id,
).await?;
if aggregate.task_principal != commit.principal.as_str() {
    return Err(StoreError::PrincipalMismatch);
}
if let Some(replay) = load_action_replay(
    &mut tx,
    &commit.request.task_id,
    "start",
    commit.request.idempotency_key.as_str(),
).await? {
    if replay.request_digest != request_digest {
        return Err(StoreError::IdempotencyConflict);
    }
    tx.commit().await?;
    return self.get_agent_execution(
        commit.request.session_id.clone(),
        commit.request.task_id.clone(),
    ).await;
}
```

Insert command UUID v7 identities in the store, never in the runtime. Derive
operational status only from all newly inserted external commands: all
`InvokeHarness` -> `WaitingForHarness`; all `RequestContext` ->
`WaitingForContext`; all `WaitForEvent` -> `WaitingForEvent`; all
`RequestApproval` -> `WaitingForApproval`; a returned snapshot -> `Finished`;
mixed external kinds or an internal authority command at start returns
`AgentTransitionInvalid`.

- [ ] **Step 5: Add conflicting replay and transaction-failure coverage**

Test changed expected revisions, task id, definition id/hash, transition
snapshot, and command payload under the same start key. Test a fresh key after
successful start returns `AgentTaskAlreadyTerminal`. Race authorization with a
session policy update and require `PolicyRevisionConflict` with no agent row
mutation. Install a temporary
database trigger that rejects `agent_commands` insertion and prove the failed
transaction leaves task/goal revisions 0, no snapshot, no transition, no
command, no event, and no action receipt.

```rust
#[tokio::test]
async fn command_insert_failure_rolls_back_the_entire_start() {
    let Some(fixture) = StartFixture::accepted_version_one().await else { return };
    fixture.reject_agent_command_insert().await;
    assert!(fixture.kernel.start_agent(
        &fixture.principal,
        fixture.start_request("atomic-start"),
    ).await.is_err());
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Accepted);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!((view.task_revision, view.goal_revision), (0, 0));
    assert!(view.runtime.is_none());
    for table in [
        "agent_runtime_snapshots",
        "agent_transitions",
        "agent_commands",
        "agent_policy_decisions",
        "agent_task_events",
        "agent_task_actions",
    ] {
        assert_eq!(fixture.count(table).await, 0, "unexpected row in {table}");
    }
}
```

- [ ] **Step 6: Prove start/session-close lock compatibility**

Add a lock test that pauses start after it locks the task but before it locks
the goal, then starts session close. Assert session close cannot pass the
session lock; release start and assert both complete without deadlock. Add a
second test that holds a goal lock after the standard session/task prefix and
proves another start does not invert the order.

- [ ] **Step 7: Run and commit atomic start**

Run:

```bash
cargo test --test agent_start --test agent_lifecycle_store --test postgres_lock_order --test task_store
cargo test --test execution_store --test session_store
git diff --check
```

Expected: PASS; every failed start leaves no partial row.

```bash
git add src/agent/kernel.rs src/agent/mod.rs src/store/agent.rs src/store/mod.rs src/store/models.rs src/lib.rs tests/agent_start.rs tests/agent_lifecycle_store.rs tests/postgres_lock_order.rs tests/task_store.rs
git commit -m "feat: start frozen agents atomically"
```

### Task 4: Implement the pure built-in `.kite` snapshot state machine

**Files:**

- Create: `src/agent/kite.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/agent/runtime.rs`
- Modify: `src/agent/types.rs`
- Modify: `src/dsl/ir.rs`
- Modify: `src/lib.rs`
- Create: `tests/kite_agent_runtime.rs`
- Modify: `tests/agent_runtime_contract.rs`
- Modify: `tests/agent_start.rs`

**Interfaces:**

- Consumes: Task 1 `AgentRuntime`, `RuntimeStart`, `RuntimeHandle`, all runtime
  envelope/command/event types, explicit goal IR, Slice 1 context-aware
  `CompiledCall`, typed tasks, completion contracts, `ModelRole`, tool
  descriptors, and `TaskTarget::GithubPullRequest`; Task 3 atomic start uses
  this implementation after its stub-runtime tests pass.
- Produces:

```rust
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct KiteFrameId(u32);

impl KiteFrameId {
    pub const fn new(value: u32) -> Self { Self(value) }
    pub const fn get(self) -> u32 { self.0 }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
pub enum KiteFrameState {
    Ready,
    WaitingForHarness { command_key: RuntimeCommandKey },
    WaitingForApproval {
        command_key: RuntimeCommandKey,
        binding: String,
        answer_type: CompiledType,
    },
    WaitingForChildren { child_ids: Vec<KiteFrameId> },
    Returned { result: serde_json::Value },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KiteFrameSnapshot {
    pub id: KiteFrameId,
    pub parent_id: Option<KiteFrameId>,
    pub parent_binding: Option<String>,
    pub branch_index: u16,
    pub task_name: String,
    pub bindings: BTreeMap<String, serde_json::Value>,
    pub outcome: Option<String>,
    pub statement_index: usize,
    pub history: Vec<RuntimeHistoryItem>,
    pub state: KiteFrameState,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KiteRuntimeState {
    pub definition_hash: String,
    pub root_frame_id: KiteFrameId,
    pub next_frame_id: u32,
    pub next_history_sequence: u64,
    pub steps_consumed: u64,
    pub frames: BTreeMap<KiteFrameId, KiteFrameSnapshot>,
}

pub struct KiteAgentRuntime { ceilings: RuntimeCeilings }

impl KiteAgentRuntime {
    pub const fn new(ceilings: RuntimeCeilings) -> Self;
    pub fn decode_snapshot(
        &self,
        snapshot: &RuntimeStateSnapshot,
    ) -> Result<KiteRuntimeState, AgentRuntimeError>;
}

impl AgentRuntime for KiteAgentRuntime {
    fn validate_agent(&self, definition: &CompiledDefinition)
        -> Result<(), AgentRuntimeError>;
    fn start(&self, input: RuntimeStart<'_>)
        -> Result<RuntimeTransition, AgentRuntimeError>;
    fn handle(&self, input: RuntimeHandle<'_>)
        -> Result<RuntimeTransition, AgentRuntimeError>;
}

#[derive(Clone, Copy)]
struct RuntimeRevisions { task: u64, goal: u64 }

fn accepted_input(task: &AcceptedUserTask) -> serde_json::Value;
fn resolve_call(
    call: &CompiledCall,
    scope: &serde_json::Value,
) -> Result<BTreeMap<String, serde_json::Value>, AgentRuntimeError>;
fn closed_object_schema(
    fields: &BTreeMap<String, CompiledType>,
    definition: &CompiledDefinition,
) -> Result<ClosedSchema, AgentRuntimeError>;
fn completion_result_schema(
    completion: &CompletionContract,
    definition: &CompiledDefinition,
) -> Result<ClosedSchema, AgentRuntimeError>;
fn harness_tools(bindings: &[CompiledToolBinding]) -> Vec<HarnessTool>;
fn remaining_limits(
    definition: &CompiledDefinition,
    state: &KiteRuntimeState,
) -> Result<HarnessStructuralLimits, AgentRuntimeError>;
fn reduce_to_transition(
    state: KiteRuntimeState,
    definition: &CompiledDefinition,
    goal: &GoalContract,
    task_revision: u64,
    goal_revision: u64,
    idempotency_key: String,
    ceilings: &RuntimeCeilings,
) -> Result<RuntimeTransition, AgentRuntimeError>;
fn create_child(
    state: &mut KiteRuntimeState,
    parent_id: KiteFrameId,
    binding: &str,
    call: &CompiledCall,
    branch_index: u16,
    definition: &CompiledDefinition,
) -> Result<KiteFrameSnapshot, AgentRuntimeError>;
fn invoke_harness_for(
    frame_id: KiteFrameId,
    state: &KiteRuntimeState,
    definition: &CompiledDefinition,
    goal: &GoalContract,
    revisions: RuntimeRevisions,
) -> Result<KernelCommand, AgentRuntimeError>;
fn return_frame(
    state: &mut KiteRuntimeState,
    frame_id: KiteFrameId,
    result: serde_json::Value,
) -> Result<(), AgentRuntimeError>;
fn complete_goal_command(
    values: &BTreeMap<String, CompiledValue>,
    bindings: &BTreeMap<String, serde_json::Value>,
    revisions: RuntimeRevisions,
) -> Result<KernelCommand, AgentRuntimeError>;
fn block_goal_command(
    index: u16,
    reason_code: &CompiledValue,
    explanation: &CompiledValue,
    evidence: &[CompiledValue],
    unlock: &[CompiledValue],
    bindings: &BTreeMap<String, serde_json::Value>,
    revisions: RuntimeRevisions,
) -> Result<KernelCommand, AgentRuntimeError>;
fn fail_task_command(
    revisions: RuntimeRevisions,
    code: String,
    message: String,
) -> Result<KernelCommand, AgentRuntimeError>;

// Test-local support in tests/kite_agent_runtime.rs.
struct RuntimeFixture {
    runtime: KiteAgentRuntime,
    execution: AgentExecutionView,
    definition: CompiledDefinition,
}

impl RuntimeFixture {
    fn simple(outcome_body: &str) -> Self;
    fn history_graph() -> Self;
    fn start_input(&self) -> RuntimeStart<'_>;
    fn handle_harness(
        &self,
        snapshot: RuntimeStateSnapshot,
        command: &KernelCommand,
        outcome: &str,
        fields: serde_json::Value,
    ) -> RuntimeTransition;
    fn decode(&self, snapshot: &RuntimeStateSnapshot) -> KiteRuntimeState;
    fn root_history(&self) -> Vec<RuntimeHistoryItem>;
}
```

Frame ids, history sequence values, command keys, transition keys, and command
order derive only from the previous snapshot and event. Root is frame 0;
children take monotonically increasing frame ids; a transition targeting task
revision `N` uses command keys `rN-f<frame>-<kind>` and transition idempotency
`start:<task-uuid>:r0` or `event:<event-id>`.

- [ ] **Step 1: Add failing deterministic start and root-done tests**

```rust
// tests/kite_agent_runtime.rs
#[test]
fn start_is_deterministic_and_emits_one_complete_harness_payload() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let first = fixture.runtime.start(fixture.start_input()).unwrap();
    let second = fixture.runtime.start(fixture.start_input()).unwrap();
    assert_eq!(serde_json::to_value(&first).unwrap(), serde_json::to_value(&second).unwrap());
    assert_eq!(first.expected_task_revision, 0);
    assert_eq!(first.expected_goal_revision, 0);
    let [KernelCommand {
        payload: KernelCommandPayload::InvokeHarness(invocation), ..
    }] = first.commands.as_slice() else { panic!("one harness invocation") };
    assert_eq!(invocation.role, ModelRole::Reasoning);
    assert_eq!(invocation.input, json!({"repository":"acme/payments","pull_request":142}));
    assert_eq!(invocation.goal.objective, fixture.execution.task.goal.objective);
    assert!(invocation.result_schema.validate_definition().is_ok());
    assert_eq!(invocation.tools.len(), 0);
    assert!(invocation.projected_history.is_empty());
}

#[test]
fn root_done_returns_the_task_without_completing_the_goal() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let transition = fixture.handle_harness(
        start.snapshot,
        &start.commands[0],
        "ok",
        json!({"answer":"finished traversal"}),
    );
    assert_eq!(transition.snapshot.disposition, RuntimeDisposition::Returned);
    assert!(transition.commands.is_empty());
    assert!(!serde_json::to_string(&transition).unwrap().contains("complete_goal"));
}
```

- [ ] **Step 2: Run and observe the absent built-in runtime**

Run:

```bash
cargo test --test kite_agent_runtime start_is_deterministic -- --nocapture
cargo test --test kite_agent_runtime root_done_returns -- --nocapture
```

Expected: FAIL to compile because `KiteAgentRuntime` and snapshot types do not
exist.

- [ ] **Step 3: Validate and start only version-2 agent definitions**

`validate_agent` requires `DefinitionKind::Agent`, `dsl_version == 2`, a
present entry task, one `ModelRole` on every task, all context modes non-legacy,
valid completion schemas, and no tool descriptor/hash mismatch. It also
rejects more than 256 tasks because each task can become a live frame under
the Slice 2 ceiling.

`decode_snapshot` first calls `snapshot.canonical_bytes(&self.ceilings)` so an
integrity or lowered-ceiling failure occurs before deserializing
`KiteRuntimeState`; it then requires the embedded definition hash and frame
counters to match the immutable definition and envelope ceilings.

`start` projects the fixed V1 target to agent inputs:

```rust
fn accepted_input(task: &AcceptedUserTask) -> serde_json::Value {
    match &task.target {
        TaskTarget::GithubPullRequest { repository, number } => json!({
            "repository": repository,
            "pull_request": number,
        }),
    }
}
```

Validate the projected object against the compiled definition inputs, resolve
the entry call into root bindings, create `KiteRuntimeState` with frame 0, and
run the reducer until it emits the first `InvokeHarness`. Do not persist UUIDs,
timestamps, or provider metadata in the snapshot.

```rust
let state = KiteRuntimeState {
    definition_hash: input.definition.definition_hash.clone(),
    root_frame_id: KiteFrameId(0),
    next_frame_id: 1,
    next_history_sequence: 0,
    steps_consumed: 0,
    frames: BTreeMap::from([(
        KiteFrameId(0),
        KiteFrameSnapshot {
            id: KiteFrameId(0),
            parent_id: None,
            parent_binding: None,
            branch_index: 0,
            task_name: input.definition.entry.task.clone(),
            bindings: resolve_call(&input.definition.entry, &accepted_input(input.execution))?,
            outcome: None,
            statement_index: 0,
            history: Vec::new(),
            state: KiteFrameState::Ready,
        },
    )]),
};
reduce_to_transition(
    state,
    input.definition,
    &input.execution.task.goal,
    input.execution.task_revision,
    input.execution.goal_revision,
    format!("start:{}:r0", input.execution.task.id.as_uuid()),
    &self.ceilings,
)
```

- [ ] **Step 4: Build the complete provider-free `InvokeHarness` payload**

For each ready frame, derive:

- role and prompt from its compiled task;
- input value and closed schema from compiled task inputs;
- result schema as a closed object with `outcome` enum and exact per-outcome
  field union from `CompletionContract`;
- tools from frozen compiled bindings, sorted by qualified name;
- a goal projection containing objective, criterion id/evidence-type pairs,
  and constraints, but not blockers that the task has not selected;
- history already projected into that frame;
- remaining structural limits from the immutable definition and snapshot
  counters.

```rust
let command = KernelCommand {
    schema_version: 1,
    key: RuntimeCommandKey::new(format!(
        "r{}-f{}-harness",
        expected_task_revision + 1,
        frame.id.0,
    ))?,
    payload: KernelCommandPayload::InvokeHarness(InvokeHarness {
        role: task.model_role.ok_or_else(|| AgentRuntimeError::InvalidDefinition(
            "agent task has no model role".into(),
        ))?,
        prompt: task.prompt.clone(),
        goal: GoalProjection::from_contract(goal),
        input: serde_json::to_value(&frame.bindings)
            .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?,
        input_schema: closed_object_schema(&task.inputs, definition)?,
        result_schema: completion_result_schema(&task.completion, definition)?,
        tools: harness_tools(&task.tools),
        projected_history: frame.history.clone(),
        limits: remaining_limits(definition, state)?,
    }),
};
```

No field selects `.kite`, the direct loop, a provider, endpoint, credential,
model, or subscription.

- [ ] **Step 5: Handle harness outcomes and reduce traversal deterministically**

`handle` decodes the snapshot, checks definition hash, finds exactly one frame
waiting on the event command key, rejects other event variants for that wait,
validates the outcome and exact fields against `CompletionContract`, appends
the returned normalized history in order, binds outcome fields, injects
`current_evidence` from `HarnessResult.evidence_id` only after those checks,
and reduces statements until the next durable command or terminal disposition.
The injected UUID may be returned through `done`/structured calls; it is
removed when the next harness outcome is selected and replaced only by that
new validated event.

Implement every statement:

```rust
match statement {
    CompiledStatement::Call { binding, call } => {
        let child = create_child(state, frame_id, binding, call, 0, definition)?;
        parent.state = KiteFrameState::WaitingForChildren { child_ids: vec![child.id] };
        parent.statement_index += 1;
        commands.push(invoke_harness_for(child.id, state, definition, goal, revisions)?);
    }
    CompiledStatement::Parallel { calls } => {
        let child_ids = calls.iter().enumerate().map(|(branch, call)| {
            create_child(state, frame_id, &call.binding, &call.call, branch as u16, definition)
                .map(|child| child.id)
        }).collect::<Result<Vec<_>, _>>()?;
        parent.state = KiteFrameState::WaitingForChildren { child_ids: child_ids.clone() };
        parent.statement_index += 1;
        for child_id in child_ids {
            commands.push(invoke_harness_for(child_id, state, definition, goal, revisions)?);
        }
    }
    CompiledStatement::Ask { binding, answer_type, question } => {
        let key = command_key(revisions.task + 1, frame_id, "approval")?;
        let text = resolve_string(question, &frame.bindings)?;
        frame.state = KiteFrameState::WaitingForApproval {
            command_key: key.clone(),
            binding: binding.clone(),
            answer_type: answer_type.clone(),
        };
        frame.statement_index += 1;
        commands.push(KernelCommand {
            schema_version: 1,
            key,
            payload: KernelCommandPayload::RequestApproval(RequestApproval {
                question: text,
                answer_schema: closed_schema(definition, answer_type)?,
            }),
        });
    }
    CompiledStatement::Goto { call } => {
        let next = resolve_call(call, &frame.bindings)?;
        frame.task_name = call.task.clone();
        frame.bindings = next;
        frame.outcome = None;
        frame.statement_index = 0;
        if call.context == ContextMode::Isolated { frame.history.clear(); }
        frame.state = KiteFrameState::Ready;
    }
    CompiledStatement::Done { value } => return_frame(state, frame_id, resolve(value, &frame.bindings)?)?,
    CompiledStatement::Fail { code, message } => commands.push(fail_task_command(
        revisions,
        resolve_string(code, &frame.bindings)?,
        resolve_string(message, &frame.bindings)?,
    )?),
    CompiledStatement::CompleteGoal { criterion_evidence } => {
        commands.push(complete_goal_command(criterion_evidence, &frame.bindings, revisions)?);
    }
    CompiledStatement::BlockGoal {
        blocked_when_index, reason_code, explanation, evidence, unlock_conditions,
    } => {
        commands.push(block_goal_command(
            *blocked_when_index, reason_code, explanation, evidence,
            unlock_conditions, &frame.bindings, revisions,
        )?);
    }
}
```

Increment `steps_consumed` before each statement reduction and reject the next
step after the compiled agent's immutable `steps` limit. Derive per-frame
model turns from normalized harness-output history; the other structural
limits are the frozen per-invocation values. No wall clock enters the runtime
snapshot or transition.

When a child returns, insert its result into the parent binding. Resume the
parent only after every child id in the structured group is `Returned`. A
child never survives its join. Sort runnable children by `(branch_index, id)`
before generating ordered commands.

- [ ] **Step 6: Enforce isolated, kept, fork, and return history boundaries**

For `call context isolated`, the child starts with no history. For `call
context kept`, copy the parent's current ordered history. Each fork child gets
its own copy according to its own call context; later child history is never
merged into a sibling or parent. A tail return with isolated context clears the
current frame history; `kept` retains it.

```rust
#[test]
fn isolated_kept_and_fork_histories_have_exact_boundaries() {
    let fixture = RuntimeFixture::history_graph();
    let after_root = fixture.complete_root("split", json!({
        "left":"left-input", "right":"right-input"
    }));
    let invocations = harness_invocations(&after_root.commands);
    assert_eq!(invocations.len(), 2);
    assert!(invocations[0].projected_history.is_empty());
    assert_eq!(invocations[1].projected_history, fixture.root_history());

    let after_left = fixture.complete_branch(after_root.snapshot.clone(), 0, "ok");
    let right_state = fixture.decode(&after_left.snapshot)
        .frames[&KiteFrameId::new(2)].clone();
    assert_eq!(right_state.history, fixture.root_history());
    assert!(!right_state.history.iter().any(|item| item.payload == json!("left result")));
}
```

- [ ] **Step 7: Prove explicit authority, failure, ask, and forged-result behavior**

Add tests that:

- `complete goal` emits one `CompleteGoal` with criterion ids and parsed UUIDs;
- `block goal when 1` emits one `BlockGoal` with index 1 and ordered evidence;
- `done current_evidence` returns the kernel event's UUID through a child call,
  while an outcome field named `current_evidence` is schema/compiler-invalid;
- root `done` emits no `CompleteGoal`;
- `fail` emits `FailTask`, never `BlockGoal` or `CompleteGoal`;
- `ask` emits typed `RequestApproval` and waits;
- an approval answer of the wrong type fails without mutating the supplied
  snapshot;
- a harness result for a non-waiting or already-consumed command key fails;
- a `HarnessResult` JSON object containing `goalPhase`, `completeGoal`, or
  unknown fields fails deserialization;
- an undeclared outcome, missing/extra/wrong-type field, duplicate history
  sequence, oversized history, or definition-hash mismatch fails.

```rust
#[test]
fn harness_result_cannot_carry_goal_authority() {
    let forged = json!({
        "type": "harness_result",
        "payload": {
            "event_id": "evt-1",
            "command_key": "r1-f0-harness",
            "outcome": "ok",
            "fields": {},
            "evidence_id": Uuid::nil(),
            "history": [],
            "goalPhase": "complete"
        }
    });
    assert!(serde_json::from_value::<RuntimeEvent>(forged).is_err());
}
```

- [ ] **Step 8: Run and commit the pure runtime**

Run:

```bash
cargo test --test kite_agent_runtime --test agent_runtime_contract --test agent_start
cargo test --test agent_compiler --test transition_engine
git diff --check
```

Expected: PASS; no test needs PostgreSQL except `agent_start`.

```bash
git add src/agent/kite.rs src/agent/mod.rs src/agent/runtime.rs src/agent/types.rs src/dsl/ir.rs src/lib.rs tests/kite_agent_runtime.rs tests/agent_runtime_contract.rs tests/agent_start.rs
git commit -m "feat: execute pure kite agent snapshots"
```

## Cross-task review gate A: Tasks 1-4

Stop before Task 5. Dispatch a fresh independent reviewer using the cross-task
review protocol. Save the initial verdict to:

```text
.superpowers/sdd/2026-08-17-kiteframe-declarative-agents-slice-2/cross-task-review-1-4.md
```

The review covers the exact Task 1 base through Task 4 head and must prove:

- the lifecycle, command, evidence, explicit goal syntax, schema, atomic start,
  and pure snapshot runtime jointly implement the applicable design sections;
- the Slice 1 immutable task, goal, publication, protocol, hash, and legacy
  workflow contracts remain unchanged;
- task/goal/operational axes, complete/block authority, snapshot ceilings,
  frozen-definition selection, lock order, and deterministic traversal agree
  across Rust types, DSL IR, PostgreSQL schema, fixtures, and tests;
- Task 5 can consume the exact `RuntimeTransition`, `RuntimeStateSnapshot`,
  `KernelCommand`, revisions, persisted rows, and lock helpers promised by its
  `Interfaces` block without an adapter or semantic reinterpretation;
- Tasks 5-8 remain implementable without weakening Tasks 1-4; and
- the reviewed contracts leave the Slice 3 command/harness seam possible while
  introducing none of the explicit Slice 3-8 exclusions.

Run at minimum, with configured disposable PostgreSQL:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --test agent_runtime_contract --test agent_lifecycle_store --test agent_start --test kite_agent_runtime --test agent_compiler --test agent_parser --test goal_contract --test definition_store --test task_store --test postgres_lock_order
git diff --check
git status --short
```

Expected: every command passes, PostgreSQL cases execute rather than skip, the
coverage matrix has no unmapped applicable requirement, and the report verdict
is `approved`. If the verdict is `changes_required`, write fixes and
`cross-task-review-1-4-fix-report.md`, then obtain a fresh approved re-review.
Do not begin Task 5 before approval.

### Task 5: Commit CAS transitions, command journal, and recovery without replay

**Files:**

- Modify: `src/agent/kernel.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/agent/runtime.rs`
- Modify: `src/agent/types.rs`
- Modify: `src/store/agent.rs`
- Modify: `src/store/mod.rs`
- Modify: `src/store/models.rs`
- Modify: `src/lib.rs`
- Create: `tests/agent_transition_store.rs`
- Create: `tests/e2e_agent_restart.rs`
- Modify: `tests/agent_lifecycle_store.rs`
- Modify: `tests/agent_start.rs`
- Modify: `tests/postgres_lock_order.rs`

**Interfaces:**

- Consumes: Task 4 pure `AgentRuntime::handle` and complete replacement
  snapshots; Task 3 `AgentKernel`, lock order, exact start journal; Task 2
  command/transition tables and strict readers.
- Produces the internal Slice 3 command handoff unchanged:

```rust
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimAgentCommand {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub command_id: AgentCommandId,
    pub expected_command_revision: u64,
    pub claimant: String,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteAgentCommand {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub command_id: AgentCommandId,
    pub expected_command_revision: u64,
    pub idempotency_key: IdempotencyKey,
    pub event: RuntimeEvent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailAgentCommand {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub command_id: AgentCommandId,
    pub expected_command_revision: u64,
    pub idempotency_key: IdempotencyKey,
    pub code: String,
}

#[derive(Clone, Debug)]
pub struct AgentCommandContext {
    pub execution: AgentExecutionView,
    pub definition: CompiledDefinition,
    pub command: AgentCommandView,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentRecoveryReport {
    pub pending: u64,
    pub claimed: u64,
}

impl PostgresStore {
    pub async fn dispatchable_agent_commands(
        &self,
        limit: i64,
    ) -> Result<Vec<AgentCommandView>, StoreError>;

    pub async fn claim_agent_command(
        &self,
        request: ClaimAgentCommand,
    ) -> Result<AgentCommandView, StoreError>;

    pub async fn load_agent_command_context(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
        command_id: AgentCommandId,
    ) -> Result<AgentCommandContext, StoreError>;

    pub async fn replay_agent_command_completion(
        &self,
        principal: &PrincipalId,
        completion: &CompleteAgentCommand,
    ) -> Result<Option<AgentExecutionView>, StoreError>;

    pub async fn commit_agent_command_transition(
        &self,
        principal: &PrincipalId,
        completion: &CompleteAgentCommand,
        transition: &RuntimeTransition,
        policy: Option<&AgentPolicyDecision>,
        ceilings: &RuntimeCeilings,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn fail_unavailable_agent_command(
        &self,
        principal: &PrincipalId,
        request: &FailAgentCommand,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn recover_agent_commands(
        &self,
        limit: i64,
    ) -> Result<AgentRecoveryReport, StoreError>;
}

impl AgentKernel {
    pub async fn complete_command(
        &self,
        principal: &PrincipalId,
        request: CompleteAgentCommand,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn fail_command(
        &self,
        principal: &PrincipalId,
        request: FailAgentCommand,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn recover(&self, limit: i64)
        -> Result<AgentRecoveryReport, StoreError>;
}

// Test-local support in tests/agent_transition_store.rs.
struct TransitionFixture {
    kernel: Arc<AgentKernel>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    task_id: UserTaskId,
    ceilings: RuntimeCeilings,
}

impl TransitionFixture {
    async fn started_simple_agent() -> Option<Self>;
    async fn started_fork_agent() -> Option<Self>;
    async fn claim_first(&self, claimant: &str, key: &str) -> AgentCommandView;
    async fn current(&self) -> AgentExecutionView;
    fn harness_result(
        &self,
        command: &AgentCommandView,
        outcome: &str,
        fields: serde_json::Value,
    ) -> RuntimeEvent;
    fn completion(
        &self,
        command: &AgentCommandView,
        event_id: &str,
        outcome: &str,
        fields: serde_json::Value,
    ) -> CompleteAgentCommand;
    async fn commands_for_latest_transition(&self) -> Vec<AgentCommandView>;
    async fn transition_count_for(&self, key: &str) -> i64;
    fn runtime_handle_calls(&self) -> usize;
    async fn reject_command_ordinal(&self, ordinal: i32);
    async fn database_fingerprint(&self) -> serde_json::Value;
}

// Test-local support in tests/e2e_agent_restart.rs.
struct RestartAgentFixture {
    kernel: Arc<AgentKernel>,
    store: Arc<PostgresStore>,
    database_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RestartAuthoritySnapshot {
    execution_json: serde_json::Value,
    command_rows: Vec<serde_json::Value>,
    transition_count: i64,
    event_count: i64,
}

impl RestartAgentFixture {
    async fn started_fork() -> Option<Self>;
    async fn complete_root_into_two_pending_children(&mut self);
    async fn authority_snapshot(&self) -> RestartAuthoritySnapshot;
    async fn authority_snapshot_from(store: &PostgresStore) -> RestartAuthoritySnapshot;
}
```

`dispatchable_agent_commands` returns both pending and claimed rows ordered by
`created_at, id`; a claimed row is redelivered with its existing claimant and
revision. Slice 2 adds no lease, reassignment, or fencing semantics.

- [ ] **Step 1: Add failing CAS, duplicate, and ordering tests**

```rust
// tests/agent_transition_store.rs
#[tokio::test]
async fn command_result_replaces_snapshot_and_journals_commands_atomically() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else { return };
    let command = fixture.claim_first("builtin-direct-v1", "claim-root").await;
    let before = fixture.current().await;
    let event = fixture.harness_result(&command, "split", json!({
        "left":"one", "right":"two"
    }));
    let after = fixture.kernel.complete_command(
        &fixture.principal,
        CompleteAgentCommand {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: command.id.clone(),
            expected_command_revision: command.revision,
            idempotency_key: IdempotencyKey::new("complete-root").unwrap(),
            event,
        },
    ).await.unwrap();

    assert_eq!(after.task_revision, before.task_revision + 1);
    assert_eq!(after.goal_revision, before.goal_revision);
    assert_ne!(after.runtime.as_ref().unwrap().digest(&fixture.ceilings).unwrap(),
        before.runtime.as_ref().unwrap().digest(&fixture.ceilings).unwrap());
    let commands = fixture.commands_for_latest_transition().await;
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].ordinal, 0);
    assert_eq!(commands[1].ordinal, 1);
}

#[tokio::test]
async fn duplicate_event_is_one_transition_and_conflicting_content_is_rejected() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else { return };
    let command = fixture.claim_first("builtin-direct-v1", "claim").await;
    let request = fixture.completion(&command, "evt-stable", "ok", json!({"answer":"x"}));
    let first = fixture.kernel.complete_command(&fixture.principal, request.clone()).await.unwrap();
    let replay = fixture.kernel.complete_command(&fixture.principal, request).await.unwrap();
    assert_eq!(first.task_revision, replay.task_revision);
    assert_eq!(fixture.transition_count_for("event:evt-stable").await, 1);
    assert_eq!(fixture.runtime_handle_calls(), 1);

    let changed = fixture.completion(&command, "evt-stable", "ok", json!({"answer":"changed"}));
    assert!(matches!(
        fixture.kernel.complete_command(&fixture.principal, changed).await,
        Err(StoreError::IdempotencyConflict),
    ));
    assert_eq!(fixture.transition_count_for("event:evt-stable").await, 1);
}
```

- [ ] **Step 2: Run and observe missing command APIs**

Run:

```bash
cargo test --test agent_transition_store command_result_replaces -- --nocapture
cargo test --test agent_transition_store duplicate_event -- --nocapture
```

Expected: FAIL to compile because claim, completion, and CAS transition methods
do not exist.

- [ ] **Step 3: Implement revisioned idempotent command claim**

Validate claimant as 1-128 printable non-whitespace ASCII bytes. Compute the
claim digest over task id, command id, expected revision, and claimant, but not
the idempotency key. Lock session -> task -> goal -> command. Resolve a stored
claim idempotency key before revision/state checks. The first claim changes
`pending` to `claimed`, records claimant/key/digest, increments command
revision, and appends `command.claimed` without changing task or goal revision.
The same key+digest returns the same claimed row; changed content returns
`IdempotencyConflict`; another key against claimed/completed/failed returns
`RevisionConflict`.

```rust
let updated = sqlx::query(
    "UPDATE agent_commands SET state = 'claimed', claimant = $1, \
     claim_idempotency_key = $2, claim_request_digest = $3, \
     revision = revision + 1, updated_at = CURRENT_TIMESTAMP \
     WHERE id = $4 AND task_id = $5 AND state = 'pending' AND revision = $6 \
     RETURNING revision",
)
.bind(&request.claimant)
.bind(request.idempotency_key.as_str())
.bind(&request_digest)
.bind(request.command_id.as_uuid())
.bind(request.task_id.as_uuid())
.bind(request.expected_command_revision as i64)
.fetch_optional(&mut *tx).await?;
if updated.is_none() { return Err(StoreError::RevisionConflict); }
```

- [ ] **Step 4: Run the runtime before the transition transaction**

`complete_command` first performs an authenticated read-only completion-receipt
probe. An exact receipt returns the current execution without calling the
runtime; changed event content under the same key returns
`IdempotencyConflict`. With no receipt, it loads the immutable command context,
verifies the event command key equals the persisted command key and contains
no goal authority, calls the pure runtime, validates the full transition
against the loaded execution, and passes both completion and proposal to the
store. Runtime errors cause no command mutation. The transition transaction
rechecks the receipt under locks to close a concurrent completion race.

```rust
pub async fn complete_command(
    &self,
    principal: &PrincipalId,
    request: CompleteAgentCommand,
) -> Result<AgentExecutionView, StoreError> {
    if let Some(replay) = self.store
        .replay_agent_command_completion(principal, &request).await?
    {
        return Ok(replay);
    }
    let context = self.store.load_agent_command_context(
        request.session_id.clone(),
        request.task_id.clone(),
        request.command_id.clone(),
    ).await?;
    if request.event.command_key() != Some(&context.command.key) {
        return Err(StoreError::AgentTransitionInvalid(
            "event command key does not match persisted command".into(),
        ));
    }
    let transition = self.runtime.handle(RuntimeHandle {
        execution: &context.execution,
        definition: &context.definition,
        event: &request.event,
    }).map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    validate_transition(&transition, &context.execution, &self.ceilings)
        .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    self.store.commit_agent_command_transition(
        principal,
        &request,
        &transition,
        None,
        &self.ceilings,
    ).await
}
```

`fail_command` creates a closed `RuntimeEvent::CommandFailed` whose event id is
`command-failed:<command-uuid>:<completion-key>`, calls the same path, and
requires the runtime to return exactly one `FailTask` command. It never maps a
definitive harness failure to `BlockGoal` or `CompleteGoal`.

- [ ] **Step 5: Commit event, source command, snapshot, and new commands in one CAS**

Before SQL, canonicalize completion event, transition, snapshot, and each
command. Inside one transaction:

1. lock session -> task -> goal -> source command;
2. resolve `(command_id, completion_idempotency_key)` replay before revisions;
3. require active session, authenticated owner, running task, active goal,
   current task/goal revisions equal transition expectations, source command
   claimed, and command revision equals request; when a policy decision is
   present, also require it is allowed and its revision equals the locked
   session policy revision;
4. require event id equals the transition key suffix and command key matches;
5. validate every new command schema and reject duplicate runtime keys;
6. mark source command completed with result and increment command revision;
7. insert transition and full replacement snapshot at task revision + 1;
8. insert new commands in vector order;
9. if snapshot is `Returned`, set lifecycle `succeeded` and operational
   `finished` while leaving goal active; if it is `Running`, derive the wait;
   if exactly one `FailTask` is proposed, store that command completed and set
   task failed/finished without changing goal;
10. append one ordered event and commit.

`CompleteGoal` and `BlockGoal` proposals return
`AgentTransitionInvalid("goal authority is not enabled until Task 6")` in
this task; no partial transition is written.

`fail_unavailable_agent_command` requires
`request.code == "runtime_command_unavailable"`, hashes every request field
except the idempotency key, and resolves the command's stored completion
receipt before state/revision checks. It atomically marks the source command
failed, sets the task failed/finished, advances the snapshot row's task
revision without changing its payload/integrity fields, leaves the goal
unchanged, appends the fixed `runtime_command_unavailable` failure event, and
cancels sibling commands. An identical replay returns the committed execution;
changed content returns `IdempotencyConflict`. It never invokes the runtime because
unavailable external work has no result to reduce.

```rust
let next_task_revision = aggregate.task_revision + 1;
let next_goal_revision = aggregate.goal_revision;
sqlx::query(
    "UPDATE agent_runtime_snapshots SET task_revision = $1, schema_version = $2, \
     runtime_id = $3, disposition = $4, payload = $5, canonical_bytes = $6, \
     snapshot_digest = $7, updated_at = CURRENT_TIMESTAMP WHERE task_id = $8",
)
.bind(next_task_revision as i64)
.bind(i32::from(transition.snapshot.schema_version))
.bind(&transition.snapshot.runtime_id)
.bind(disposition_name(transition.snapshot.disposition))
.bind(&transition.snapshot.payload)
.bind(snapshot_bytes.len() as i32)
.bind(&snapshot_digest)
.bind(completion.task_id.as_uuid())
.execute(&mut *tx).await?;
```

- [ ] **Step 6: Prove stale/duplicate/foreign/failure atomicity**

Test stale task revision, goal revision, command revision, task id, session id,
principal, command key, source event id, snapshot digest, unknown command
schema, duplicate command key, oversized snapshot/command/history, mixed
external command kinds, second completion key, and a result after task
terminal. Every failure keeps source command state/revision, task/goal
revisions, snapshot digest, transition count, new-command count, and event
count unchanged.

Install failure triggers separately on snapshot update, transition insert,
second command insert, and task event insert. Each induced error rolls back the
source-command completion as well as every new row.

```rust
#[tokio::test]
async fn second_command_insert_failure_rolls_back_source_completion() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else { return };
    let command = fixture.claim_first("builtin-direct-v1", "claim-root").await;
    fixture.reject_command_ordinal(1).await;
    let before = fixture.database_fingerprint().await;
    assert!(fixture.kernel.complete_command(
        &fixture.principal,
        fixture.completion(&command, "evt-fork", "split", json!({
            "left":"one", "right":"two"
        })),
    ).await.is_err());
    assert_eq!(fixture.database_fingerprint().await, before);
}
```

- [ ] **Step 7: Implement exact restart recovery**

`recover_agent_commands` performs read-only integrity validation of all
nonterminal tasks with pending or claimed commands. It verifies each current
snapshot, every command payload digest, and that command runtime keys appear
as waits in the snapshot. It returns counts only and changes no row. A
claimed command remains claimed by the same claimant; pending remains pending.

```rust
// tests/e2e_agent_restart.rs
#[tokio::test]
async fn restart_restores_snapshot_and_pending_commands_without_replay() {
    let Some(mut fixture) = RestartAgentFixture::started_fork().await else { return };
    fixture.complete_root_into_two_pending_children().await;
    let before = fixture.authority_snapshot().await;
    let database_url = fixture.database_url.clone();
    drop(fixture.kernel);
    drop(fixture.store);

    let reopened = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let report = reopened.recover_agent_commands(32).await.unwrap();
    let after = RestartAgentFixture::authority_snapshot_from(&reopened).await;
    assert_eq!(report, AgentRecoveryReport { pending: 2, claimed: 0 });
    assert_eq!(after, before);
    assert_eq!(after.transition_count, 2);
    assert_eq!(after.event_count, 2);
}
```

Add claimed-command restart, completed-command exclusion, corrupt snapshot,
corrupt command digest, and task-command mismatch cases. Recovery reports
corruption and performs no repair or replay.

- [ ] **Step 8: Run and commit transition authority**

Run:

```bash
cargo test --test agent_transition_store --test e2e_agent_restart --test postgres_lock_order
cargo test --test agent_start --test agent_lifecycle_store --test kite_agent_runtime
git diff --check
```

Expected: PASS; restart does not add a transition, event, or command.

```bash
git add src/agent/kernel.rs src/agent/mod.rs src/agent/runtime.rs src/agent/types.rs src/store/agent.rs src/store/mod.rs src/store/models.rs src/lib.rs tests/agent_transition_store.rs tests/e2e_agent_restart.rs tests/agent_lifecycle_store.rs tests/agent_start.rs tests/postgres_lock_order.rs
git commit -m "feat: journal agent transitions and commands"
```

### Task 6: Enforce evidence-backed complete/block and principal lifecycle actions

**Files:**

- Modify: `src/agent/kernel.rs`
- Modify: `src/agent/kite.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/agent/runtime.rs`
- Modify: `src/agent/types.rs`
- Modify: `src/goal.rs`
- Modify: `src/lib.rs`
- Modify: `src/task.rs`
- Modify: `src/store/agent.rs`
- Modify: `src/store/models.rs`
- Create: `tests/agent_goal_authority.rs`
- Modify: `tests/agent_lifecycle_store.rs`
- Modify: `tests/agent_transition_store.rs`
- Modify: `tests/e2e_agent_restart.rs`
- Modify: `tests/kite_agent_runtime.rs`
- Modify: `tests/postgres_lock_order.rs`

**Interfaces:**

- Consumes: Task 5 atomic transition commit and command completion; Task 3
  `AgentLifecycleAuthorizer`; Task 1 evidence, completion, blocker, pause,
  resume, cancel, goal phase, task lifecycle, and operational types.
- Produces:

```rust
// Task 1's PrincipalResumed event is accepted only by the authenticated
// resume path implemented in this task. Command completion continues to
// require command_key() == Some(persisted_key).

impl PostgresStore {
    pub async fn pause_user_task(
        &self,
        principal: &PrincipalId,
        request: PauseUserTask,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn resume_user_task(
        &self,
        principal: &PrincipalId,
        request: ResumeUserTask,
        blocked_transition: Option<&RuntimeTransition>,
        ceilings: &RuntimeCeilings,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn cancel_user_task(
        &self,
        principal: &PrincipalId,
        request: CancelUserTask,
    ) -> Result<AgentExecutionView, StoreError>;
}

impl AgentKernel {
    pub async fn pause_task(
        &self,
        principal: &PrincipalId,
        request: PauseUserTask,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn resume_task(
        &self,
        principal: &PrincipalId,
        request: ResumeUserTask,
    ) -> Result<AgentExecutionView, StoreError>;

    pub async fn cancel_task(
        &self,
        principal: &PrincipalId,
        request: CancelUserTask,
    ) -> Result<AgentExecutionView, StoreError>;
}

struct AuthorizedEvidence {
    evidence_type: String,
    source_authorized: bool,
    digest_current: bool,
}

const fn goal_edge_allowed(from: GoalPhase, to: GoalPhase) -> bool;

// Test-local support in tests/agent_goal_authority.rs.
struct GoalFixture {
    kernel: Arc<AgentKernel>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    task_id: UserTaskId,
}

impl GoalFixture {
    async fn three_harness_criteria() -> Option<Self>;
    async fn blocked_conditions() -> Option<Self>;
    async fn completed_harness_evidence(&self, count: usize) -> Vec<EvidenceRecord>;
    async fn current(&self) -> AgentExecutionView;
    async fn complete_goal<const N: usize>(
        &self,
        task_revision: u64,
        goal_revision: u64,
        assertions: [(&str, EvidenceId); N],
    ) -> Result<AgentExecutionView, StoreError>;
    async fn block_goal(
        &self,
        task_revision: u64,
        goal_revision: u64,
        index: u16,
        code: &str,
        explanation: &str,
        evidence: Vec<EvidenceId>,
        unlock: Vec<String>,
    ) -> Result<AgentExecutionView, StoreError>;
    async fn assertions(&self) -> Vec<CriterionAssertion>;
    async fn blocker_history(&self) -> Vec<GoalBlocker>;
    async fn database_fingerprint(&self) -> serde_json::Value;
    async fn invalid_completion_cases() -> Vec<InvalidCompletionCase>;
}

struct InvalidCompletionCase {
    name: &'static str,
    fixture: GoalFixture,
    expected: ExpectedStoreError,
    request: CompleteAgentCommand,
}

impl InvalidCompletionCase {
    async fn execute(&self) -> Result<AgentExecutionView, StoreError>;
}

enum ExpectedStoreError {
    Incomplete,
    NotFound,
    TypeMismatch,
    Unauthorized,
    Stale,
    TaskRevision,
    GoalRevision,
    PolicyRevision,
    TransitionInvalid,
    Terminal,
}

impl ExpectedStoreError {
    fn matches(&self, error: &StoreError) -> bool;
}
```

Extend `StoreError` with:

```rust
#[error("goal evidence is stale")]
GoalEvidenceStale,
```

It maps to the same coarse external `goal_evidence_invalid` code as the other
evidence errors. In this slice, stale has one exact integrity meaning: the
source command is no longer `completed`, or the evidence digest does not equal
the canonical digest of that completed command's stored result. Time expiry is
excluded until context artifacts land.

- [ ] **Step 1: Add the failing exact-completion and blocker tests**

```rust
// tests/agent_goal_authority.rs
#[tokio::test]
async fn complete_goal_requires_exact_authorized_criterion_evidence() {
    let Some(fixture) = GoalFixture::three_harness_criteria().await else { return };
    let evidence = fixture.completed_harness_evidence(3).await;
    let before = fixture.current().await;
    let after = fixture.complete_goal(
        before.task_revision,
        before.goal_revision,
        [
            ("reviews_resolved", evidence[0].id.clone()),
            ("checks_passed", evidence[1].id.clone()),
            ("pull_request_merged", evidence[2].id.clone()),
        ],
    ).await.unwrap();
    assert_eq!(after.lifecycle, TaskLifecycle::Succeeded);
    assert_eq!(after.operational_status, OperationalStatus::Finished);
    assert_eq!(after.goal_phase, GoalPhase::Complete);
    assert_eq!(after.task_revision, before.task_revision + 1);
    assert_eq!(after.goal_revision, before.goal_revision + 1);
    assert_eq!(fixture.assertions().await.len(), 3);
}

#[tokio::test]
async fn block_goal_binds_zero_based_contract_condition_and_history_is_immutable() {
    let Some(fixture) = GoalFixture::blocked_conditions().await else { return };
    let evidence = fixture.completed_harness_evidence(1).await.remove(0);
    let before = fixture.current().await;
    let blocked = fixture.block_goal(
        before.task_revision,
        before.goal_revision,
        1,
        "no_policy_permitted_path",
        "All currently permitted repair paths were exhausted.",
        vec![evidence.id],
        vec!["A policy revision permits a bounded repair.".into()],
    ).await.unwrap();
    assert_eq!(blocked.goal_phase, GoalPhase::Blocked);
    assert_eq!(blocked.operational_status, OperationalStatus::Blocked);
    assert_eq!(blocked.blocker.as_ref().unwrap().blocked_when_index, 1);
    assert_eq!(fixture.blocker_history().await.len(), 1);
}
```

- [ ] **Step 2: Run and observe goal commands are still rejected**

Run:

```bash
cargo test --test agent_goal_authority complete_goal_requires_exact -- --nocapture
cargo test --test agent_goal_authority block_goal_binds -- --nocapture
```

Expected: FAIL with Task 5's `goal authority is not enabled until Task 6`
transition error.

After the pure runtime returns a transition, inspect its sole internal command.
For `CompleteGoal` or `BlockGoal`, call `AgentLifecycleAuthorizer::authorize`
with the authenticated principal and corresponding action, require `allowed`,
and pass `Some(&decision)` to `commit_agent_command_transition`. Pass `None`
for external commands, root return, and `FailTask`. The store rejects a goal
command without a matching allowed decision. The same transaction inserts one
immutable `agent_policy_decisions` row linked to the transition and records the
same policy revision and reason code in the committed goal event.

```rust
let policy = match transition.commands.as_slice() {
    [KernelCommand { payload: KernelCommandPayload::CompleteGoal(_), .. }] => Some(
        self.authorizer.authorize(
            principal, &context.execution.task, AgentLifecycleAction::CompleteGoal,
        ).await?,
    ),
    [KernelCommand { payload: KernelCommandPayload::BlockGoal(_), .. }] => Some(
        self.authorizer.authorize(
            principal, &context.execution.task, AgentLifecycleAction::BlockGoal,
        ).await?,
    ),
    _ => None,
};
if policy.as_ref().is_some_and(|decision| !decision.allowed) {
    return Err(StoreError::AgentTransitionInvalid(
        "current policy denied goal transition".into(),
    ));
}
```

- [ ] **Step 3: Mint only command-bound built-in harness evidence**

During `commit_agent_command_transition`, when the source event is
`HarnessResult`, construct exactly one prospective `goal_evidence` record in
memory and include it in authority validation before evaluating an internal
goal command:

```rust
let evidence = EvidenceRecord {
    id: evidence_id.clone(),
    task_id: completion.task_id.clone(),
    evidence_type: "harness.invocation_result".into(),
    source: EvidenceSourceIdentity::BuiltinHarness {
        command_id: completion.command_id.clone(),
    },
    digest: canonical_sha256(&json!({
        "outcome": outcome,
        "fields": fields,
        "history": history,
    })).map_err(|error| StoreError::InvalidData(error.to_string()))?,
};
```

Require the source row to be a claimed `invoke_harness` command for the same
task. Do not insert the record until every transition, policy, goal, and
evidence check passes; then insert it in the same transaction as the source
result and lifecycle effects. Replaying the same completion finds the same
evidence id and digest; reuse with changed content is `IdempotencyConflict`.
Other event variants mint no evidence. There is no API that accepts a
caller-selected evidence type or source kind.

- [ ] **Step 4: Validate `CompleteGoal` before any mutation**

Require one allowed `AgentPolicyDecision` for `CompleteGoal`. Compare assertion
criterion ids as sets against the immutable `GoalContract.success` ids, while
also rejecting duplicate vector entries before set conversion. For each
assertion, select the evidence by both id and task id, match its type to the
criterion's `evidence`, and authorize its source through the completed source
command/digest check.

```rust
fn validate_complete_goal(
    goal: &GoalContract,
    command: &CompleteGoal,
    evidence: &BTreeMap<EvidenceId, AuthorizedEvidence>,
) -> Result<(), StoreError> {
    let expected = goal.success.iter()
        .map(|criterion| (criterion.id.as_str(), criterion.evidence.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeMap::new();
    let mut distinct_evidence = HashSet::new();
    for assertion in &command.assertions {
        if actual.insert(assertion.criterion_id.as_str(), &assertion.evidence_id).is_some() {
            return Err(StoreError::GoalEvidenceIncomplete);
        }
        if !distinct_evidence.insert(&assertion.evidence_id) {
            return Err(StoreError::GoalEvidenceIncomplete);
        }
    }
    if actual.len() != expected.len() || actual.keys().ne(expected.keys()) {
        return Err(StoreError::GoalEvidenceIncomplete);
    }
    for (criterion_id, evidence_id) in actual {
        let record = evidence.get(evidence_id).ok_or(StoreError::GoalEvidenceNotFound)?;
        if record.evidence_type != expected[criterion_id] {
            return Err(StoreError::GoalEvidenceTypeMismatch);
        }
        if !record.source_authorized {
            return Err(StoreError::GoalEvidenceUnauthorized);
        }
        if !record.digest_current {
            return Err(StoreError::GoalEvidenceStale);
        }
    }
    Ok(())
}
```

Only after all checks pass, insert assertions at goal revision + 1, mark the
`CompleteGoal` journal command completed, update goal to complete, task to
succeeded/finished, replace snapshot, increment both revisions once, append
`goal.completed`, and commit. A complete goal rejects every later runtime or
principal transition with `GoalAlreadyTerminal`/`AgentTaskAlreadyTerminal`.

- [ ] **Step 5: Validate `BlockGoal` and retain immutable history**

Require an allowed `BlockGoal` policy decision, active goal, running task,
`blocked_when_index < goal.blocked_when.len()`, reason code matching
`^[a-z][a-z0-9_]{0,63}$`, explanation 1-2,048 characters, 1-32 unique evidence
ids, and 1-32 unlock strings of 1-2,048 characters. Every evidence row must
belong to the task and pass source/digest authority, but its evidence type need
not equal a success criterion type.

Insert an immutable blocker at goal revision + 1, set
`goal_states.current_blocker_id`, phase blocked, operational status blocked,
complete the `BlockGoal` command, replace snapshot, increment both revisions,
append `goal.blocked`, and commit. Do not set task failure or success.

- [ ] **Step 6: Add the exhaustive evidence failure matrix**

For each case, capture a database fingerprint containing task/goal revisions,
phase, lifecycle, snapshot digest, source-command state/revision, evidence,
assertion, blocker, transition, command, policy-decision, and event counts.
Assert the exact error and identical fingerprint afterward:

| Case | Internal error |
|---|---|
| missing criterion | `GoalEvidenceIncomplete` |
| extra criterion | `GoalEvidenceIncomplete` |
| duplicate criterion | `GoalEvidenceIncomplete` |
| same evidence reused for multiple criteria | `GoalEvidenceIncomplete` |
| absent evidence id | `GoalEvidenceNotFound` |
| another task's evidence | `GoalEvidenceNotFound` |
| evidence type differs from criterion | `GoalEvidenceTypeMismatch` |
| source id names absent/non-harness command | `GoalEvidenceUnauthorized` |
| source command failed/cancelled | `GoalEvidenceStale` |
| source result digest changed | `GoalEvidenceStale` |
| old task revision | `TaskRevisionConflict` |
| old goal revision | `GoalRevisionConflict` |
| denied policy decision | `AgentTransitionInvalid` |
| policy revision changed before commit | `PolicyRevisionConflict` |
| already complete goal | `GoalAlreadyTerminal` |

```rust
#[tokio::test]
async fn every_invalid_completion_is_failure_atomic() {
    for case in GoalFixture::invalid_completion_cases().await {
        let before = case.fixture.database_fingerprint().await;
        let error = case.execute().await.unwrap_err();
        assert!(case.expected.matches(&error), "{}: {error}", case.name);
        assert_eq!(case.fixture.database_fingerprint().await, before, "{}", case.name);
    }
}
```

Add the blocker matrix: out-of-range index, malformed reason, blank/long
explanation, zero/33/duplicate/foreign/unauthorized/stale evidence ids,
zero/33/blank/long unlock conditions, stale task/goal/policy revisions, denied
policy, paused goal, and complete goal.

- [ ] **Step 7: Implement authenticated revisioned pause/resume/cancel**

All three request digests cover every field except idempotency key. Each method
locks session -> task -> goal, verifies the connection principal matches the
task owner, resolves action replay before revision checks, checks exact task
and goal revisions, inserts one action receipt/event, and commits atomically.

- Pause: only a `Running` task with active goal; require reason 1-2,048
  characters; increment both revisions; phase/status become paused; pending or
  claimed commands remain unchanged and undispatchable; advance the snapshot
  row's task revision without changing its payload or integrity fields.
- Resume paused: increment both revisions; phase active; status derives from
  existing pending/claimed commands; snapshot payload/digest is unchanged and
  its `task_revision` column advances.
- Resume blocked: before SQL, the kernel passes
  `RuntimeEvent::PrincipalResumed { event_id:
  "principal-resumed:<task-id>:<idempotency-key>" }` to the runtime. The
  runtime requires a blocked snapshot, clears the selected terminal outcome,
  resets its frame to `Ready`, and proposes the next deterministic command.
  The store commits that replacement transition, clears only
  `current_blocker_id`, retains blocker history, changes phase to active, and
  increments both revisions once.
- Cancel: accepted or running nonterminal task; cancel pending/claimed commands,
  increment task revision only, advance any snapshot row's task revision
  without changing its payload or integrity fields, set task
  cancelled/finished, preserve current goal phase/revision, and append
  `task.cancelled`. It never creates a blocker, assertion, or goal completion.

```rust
pub async fn resume_task(
    &self,
    principal: &PrincipalId,
    request: ResumeUserTask,
) -> Result<AgentExecutionView, StoreError> {
    let execution = self.store.get_agent_execution(
        request.session_id.clone(), request.task_id.clone(),
    ).await?;
    let transition = if execution.goal_phase == GoalPhase::Blocked {
        let seed = self.store.load_agent_start_seed(
            request.session_id.clone(), request.task_id.clone(),
        ).await?;
        Some(self.runtime.handle(RuntimeHandle {
            execution: &execution,
            definition: &seed.definition,
            event: &RuntimeEvent::PrincipalResumed {
                event_id: format!(
                    "principal-resumed:{}:{}",
                    request.task_id.as_uuid(), request.idempotency_key.as_str(),
                ),
            },
        }).map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?)
    } else { None };
    self.store.resume_user_task(
        principal, request, transition.as_ref(), &self.ceilings,
    ).await
}
```

- [ ] **Step 8: Prove lifecycle races and non-impersonation**

Add a table-driven state-machine test covering every legal edge
`active -> paused`, `paused -> active`, `active -> blocked`, `blocked ->
active`, and `active -> complete`, plus every other phase pair as an exact
rejection. Re-run every operation from `complete` and assert
`GoalAlreadyTerminal` with no revision/event change. Separately assert wait
statuses change no phase.

```rust
#[test]
fn goal_phase_edges_are_closed_and_complete_is_terminal() {
    let legal = BTreeSet::from([
        (GoalPhase::Active, GoalPhase::Paused),
        (GoalPhase::Paused, GoalPhase::Active),
        (GoalPhase::Active, GoalPhase::Blocked),
        (GoalPhase::Blocked, GoalPhase::Active),
        (GoalPhase::Active, GoalPhase::Complete),
    ]);
    for from in [GoalPhase::Active, GoalPhase::Paused, GoalPhase::Blocked, GoalPhase::Complete] {
        for to in [GoalPhase::Active, GoalPhase::Paused, GoalPhase::Blocked, GoalPhase::Complete] {
            assert_eq!(goal_edge_allowed(from, to), legal.contains(&(from, to)));
        }
    }
}
```

Race pause against command completion, resume against cancel, two different
pause keys, completion against cancel, block against complete, and session
close against each action. Exactly one CAS wins; the loser gets a revision or
terminal conflict; no deadlock occurs.

Add a repository-shape assertion that `KernelCommandPayload` contains no
Pause/Resume/Cancel variant and `RuntimeEvent::PrincipalResumed` can be created
only in `src/agent/kernel.rs` from the authenticated resume path.

```rust
#[test]
fn runtime_cannot_impersonate_principal_lifecycle_authority() {
    let types = std::fs::read_to_string("src/agent/types.rs").unwrap();
    let command_block = types.split("pub enum KernelCommandPayload").nth(1).unwrap()
        .split('}').next().unwrap();
    for forbidden in ["Pause", "Resume", "Cancel"] {
        assert!(!command_block.contains(forbidden));
    }
}
```

- [ ] **Step 9: Run and commit goal and principal authority**

Run:

```bash
cargo test --test agent_goal_authority --test agent_transition_store --test agent_lifecycle_store
cargo test --test kite_agent_runtime --test e2e_agent_restart --test postgres_lock_order
git diff --check
```

Expected: PASS; every invalid evidence/phase/action request is failure-atomic.

```bash
git add src/agent/kernel.rs src/agent/kite.rs src/agent/mod.rs src/agent/runtime.rs src/agent/types.rs src/goal.rs src/lib.rs src/task.rs src/store/agent.rs src/store/models.rs tests/agent_goal_authority.rs tests/agent_lifecycle_store.rs tests/agent_transition_store.rs tests/e2e_agent_restart.rs tests/kite_agent_runtime.rs tests/postgres_lock_order.rs
git commit -m "feat: enforce agent goal lifecycle authority"
```

### Task 7: Bridge persisted `InvokeHarness` to the current direct loop

**Files:**

- Create: `src/engine/harness.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/agent/kernel.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/lib.rs`
- Create: `tests/agent_direct_bridge.rs`
- Modify: `tests/agent_goal_authority.rs`
- Modify: `tests/agent_transition_store.rs`
- Modify: `tests/sequential_runtime.rs`
- Modify: `tests/structured_runtime.rs`

**Interfaces:**

- Consumes: Task 6 authorized command completion and built-in harness evidence;
  Task 5 list/claim/complete/fail APIs; Task 4 complete provider-free
  `InvokeHarness`; existing `ModelClient`, `ModelRequest`, `ModelResponse`,
  `ModelError`, and legacy completion validation.
- Produces the extractable internal seam Slice 3 replaces:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct HarnessExecutionResult {
    pub outcome: String,
    pub fields: serde_json::Value,
    pub history: Vec<RuntimeHistoryItem>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum HarnessExecutionError {
    #[error("harness command is unavailable: {0}")]
    Unavailable(&'static str),
    #[error("harness result is invalid: {0}")]
    InvalidResult(String),
    #[error("harness failed: {0}")]
    Failed(String),
}

#[async_trait::async_trait]
pub trait HarnessCommandExecutor: Send + Sync {
    async fn execute(
        &self,
        invocation: &InvokeHarness,
    ) -> Result<HarnessExecutionResult, HarnessExecutionError>;
}

pub struct DirectLoopHarnessExecutor { model: Arc<dyn ModelClient> }

impl DirectLoopHarnessExecutor {
    pub fn new(model: Arc<dyn ModelClient>) -> Self;
}

#[async_trait::async_trait]
impl HarnessCommandExecutor for DirectLoopHarnessExecutor {
    async fn execute(
        &self,
        invocation: &InvokeHarness,
    ) -> Result<HarnessExecutionResult, HarnessExecutionError>;
}

impl AgentKernel {
    pub(crate) fn store(&self) -> &PostgresStore;

    pub fn with_harness_executor(
        self: Arc<Self>,
        executor: Arc<dyn HarnessCommandExecutor>,
    ) -> AgentCommandDispatcher;
}

pub struct AgentCommandDispatcher {
    kernel: Arc<AgentKernel>,
    executor: Arc<dyn HarnessCommandExecutor>,
}

impl AgentCommandDispatcher {
    pub async fn tick_agent_commands(&self, limit: i64) -> Result<usize, StoreError>;
}

fn harness_tool_json(tool: &HarnessTool) -> serde_json::Value;

// Test-local support in tests/agent_direct_bridge.rs.
struct BridgeFixture {
    dispatcher: AgentCommandDispatcher,
    model: Arc<ScriptedModel>,
    pool: PgPool,
}

impl BridgeFixture {
    async fn started() -> Option<Self>;
    async fn started_with_declared_tool() -> Option<Self>;
    async fn pending_command_id(&self) -> uuid::Uuid;
    async fn current(&self) -> AgentExecutionView;
    async fn evidence(&self) -> Vec<EvidenceRecord>;
    async fn legacy_run_count(&self) -> i64;
    async fn legacy_frame_count(&self) -> i64;
}

struct ScriptedModel;

impl ScriptedModel {
    fn on_request<F, Fut>(&self, handler: F)
    where
        F: Fn(ModelRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ModelResponse, ModelError>> + Send + 'static;
    fn complete(&self, outcome: &str, fields: serde_json::Value);
    fn respond_with(&self, item: serde_json::Value);
}
```

The trait has no plugin identity, registration, capability description,
transport, protocol version, common envelope, provider name, or model choice.
Those belong to Slice 3.

- [ ] **Step 1: Add failing persist-before-model and result tests**

```rust
// tests/agent_direct_bridge.rs
#[tokio::test]
async fn invocation_is_persisted_and_claimed_before_model_call() {
    let Some(fixture) = BridgeFixture::started().await else { return };
    fixture.model.on_request({
        let pool = fixture.pool.clone();
        let command_id = fixture.pending_command_id().await;
        move |_| {
            let pool = pool.clone();
            async move {
                let state: String = sqlx::query_scalar(
                    "SELECT state FROM agent_commands WHERE id = $1",
                ).bind(command_id).fetch_one(&pool).await.unwrap();
                assert_eq!(state, "claimed");
                Ok(ModelResponse { items: vec![json!({
                    "type":"function_call",
                    "call_id":"done-1",
                    "name":"complete_task",
                    "arguments":{"outcome":"ok","fields":{"answer":"done"}}
                })] })
            }
        }
    });
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    assert_eq!(fixture.current().await.lifecycle, TaskLifecycle::Succeeded);
}

#[tokio::test]
async fn harness_success_mints_evidence_but_does_not_complete_goal() {
    let Some(fixture) = BridgeFixture::started().await else { return };
    fixture.model.complete("ok", json!({"answer":"done"}));
    fixture.dispatcher.tick_agent_commands(8).await.unwrap();
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Succeeded);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    let evidence = fixture.evidence().await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].evidence_type, "harness.invocation_result");
    assert!(matches!(evidence[0].source, EvidenceSourceIdentity::BuiltinHarness { .. }));
}
```

- [ ] **Step 2: Run and observe the absent executor seam**

Run:

```bash
cargo test --test agent_direct_bridge invocation_is_persisted -- --nocapture
cargo test --test agent_direct_bridge harness_success_mints -- --nocapture
```

Expected: FAIL to compile because `HarnessCommandExecutor` and the dispatcher
do not exist.

- [ ] **Step 3: Extract shared completion request/result helpers**

Move, without semantic changes, the existing completion-tool JSON,
`complete_task` item parser, and exact outcome/field type validation from
`src/engine/mod.rs` into `src/engine/harness.rs` as crate-private helpers.
Both the legacy `Engine` and `DirectLoopHarnessExecutor` call them. Keep legacy
function names through imports so existing tests retain byte-identical request
items.

```rust
pub(crate) fn completion_tool_from_schema(result_schema: &ClosedSchema) -> Value {
    json!({
        "type": "function",
        "name": "complete_task",
        "description": "Complete the current task with one declared outcome.",
        "parameters": result_schema,
    })
}

pub(crate) fn parse_completion_item(
    item: &Value,
) -> Option<(String, Value)> {
    if item.get("type")?.as_str()? != "function_call"
        || item.get("name")?.as_str()? != "complete_task"
    {
        return None;
    }
    let arguments = item.get("arguments")?;
    let arguments = arguments.as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| arguments.clone());
    Some((
        arguments.get("outcome")?.as_str()?.to_owned(),
        arguments.get("fields")?.clone(),
    ))
}
```

First run the existing sequential and structured tests after extraction. Do
not update their expected model requests or persisted histories.

- [ ] **Step 4: Execute one bounded direct-loop completion**

Build `ModelRequest` with instructions from prompt; input containing semantic
role, goal projection, typed input, and projected history; and tools containing
the completion tool followed by frozen harness tool descriptors. Enforce
`model_turns_remaining` and `active_execution_millis_remaining` before calling
the model, then enforce `response_item_bytes` and `tool_calls_per_response`
before accepting output.

```rust
if invocation.limits.model_turns_remaining == 0
    || invocation.limits.active_execution_millis_remaining == 0
{
    return Err(HarnessExecutionError::Failed(
        "active_execution_limit_exceeded".into(),
    ));
}
let request = ModelRequest {
    instructions: invocation.prompt.clone(),
    input: std::iter::once(json!({
        "role": invocation.role,
        "goal": invocation.goal,
        "input": invocation.input,
    })).chain(
        invocation.projected_history.iter().map(|item| item.payload.clone())
    ).collect(),
    tools: std::iter::once(completion_tool_from_schema(&invocation.result_schema))
        .chain(invocation.tools.iter().map(harness_tool_json))
        .collect(),
};
let response = tokio::time::timeout(
    Duration::from_millis(invocation.limits.active_execution_millis_remaining),
    self.model.respond(request),
).await.map_err(|_| HarnessExecutionError::Failed(
    "active_execution_limit_exceeded".into(),
))?.map_err(|error| HarnessExecutionError::Failed(error.to_string()))?;
```

Require exactly one actionable function call. A valid `complete_task` outcome
is checked against `result_schema` and returned. Any non-completion tool call
returns `HarnessExecutionError::Unavailable("model_tool_calls")`; the bridge
does not invoke a tool, write a legacy frame, or invent a result. Zero or
multiple actions, undeclared outcome, wrong fields, or oversized output returns
`InvalidResult`.

- [ ] **Step 5: Claim, execute, and complete journaled commands**

`tick_agent_commands` lists at most `limit`, loads each context, and skips tasks
whose goal is paused/blocked or lifecycle is terminal. It claims a pending row
with claimant `builtin-direct-v1` and idempotency key
`builtin-direct-v1:<command-uuid>`. A recovered claimed row with that exact
claimant is resumed directly at its stored revision; it is not claimed again.
A claimed row owned by another claimant is skipped because Slice 2 has no
lease, reassignment, or fencing authority. For `InvokeHarness`, the dispatcher
executes, allocates an evidence UUID v7 in the kernel, and calls
`complete_command` with event id
`builtin-harness-result:<command-uuid>:<command-revision>`.

```rust
let claimed = match listed.state {
    AgentCommandState::Pending => self.kernel.store().claim_agent_command(
        ClaimAgentCommand {
            session_id: context.execution.task.session_id.clone(),
            task_id: listed.task_id.clone(),
            command_id: listed.id.clone(),
            expected_command_revision: listed.revision,
            claimant: "builtin-direct-v1".into(),
            idempotency_key: IdempotencyKey::new(format!(
                "builtin-direct-v1:{}", listed.id.as_uuid(),
            )).map_err(|error| StoreError::InvalidData(error.to_string()))?,
        },
    ).await?,
    AgentCommandState::Claimed
        if listed.claimant.as_deref() == Some("builtin-direct-v1") => listed,
    AgentCommandState::Claimed => continue,
    AgentCommandState::Completed
    | AgentCommandState::Failed
    | AgentCommandState::Cancelled => continue,
};

match &claimed.payload {
    KernelCommandPayload::InvokeHarness(invocation) => {
        match self.executor.execute(invocation).await {
            Ok(result) => {
                let evidence_id = EvidenceId::new(uuid::Uuid::now_v7());
                self.kernel.complete_command(
                    &context.execution.task.principal,
                    CompleteAgentCommand {
                        session_id: context.execution.task.session_id.clone(),
                        task_id: context.execution.task.id.clone(),
                        command_id: claimed.id.clone(),
                        expected_command_revision: claimed.revision,
                        idempotency_key: IdempotencyKey::new(format!(
                            "builtin-result:{}:{}",
                            claimed.id.as_uuid(), claimed.revision,
                        )).map_err(|error| StoreError::InvalidData(error.to_string()))?,
                        event: RuntimeEvent::HarnessResult {
                            event_id: format!(
                                "builtin-harness-result:{}:{}",
                                claimed.id.as_uuid(), claimed.revision,
                            ),
                            command_key: claimed.key.clone(),
                            outcome: result.outcome,
                            fields: result.fields,
                            evidence_id,
                            history: result.history,
                        },
                    },
                ).await?;
            }
            Err(HarnessExecutionError::Unavailable(_)) => {
                self.kernel.store().fail_unavailable_agent_command(
                    &context.execution.task.principal,
                    &FailAgentCommand {
                        session_id: context.execution.task.session_id.clone(),
                        task_id: claimed.task_id.clone(),
                        command_id: claimed.id.clone(),
                        expected_command_revision: claimed.revision,
                        idempotency_key: IdempotencyKey::new(format!(
                            "builtin-unavailable:{}:{}",
                            claimed.id.as_uuid(), claimed.revision,
                        )).map_err(|error| StoreError::InvalidData(error.to_string()))?,
                        code: "runtime_command_unavailable".into(),
                    },
                ).await?;
            }
            Err(HarnessExecutionError::InvalidResult(_) | HarnessExecutionError::Failed(_)) => {
                self.kernel.fail_command(
                    &context.execution.task.principal,
                    FailAgentCommand {
                        session_id: context.execution.task.session_id.clone(),
                        task_id: claimed.task_id.clone(),
                        command_id: claimed.id.clone(),
                        expected_command_revision: claimed.revision,
                        idempotency_key: IdempotencyKey::new(format!(
                            "builtin-failure:{}:{}", claimed.id.as_uuid(), claimed.revision,
                        )).map_err(|error| StoreError::InvalidData(error.to_string()))?,
                        code: "harness_failed".into(),
                    },
                ).await?;
            }
        }
    }
    KernelCommandPayload::RequestContext(_)
    | KernelCommandPayload::WaitForEvent(_)
    | KernelCommandPayload::RequestApproval(_) => {
        self.kernel.store().fail_unavailable_agent_command(
            &context.execution.task.principal,
            &FailAgentCommand {
                session_id: context.execution.task.session_id.clone(),
                task_id: claimed.task_id.clone(),
                command_id: claimed.id.clone(),
                expected_command_revision: claimed.revision,
                idempotency_key: IdempotencyKey::new(format!(
                    "builtin-unavailable:{}:{}",
                    claimed.id.as_uuid(), claimed.revision,
                )).map_err(|error| StoreError::InvalidData(error.to_string()))?,
                code: "runtime_command_unavailable".into(),
            },
        ).await?;
    }
    KernelCommandPayload::CompleteGoal(_)
    | KernelCommandPayload::BlockGoal(_)
    | KernelCommandPayload::FailTask(_) => {
        return Err(StoreError::AgentTransitionInvalid(
            "internal authority command reached dispatcher".into(),
        ));
    }
}
```

Internal authority commands are completed in the transition transaction and
never appear in `dispatchable_agent_commands`.

- [ ] **Step 6: Prove unavailable and definitive failure semantics**

Add tests for RequestContext, WaitForEvent, RequestApproval, a model tool call,
timeout, model failure, invalid completion, stale claim, duplicate dispatcher
tick, paused/blocked task, task cancelled during model execution, and restart
between claim and result. Assert:

- unsupported behavior yields task `Failed`, operational `Finished`, goal
  unchanged, command `Failed`, and stable `RuntimeCommandUnavailable` event;
- model/validation failure yields `FailTask`/`harness_failed`, not blocked or
  complete;
- late result after cancel is retained only as command failure metadata and
  cannot advance task, goal, snapshot, or evidence;
- duplicate tick never calls the model twice for a completed command;
- restart redelivers a claimed command with the same claimant/key and accepts
  one idempotent result.

```rust
#[tokio::test]
async fn model_tool_call_fails_closed_without_legacy_frame_or_goal_change() {
    let Some(fixture) = BridgeFixture::started_with_declared_tool().await else { return };
    fixture.model.respond_with(json!({
        "type":"function_call", "call_id":"tool-1",
        "name":"github/context", "arguments":{}
    }));
    fixture.dispatcher.tick_agent_commands(8).await.unwrap();
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Failed);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.legacy_run_count().await, 0);
    assert_eq!(fixture.legacy_frame_count().await, 0);
}
```

- [ ] **Step 7: Run and commit the direct-loop bridge**

Run:

```bash
cargo test --test agent_direct_bridge --test agent_transition_store --test agent_goal_authority
cargo test --test sequential_runtime --test structured_runtime --test recovery
git diff --check
```

Expected: PASS; existing direct workflow-loop requests, results, histories, and
recovery behavior remain unchanged.

```bash
git add src/engine/harness.rs src/engine/mod.rs src/agent/kernel.rs src/agent/mod.rs src/lib.rs tests/agent_direct_bridge.rs tests/agent_goal_authority.rs tests/agent_transition_store.rs tests/sequential_runtime.rs tests/structured_runtime.rs
git commit -m "feat: bridge durable agent harness commands"
```

### Task 8: Expose the private lifecycle/read model and close compatibility gates

**Files:**

- Modify: `src/agent/kernel.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/protocol/application.rs`
- Modify: `src/protocol/transport.rs`
- Modify: `src/protocol/types.rs`
- Modify: `src/protocol/v1.rs`
- Modify: `src/protocol/v2.rs`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Create: `tests/agent_protocol.rs`
- Modify: `tests/acp_conformance.rs`
- Modify: `tests/e2e_restart.rs`
- Modify: `tests/execution_store.rs`
- Modify: `tests/repository_shape.rs`
- Modify: `tests/responses_conformance.rs`
- Modify: `tests/transports.rs`
- Modify: `README.md`
- Modify: `docs/reference/agent-runtime-and-goal-lifecycle.md`
- Modify: `docs/reference/kite-language.md`
- Modify: `docs/reference/user-task-and-goal-contract.md`
- Modify: `docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md`
- Modify: `examples/babysit_pr.kite`

**Interfaces:**

- Consumes: Task 7 operational `AgentKernel` and dispatcher; Task 6 authenticated
  start/pause/resume/cancel; Task 5 ordered agent event cursor; Slice 1 private
  task submit/get and definition publication; all existing application and
  transport bindings.
- Produces these private extensions without claiming the Slice 3 plugin
  protocol:

```rust
pub struct GetAgentExecution {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
}

pub struct AgentEventCursor {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub after: i64,
    pub limit: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetAgentExecutionDto {
    session_id: SessionId,
    task_id: UserTaskId,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentEventCursorDto {
    session_id: SessionId,
    task_id: UserTaskId,
    after: i64,
    limit: u32,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

// ApplicationCommand additions
StartAgent(StartAgent),
GetAgentExecution(GetAgentExecution),
PauseTask(PauseUserTask),
ResumeTask(ResumeUserTask),
CancelTask(CancelUserTask),
ReplayAgentEvents(AgentEventCursor),

// ApplicationResult additions
AgentExecution(AgentExecutionView),
AgentEvents(Vec<AgentTaskEvent>),

// Test-local support in tests/agent_protocol.rs.
struct AgentTransportFixture;

impl AgentTransportFixture {
    async fn new(kind: TransportKind) -> Self;
    async fn call(&self, method: &str, params: serde_json::Value)
        -> serde_json::Value;
    fn foreign_connection(&self) -> AgentTransportConnection;
    fn start_payload(&self) -> serde_json::Value;
    fn session_id(&self) -> SessionId;
    fn task_id(&self) -> UserTaskId;
    async fn count(&self, table: &str) -> i64;
}

struct AgentTransportConnection;

impl AgentTransportConnection {
    async fn call(&self, method: &str, params: serde_json::Value)
        -> serde_json::Value;
}

fn command_tag(command: &ApplicationCommand) -> &'static str;
fn blocked_execution_fixture() -> AgentExecutionView;
struct EncodedRpcError {
    code: i64,
    message: String,
    data: Option<serde_json::Value>,
}
fn encode_on_every_adapter(error: ProtocolError) -> Vec<EncodedRpcError>;

impl Application {
    pub fn agent_kernel(&self) -> Arc<AgentKernel>;
}

async fn run_scheduler(
    engine: Arc<Engine>,
    agent_dispatcher: Arc<AgentCommandDispatcher>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), EngineError>;
```

Wire methods are exactly:

```text
_kiteframe/agent/start
_kiteframe/agent/get
_kiteframe/task/pause
_kiteframe/task/resume
_kiteframe/task/cancel
_kiteframe/task/events
```

Slice 1 `_kiteframe/task/submit` and `_kiteframe/task/get` keep their existing
request and response bytes. `_kiteframe/agent/get` is the new mutable lifecycle
read model.

- [ ] **Step 1: Add failing v1/v2 decode and encoding tests**

```rust
// tests/agent_protocol.rs
const SESSION: &str = "018f0f47-5d8b-7d5a-9cb1-3a09d0d4c902";
const TASK: &str = "018f0f47-5d8b-7d5a-9cb1-3a09d0d4c903";

#[test]
fn every_agent_lifecycle_method_decodes_identically_in_v1_and_v2() {
    let cases = [
        ("_kiteframe/agent/start", json!({
            "sessionId": SESSION, "taskId": TASK,
            "expectedTaskRevision": 0, "expectedGoalRevision": 0,
            "idempotencyKey": "start-agent"
        })),
        ("_kiteframe/agent/get", json!({
            "sessionId": SESSION, "taskId": TASK
        })),
        ("_kiteframe/task/pause", json!({
            "sessionId": SESSION, "taskId": TASK,
            "expectedTaskRevision": 1, "expectedGoalRevision": 0,
            "reason": "User paused work.", "idempotencyKey": "pause-agent"
        })),
        ("_kiteframe/task/resume", json!({
            "sessionId": SESSION, "taskId": TASK,
            "expectedTaskRevision": 2, "expectedGoalRevision": 1,
            "idempotencyKey": "resume-agent"
        })),
        ("_kiteframe/task/cancel", json!({
            "sessionId": SESSION, "taskId": TASK,
            "expectedTaskRevision": 3, "expectedGoalRevision": 2,
            "reason": "No longer needed.", "idempotencyKey": "cancel-agent"
        })),
        ("_kiteframe/task/events", json!({
            "sessionId": SESSION, "taskId": TASK, "after": 0, "limit": 32
        })),
    ];
    for (method, params) in cases {
        let v1 = V1Adapter::decode_extension(method, params.clone()).unwrap();
        let v2 = V2Adapter::new(true).decode_extension(method, params).unwrap();
        assert_eq!(command_tag(&v1), command_tag(&v2));
    }
}

#[test]
fn agent_read_model_keeps_all_three_state_axes_distinct() {
    let encoded = encode_application_result(ApplicationResult::AgentExecution(
        blocked_execution_fixture(),
    )).unwrap();
    assert_eq!(encoded["lifecycle"], "running");
    assert_eq!(encoded["operationalStatus"], "blocked");
    assert_eq!(encoded["goal"]["phase"], "blocked");
    assert_eq!(encoded["goal"]["blockedWhenIndex"], 0);
    assert_eq!(encoded["runtime"]["runtimeId"], "kite.v1");
    assert!(encoded["runtime"].get("payload").is_none());
}
```

- [ ] **Step 2: Run and observe unsupported methods**

Run:

```bash
cargo test --all-features --test agent_protocol every_agent_lifecycle -- --nocapture
cargo test --all-features --test agent_protocol agent_read_model -- --nocapture
```

Expected: FAIL with unsupported methods and absent `ApplicationResult`
variants.

- [ ] **Step 3: Decode closed DTOs and bind every command to the connection**

Deserialize the production request types directly for start/pause/resume/cancel.
Use closed camel-case DTOs for get and events, with optional `_meta` matching
the existing extension convention. Reject unknown fields and a wire principal.
Add every command's `session_id` to `command_session` so the existing
authenticated connection check runs before dispatch.

```rust
match method {
    "_kiteframe/agent/start" => decode::<StartAgent>(params)
        .map(ApplicationCommand::StartAgent),
    "_kiteframe/agent/get" => decode::<GetAgentExecutionDto>(params)
        .map(|dto| ApplicationCommand::GetAgentExecution(GetAgentExecution {
            session_id: dto.session_id,
            task_id: dto.task_id,
        })),
    "_kiteframe/task/pause" => decode::<PauseUserTask>(params)
        .map(ApplicationCommand::PauseTask),
    "_kiteframe/task/resume" => decode::<ResumeUserTask>(params)
        .map(ApplicationCommand::ResumeTask),
    "_kiteframe/task/cancel" => decode::<CancelUserTask>(params)
        .map(ApplicationCommand::CancelTask),
    "_kiteframe/task/events" => decode::<AgentEventCursorDto>(params)
        .map(|dto| ApplicationCommand::ReplayAgentEvents(AgentEventCursor {
            session_id: dto.session_id,
            task_id: dto.task_id,
            after: dto.after,
            limit: dto.limit,
        })),
    _ => decode_existing_extension(method, params),
}
```

- [ ] **Step 4: Dispatch only with the authenticated principal**

Construct the default built-in `AgentKernel` inside `Application::new` from
the existing store, a `KiteAgentRuntime` created with the store's configured
ceilings, and `TaskOwnerAuthorizer`. Add `with_agent_kernel` for tests and `agent_kernel` for the process
scheduler; both return/share the same `Arc`, never a second authority object.
Dispatch start and all principal actions with
`context.connection_principal`; get/events remain session scoped.

```rust
ApplicationCommand::StartAgent(request) => self.agent_kernel
    .start_agent(&context.connection_principal, request)
    .await.map(ApplicationResult::AgentExecution).map_err(ProtocolError::from),
ApplicationCommand::GetAgentExecution(request) => self.store
    .get_agent_execution(request.session_id, request.task_id)
    .await.map(ApplicationResult::AgentExecution).map_err(ProtocolError::from),
ApplicationCommand::PauseTask(request) => self.agent_kernel
    .pause_task(&context.connection_principal, request)
    .await.map(ApplicationResult::AgentExecution).map_err(ProtocolError::from),
ApplicationCommand::ResumeTask(request) => self.agent_kernel
    .resume_task(&context.connection_principal, request)
    .await.map(ApplicationResult::AgentExecution).map_err(ProtocolError::from),
ApplicationCommand::CancelTask(request) => self.agent_kernel
    .cancel_task(&context.connection_principal, request)
    .await.map(ApplicationResult::AgentExecution).map_err(ProtocolError::from),
ApplicationCommand::ReplayAgentEvents(cursor) => {
    let after = u64::try_from(cursor.after).map_err(|_| {
        ProtocolError::InvalidPayload("agent event cursor must not be negative".into())
    })?;
    self.store.agent_task_events_after(
        cursor.session_id, cursor.task_id, after, cursor.limit,
    ).await.map(ApplicationResult::AgentEvents).map_err(ProtocolError::from)
}
```

In `src/main.rs`, create one `DirectLoopHarnessExecutor` from the existing
model client and the application's shared kernel. Pass the resulting dispatcher
through both stdio and remote runners into `run_scheduler`. Keep the current
batch size, 50 ms idle delay, cancellation on shutdown, and warning/continue
behavior. Do not create a second model client or tool resolver.

```rust
let application = Application::new(store.clone(), engine.clone())
    .with_mcp_manager(mcp);
let agent_recovery = application.agent_kernel()
    .recover(engine.config.scheduler_batch_size)
    .await?;
tracing::info!(
    pending = agent_recovery.pending,
    claimed = agent_recovery.claimed,
    "agent command recovery complete",
);
let agent_dispatcher = Arc::new(
    application.agent_kernel().with_harness_executor(Arc::new(
        DirectLoopHarnessExecutor::new(model.clone()),
    )),
);
let application = Arc::new(application);

async fn run_scheduler(
    engine: Arc<Engine>,
    agent_dispatcher: Arc<AgentCommandDispatcher>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), EngineError> {
    loop {
        if *shutdown.borrow() { return Ok(()); }
        let batch_size = engine.config.scheduler_batch_size;
        let applied = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                continue;
            }
            result = async {
                let legacy = engine.tick_until_idle().await?;
                let agents = agent_dispatcher.tick_agent_commands(batch_size).await?;
                Ok::<usize, EngineError>(legacy + agents)
            } => match result {
                Ok(applied) => applied,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "scheduler tick failed; retaining service availability",
                    );
                    0
                }
            },
        };
        if applied == 0 {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }
}
```

- [ ] **Step 5: Encode a bounded read model and ordered events**

The lifecycle result includes the immutable Slice 1 accepted-task fields plus
three distinct axes and revisions. Runtime output exposes envelope identity,
disposition, and digest, but not opaque payload/history/prompts. Evidence ids,
source identities, assertion ids, and blocker evidence remain behind the
event/read authorization boundary; blocker output exposes condition index,
reason code, explanation, and unlock conditions.

```json
{
  "id": "task-uuid",
  "sessionId": "session-uuid",
  "principal": "user_456",
  "agent": {
    "id": "definition-uuid",
    "kind": "agent",
    "name": "babysit_pr",
    "version": 1,
    "definitionHash": "sha256"
  },
  "target": {"type":"github_pull_request","repository":"acme/payments","number":142},
  "preferences": {"harnessRole":"coding"},
  "lifecycle": "running",
  "operationalStatus": "waiting_for_harness",
  "taskRevision": 1,
  "goal": {
    "contract": {
      "version": 1,
      "objective": "Address review feedback safely.",
      "success": [{
        "id": "review_verified",
        "statement": "The bounded review check succeeded.",
        "evidence": "harness.invocation_result"
      }],
      "constraints": ["Do not mutate repository state."],
      "blockedWhen": ["Required review information is unavailable."]
    },
    "contractDigest": "sha256",
    "phase": "active",
    "revision": 0,
    "blocker": null
  },
  "runtime": {
    "schemaVersion": 1,
    "runtimeId": "kite.v1",
    "disposition": "running",
    "snapshotDigest": "sha256"
  },
  "createdAt": "2026-08-18T00:00:00Z"
}
```

Events encode sequence, kind, already-redacted metadata, task revision, goal
revision, and creation time. Enforce event limit 1-1,000. A negative cursor or
zero/over-limit count returns `Invalid params`.

- [ ] **Step 6: Map stable failures to fixed coarse transport shapes**

Use Slice 1's `ProtocolError::StableApplication`. Extend its mapper with the
table in **Stable internal and external failures**, including
`GoalEvidenceStale`. ACP SDK, NDJSON, and WebSocket must all encode:

```json
{
  "code": -32000,
  "message": "application request failed",
  "data": {"kiteframe_code": "goal_evidence_invalid"}
}
```

Invalid JSON shape remains `-32602`/`Invalid params`. Session mismatch and
principal mismatch remain fixed application failures. Logs contain method,
coarse code, task revision when known, and no task content.

```rust
#[test]
fn lifecycle_errors_have_transport_independent_coarse_codes() {
    for (error, code) in [
        (StoreError::TaskRevisionConflict, "revision_conflict"),
        (StoreError::GoalRevisionConflict, "revision_conflict"),
        (StoreError::GoalEvidenceIncomplete, "goal_evidence_invalid"),
        (StoreError::GoalEvidenceStale, "goal_evidence_invalid"),
        (StoreError::RuntimeCommandUnavailable, "runtime_command_unavailable"),
    ] {
        for encoded in encode_on_every_adapter(ProtocolError::from(error)) {
            assert_eq!(encoded.code, -32000);
            assert_eq!(encoded.message, "application request failed");
            assert_eq!(encoded.data.unwrap()["kiteframe_code"], code);
        }
    }
}
```

- [ ] **Step 7: Add remote parity, isolation, idempotency, and redaction tests**

For ACP v1/v2 decode, NDJSON, and WebSocket, test start/get/pause/resume/cancel
and event replay success; identical replay; changed-content replay; stale task
and goal revisions; another session for the same principal; another principal;
closed session; missing task; negative cursor; and unknown fields. Assert
identical result/error shapes on every adapter.

```rust
#[tokio::test]
async fn remote_start_replay_and_actions_are_session_and_principal_scoped() {
    for kind in [TransportKind::Stdio, TransportKind::WebSocket] {
        let fixture = AgentTransportFixture::new(kind).await;
        let start = fixture.call("_kiteframe/agent/start", fixture.start_payload()).await;
        let replay = fixture.call("_kiteframe/agent/start", fixture.start_payload()).await;
        assert_eq!(start["result"]["id"], replay["result"]["id"]);
        assert_eq!(fixture.count("agent_runtime_snapshots").await, 1);

        let foreign = fixture.foreign_connection().call(
            "_kiteframe/agent/get",
            json!({"sessionId": fixture.session_id(), "taskId": fixture.task_id()}),
        ).await;
        assert_eq!(foreign["error"]["message"], "application request failed");
    }
}
```

Capture logs for invalid start, evidence failure, pause, and cancel. Assert they
exclude objective, constraints, blocked text, prompt, model output, repository,
evidence UUID/digest, reason text, and idempotency key.

- [ ] **Step 8: Prove all legacy behavior and repository boundaries**

Run existing workflow tests before changing an expected fixture. Add explicit
assertions that:

- the support workflow source compiles to the exact version-1 JSON/hash;
- workflow publish/start/get/events are byte-identical;
- `StartRun` still rejects agents with `agent_runtime_unavailable` before
  legacy run/frame/event/tool-binding rows;
- agent start creates no legacy run, frame, model item, tool call, question, or
  permission row;
- legacy run completion cannot alter `goal_states`;
- agent completion cannot alter legacy runs;
- session close retains task/goal/run/frame lock order;
- source has no plugin registry, common envelope, external runtime transport,
  ACP harness adapter, connector, event binding, work-item lease/fence,
  profile, budget, secret, editor, or `babysit_pr` loop module.

```rust
#[test]
fn slice_two_does_not_pull_later_slices_into_the_repository() {
    let root = std::env::current_dir().unwrap();
    for absent in [
        "src/plugin", "src/connector", "src/channel", "src/work_item",
        "src/budget", "src/profile", "clients/editor", "python/connectors",
    ] {
        assert!(!root.join(absent).exists(), "later-slice path exists: {absent}");
    }
    let source = std::fs::read_to_string("src/agent/types.rs").unwrap();
    for forbidden in [
        "provider_id", "model_name", "credential", "endpoint",
        "protocol_version", "capability_negotiation", "outcome_unknown",
    ] {
        assert!(!source.contains(forbidden), "later-slice field: {forbidden}");
    }
    for path in [
        "examples/babysit_pr.kite",
        "tests/fixtures/dsl/agent_babysit_pr.kite",
    ] {
        let kite = std::fs::read_to_string(path).unwrap();
        for forbidden in [
            "provider_id", "model_name", "credential", "endpoint",
            "subscription_id", "runtime_id",
        ] {
            assert!(!kite.contains(forbidden), "runtime choice in {path}: {forbidden}");
        }
    }
}
```

Do not treat the user's untracked `fuzz/` or `site/` directories as Slice 2
implementation paths and do not modify their contents.

- [ ] **Step 9: Publish truthful Slice 2 documentation**

Update the example to use the reviewed explicit goal statements. Document:

- implemented now: immutable exact agent start, pure built-in runtime,
  revisioned task/goal/snapshot transitions, command journal, minimal harness
  evidence, complete/block authority, pause/resume/cancel, direct-loop bridge,
  restart recovery, and private lifecycle methods;
- prerequisite already implemented by Slice 1: authoring/publication and
  immutable accepted intent;
- not implemented: every explicit Slice 3+ exclusion in Global Constraints;
- root `done`, harness success, task success, task failure, cancellation, and
  goal completion are distinct;
- only `harness.invocation_result` evidence can be minted in Slice 2, so the
  GitHub-typed `babysit_pr` goal cannot complete until context/connector slices
  provide authorized GitHub evidence.

Do not change the design spec's normative scope or claim the product vertical
slice works. Its non-normative implementation-status evidence is updated only
after the Task 8 implementation commit exists.

- [ ] **Step 10: Run the complete Slice 2 gate**

Run from repository root with a disposable configured PostgreSQL database:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
(cd clients/python && uv run ruff check . && uv run pyright && uv run pytest)
(cd clients/typescript && bun run typecheck && bun test && bun run build)
git diff --check
git status --short
git diff --stat
```

Expected: every command passes. Only listed Slice 2 paths are staged or
modified; unrelated `.worktrees/`, `fuzz/`, and `site/` content is untouched.

- [ ] **Step 11: Commit the protocol and evidence boundary**

```bash
git add src/agent/kernel.rs src/agent/mod.rs src/protocol/application.rs src/protocol/transport.rs src/protocol/types.rs src/protocol/v1.rs src/protocol/v2.rs src/lib.rs src/main.rs tests/agent_protocol.rs tests/acp_conformance.rs tests/e2e_restart.rs tests/execution_store.rs tests/repository_shape.rs tests/responses_conformance.rs tests/transports.rs README.md docs/reference/agent-runtime-and-goal-lifecycle.md docs/reference/kite-language.md docs/reference/user-task-and-goal-contract.md examples/babysit_pr.kite
git commit -m "feat: expose authoritative agent lifecycle"
```

## Cross-task review gate B: Tasks 1-8 and Slice 3 handoff

Stop before recording implementation evidence in the design spec. Dispatch a
fresh independent reviewer using the cross-task review protocol. The reviewer
must not be an implementer or fixer from Tasks 5-8 and must evaluate the full
Slice 2 base-to-head diff, not only the second block. Save the initial verdict
to:

```text
.superpowers/sdd/2026-08-17-kiteframe-declarative-agents-slice-2/cross-task-review-1-8.md
```

This is the cumulative final review for Slice 2. It repeats Gate A's full-spec
mapping against the final code and additionally proves:

- Tasks 5-8 consume Tasks 1-4 without signature drift, duplicated authority,
  migration workarounds, weakened revisions, or changed stable errors;
- transition CAS, command recovery, evidence-backed complete/block,
  principal-only lifecycle actions, the direct-loop bridge, and private
  transports jointly preserve persist-before-dispatch and failure atomicity;
- every Slice 2 acceptance item maps to inspected implementation and freshly
  executed evidence, including configured PostgreSQL races and restart cases;
- every explicit Slice 3+ exclusion is absent and no documentation claims the
  GitHub/Codex/editor product path is operational;
- Slice 3 can replace `HarnessCommandExecutor` and use the command
  list/claim/complete/fail APIs without moving task, goal, evidence, snapshot,
  policy, idempotency, or recovery authority out of the Rust kernel; and
- the final contracts do not obstruct the context/event, worker, connector,
  editor, or `babysit_pr` outcomes of Slices 4-8.

Run the complete Task 8 Step 10 gate again from a clean review checkout, plus:

```bash
cargo test --all-features --test agent_runtime_contract --test agent_lifecycle_store --test agent_start --test kite_agent_runtime --test agent_transition_store --test agent_goal_authority --test agent_direct_bridge --test agent_protocol --test e2e_agent_restart --test postgres_lock_order --test repository_shape
git diff --check
git status --short
git diff --stat
```

Expected: all commands pass, required PostgreSQL cases execute, the final
coverage matrix has no gap or contradiction, and the report verdict is
`approved`. If blocked, record fixes in
`cross-task-review-1-8-fix-report.md` and obtain a fresh approved re-review.
Do not update implementation-status evidence or declare Slice 2 complete until
this gate passes.

- [ ] **Step 12: Record only existing implementation evidence**

After Step 11 and cross-task review gate B succeed, update only the design
spec's non-normative implementation-status note with the eight Slice 2
implementation commit hashes, the approved Gate A and Gate B report paths, and
the exact successful commands from Step 10 and Gate B. State that Slices 3-8
and the end-to-end product loop remain unimplemented. Do not include the
status-note commit's not-yet-existing hash and do not alter normative design
text.

```bash
git diff --check -- docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md
git add docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md
git commit -m "docs: record agent lifecycle slice evidence"
```

Expected: the note names only commits that already exist and only commands
whose successful output was observed in Step 10.

## Slice 2 acceptance gate

Slice 2 is complete only when all of the following are true:

- the complete Slice 1 gate is implemented, reviewed, and passing before any
  Slice 2 execution path is enabled;
- cross-task review gate A approves the cumulative Tasks 1-4 diff before Task
  5 begins, with no blocking spec, contract, compatibility, evidence, or
  future-slice finding;
- cross-task review gate B approves the cumulative Tasks 1-8 diff and Slice 3
  handoff before implementation evidence is recorded or Slice 2 is declared
  complete;
- both durable cross-task reports contain complete requirement mappings,
  executed configured-PostgreSQL evidence, exact commit ranges, future-task
  and future-slice checks, and an `approved` verdict; every fix round has a
  corresponding fix report and fresh approved re-review;
- `GoalPhase`, task lifecycle, and operational status remain separate on every
  edge, including waits, pause, block, root return, failure, cancellation, and
  completion;
- every valid goal-phase edge passes, `complete` is terminal, and failure or
  cancellation never changes a goal to complete;
- `AgentRuntime::validate_agent`, `start`, and `handle` are deterministic and
  side-effect free, and every transition replaces one canonical version-1
  snapshot under the exact ceilings;
- StartAgent uses only the task's frozen exact definition and atomically writes
  snapshot, ordered commands, transition, task revision, immutable policy
  decision, and event under session -> task -> goal locks;
- identical start replay/concurrency creates one execution, conflicting replay
  fails, and publishing a newer agent never changes an accepted task;
- built-in `.kite` call, structured fork/join, tail return, isolated/kept
  history, root done, ask proposal, fail, explicit complete, and explicit block
  semantics pass pure tests;
- root `done` and `HarnessResult` cannot infer or carry goal completion;
- transition CAS rejects stale task, goal, and command revisions; duplicate
  events are idempotent; every transaction failure leaves source command,
  snapshot, revisions, journals, evidence, and events unchanged;
- command list/claim/complete/fail APIs are closed, revisioned, idempotent, and
  ready for Slice 3 without a provider/runtime identity in `.kite`;
- restart restores the exact current snapshot and pending/claimed commands
  without replaying a transition or duplicating a command/event;
- `CompleteGoal` rejects missing, foreign, wrong-type, duplicate, extra, stale,
  and unauthorized evidence without mutation and succeeds only with exact
  current criterion coverage;
- `BlockGoal` binds the immutable zero-based `blocked_when` index, requires
  authorized evidence and unlock conditions, and retains immutable blocker
  history across resume;
- pause/resume/cancel require the authenticated task owner, expected task and
  goal revisions, and idempotency; the runtime cannot construct those actions;
- the direct-loop bridge dispatches only persisted claimed `InvokeHarness`,
  mints only command-bound `harness.invocation_result` metadata, and fails
  unsupported downstream commands/tool calls closed with stable errors;
- ACP v1/v2, NDJSON, and WebSocket expose identical private lifecycle results
  and coarse fixed errors without sensitive text;
- all legacy workflow bytes, hashes, protocol results, SQL behavior, execution,
  recovery, and stable failures pass unchanged;
- repository-shape and documentation checks prove no Slice 3-8 subsystem or
  product claim was pulled forward; and
- Rust, PostgreSQL, Python, TypeScript, Markdown, diff, and status gates pass
  with the user's unrelated paths preserved.

## Slice 3 handoff

Slice 3 may begin only after this acceptance gate is reviewed. It consumes,
without renaming or weakening:

```rust
AgentRuntime::{validate_agent, start, handle}
RuntimeStateSnapshot {
    schema_version,
    runtime_id,
    disposition,
    payload,
    ceiling_bytes,
    canonical_byte_count,
    snapshot_digest,
}
RuntimeTransition {
    idempotency_key,
    expected_task_revision,
    expected_goal_revision,
    snapshot,
    commands,
}
KernelCommand { schema_version, key, payload }
KernelCommandPayload::InvokeHarness(InvokeHarness {
    role,
    prompt,
    goal,
    input,
    input_schema,
    result_schema,
    tools,
    projected_history,
    limits,
})
RuntimeEvent::HarnessResult {
    event_id,
    command_key,
    outcome,
    fields,
    evidence_id,
    history,
}
PostgresStore::{
    dispatchable_agent_commands,
    claim_agent_command,
    load_agent_command_context,
    commit_agent_command_transition,
    fail_unavailable_agent_command,
    recover_agent_commands,
}
AgentKernel::{complete_command, fail_command}
```

Slice 3 replaces `HarnessCommandExecutor` with built-in and out-of-process
harness implementations plus registration, capability negotiation, common
versioned envelopes, and conformance. It MUST preserve the kernel's task/goal
authority, snapshot/CAS rules, command journal, evidence checks, root-done
semantics, and principal-only lifecycle actions. A harness result remains an
invocation result, never a goal-completion command.
