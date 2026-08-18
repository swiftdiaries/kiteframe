# Agent runtime and goal lifecycle reference

This reference freezes the Slice 2 Rust contract between an agent runtime and
the authoritative Kiteframe kernel. It defines data and validation boundaries;
it does not select a runtime implementation or let a harness mutate durable
task or goal state.

## Authoritative state

Task lifecycle and current operational status are independent:

```rust
pub enum TaskLifecycle { Accepted, Running, Succeeded, Failed, Cancelled }

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

pub enum GoalPhase { Active, Paused, Blocked, Complete }
```

`AgentExecutionView` is the runtime's read-only input. It contains the accepted
task, task lifecycle and operational status, exact task and goal revisions,
goal phase, optional blocker, and optional validated runtime snapshot. Runtime
output never mutates this view directly.

The principal commands are closed camel-case JSON objects and carry both
expected revisions and an `IdempotencyKey`:

```rust
pub struct StartAgent {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub idempotency_key: IdempotencyKey,
}

pub struct PauseUserTask {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub reason: String,
    pub idempotency_key: IdempotencyKey,
}

pub struct ResumeUserTask {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub idempotency_key: IdempotencyKey,
}

pub struct CancelUserTask {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub reason: Option<String>,
    pub idempotency_key: IdempotencyKey,
}
```

## Runtime boundary

One implementation satisfies this exact trait:

```rust
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
```

`AgentRuntimeError` has the stable variants `InvalidDefinition(String)`,
`InvalidState(String)`, `InvalidTransition(String)`, `InvalidValue(String)`,
`CeilingExceeded(&'static str)`, and `Canonicalization(String)`.

`AgentCommandId` is a transparent UUID. `RuntimeCommandKey` is a transparent
string constructed by `RuntimeCommandKey::new`; it contains 1-128 ASCII bytes,
each a lowercase letter, digit, `-`, `_`, `.`, `/`, or `:`. `as_str` returns
the stored key.

## Runtime state envelope and ceilings

The deployment ceiling defaults are exact:

| Ceiling | Default maximum |
|---|---:|
| snapshot bytes | 262,144 |
| command bytes | 65,536 |
| commands per transition | 64 |
| frames | 256 |
| history items per frame | 256 |

All configured values must be nonzero and no greater than the corresponding
default. Invalid configurations fail with
`CeilingExceeded("configured_ceilings")`.

```rust
pub enum RuntimeDisposition { Running, Returned, Blocked, Terminating }

pub struct RuntimeStateSnapshot {
    pub schema_version: u16,
    pub runtime_id: String,
    pub disposition: RuntimeDisposition,
    pub payload: serde_json::Value,
    pub ceiling_bytes: u32,
    pub canonical_byte_count: u32,
    pub snapshot_digest: String,
}
```

`RuntimeStateSnapshot::new` canonicalizes and reparses the payload and hashes
only the non-recursive content envelope
`{schemaVersion,runtimeId,disposition,payload}`. The schema version is `1`, the
runtime id is `kite.v1`, and the digest is lowercase SHA-256 of RFC 8785 JSON.
The stored metadata records the applied deployment ceiling and exact canonical
content byte count.

`canonical_bytes` and `digest` validate before returning data. They reject an
unsupported version/runtime id, a non-object payload, a stored ceiling of zero
or greater than 262,144, bytes above the stored or current ceiling, and any
byte-count or digest mismatch. Snapshot construction, transition validation,
and snapshot reads validate all configured ceilings first.

## Commands and transition validation

Every `KernelCommand` is a closed camel-case object with `schemaVersion`,
`key`, and `payload`. Schema version `1` is the only accepted version. Payload
is a closed snake-case tagged union:

| Payload | Exact fields |
|---|---|
| `RequestContext` | `evidenceType`, `subject` |
| `WaitForEvent` | `eventType`, `correlation` |
| `InvokeHarness` | `role`, `prompt`, `goal`, `input`, `inputSchema`, `resultSchema`, `tools`, `projectedHistory`, `limits` |
| `RequestApproval` | `question`, `answerSchema` |
| `CompleteGoal` | `assertions` |
| `BlockGoal` | `blockedWhenIndex`, `reasonCode`, `explanation`, `evidenceIds`, `unlockConditions` |
| `FailTask` | `code`, `message` |

`GoalProjection` contains objective, ordered `{id,evidenceType}` success
criteria, and constraints only. `GoalProjection::from_contract` does not expose
criterion prose or blocked conditions. `HarnessStructuralLimits` contains
`modelTurnsRemaining`, `toolCallsPerResponse`, `responseItemBytes`, and
`activeExecutionMillisRemaining`. A `HarnessTool` carries `qualifiedName`,
`descriptorDigest`, and the immutable descriptor. Projected history items
carry `sequence`, one of `harness_input`, `harness_output`, `tool_result`, or
`task_return`, and a JSON payload.

```rust
pub struct RuntimeTransition {
    pub idempotency_key: String,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub snapshot: RuntimeStateSnapshot,
    pub commands: Vec<KernelCommand>,
}
```

`RuntimeTransition.commands` is the ordered delta of newly proposed work, not
the complete set of persisted outstanding commands.
`running` normally proposes one or more newly proposed commands.

`validate_transition` validates ceilings, exact task and goal revisions, a
1-128 byte idempotency key, snapshot integrity, at most 64 commands, command
schema/key validity and uniqueness, and at most 65,536 canonical bytes per
command. Its disposition rules are exact:

| Disposition | Newly proposed command shape |
|---|---|
| `running` | normally one or more newly proposed commands; zero only for the structured partial-join exception below |
| `returned` | no commands |
| `blocked` | exactly one `BlockGoal` |
| `terminating` | exactly one `CompleteGoal` or `FailTask` |

A `running` snapshot with no newly proposed command is valid only after
consuming one structured child result while at least one persisted `pending`
or `claimed` sibling command still matches a wait in the full replacement
snapshot. Task 5 must prove that persisted-command/snapshot-wait
correspondence atomically before any write. Any other `running` transition
with no newly proposed command, a `returned` snapshot with a command, or an
authority disposition with any other command shape fails closed.

Kernel acceptance maps pending commands to operational status:

| Pending command | Operational status |
|---|---|
| `RequestContext` | `waiting_for_context` |
| `WaitForEvent` | `waiting_for_event` |
| `InvokeHarness` | `waiting_for_harness` |
| `RequestApproval` | `waiting_for_approval` |
| `CompleteGoal` | `finished` after authority validation |
| `BlockGoal` | `blocked` after authority validation |
| `FailTask` | `finished` |

Runtime events are the closed variants `HarnessResult`, `ContextResolved`,
`SemanticEvent`, `ApprovalResult`, `CommandFailed`, and `PrincipalResumed`.
Every event exposes `event_id()`. All except `PrincipalResumed` expose a
`command_key()`.

## Transaction and lock order

Lifecycle mutation is one kernel transaction. Implementations acquire the
session row first, then the user-task row, then the goal-state row, then the
runtime-state row. Command rows are acquired or inserted in
`RuntimeCommandKey` order; evidence rows are acquired or inserted in
`EvidenceId` order. Revisions and idempotency are checked while those locks are
held. No runtime or harness callback occurs while database locks are held.
Every later persistence slice must preserve this order to avoid deadlocks.

## Evidence and explicit goal authority

`EvidenceId` is a transparent UUID. Evidence identity is closed to:

```rust
pub enum EvidenceSourceIdentity {
    BuiltinHarness { command_id: AgentCommandId },
}

pub struct EvidenceRecord {
    pub id: EvidenceId,
    pub task_id: UserTaskId,
    pub evidence_type: String,
    pub source: EvidenceSourceIdentity,
    pub digest: String,
}
```

The kernel records the owning task, declared evidence type, exact built-in
harness command identity, and content digest. `CriterionAssertion` binds one
goal criterion id to one `EvidenceId`; it does not contain free-form evidence.
`GoalBlocker` records `blockedWhenIndex`, `reasonCode`, `explanation`, ordered
`evidenceIds`, ordered `unlockConditions`, and the resulting `goalRevision`.

`HarnessResult` supplies only an invocation outcome and its evidence/history.
It cannot complete a goal. Only a validated `CompleteGoal` command whose
assertions cover the immutable goal contract can change the phase to
`complete`. Only a validated `BlockGoal` can establish the durable blocker and
change the phase to `blocked`.
