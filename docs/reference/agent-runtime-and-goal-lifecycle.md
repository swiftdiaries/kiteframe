# Agent runtime and goal lifecycle reference

This reference freezes the Slice 2 Rust contract between an agent runtime and
the authoritative Kiteframe kernel. Slice 2 implements the pure built-in
`KiteAgentRuntime` (`kite.v1`) and a bounded direct-loop harness executor. The
kernel remains the only durable task, goal, snapshot, command, policy, and
evidence authority; a harness cannot mutate those aggregates directly.

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

`GoalProjection` contains the full durable semantic goal: objective; every
ordered success criterion's id, human statement, and evidence type; all
constraints; and all blocked conditions. `GoalProjection::from_contract`
copies those immutable fields into every persisted `InvokeHarness`, so repeated
or recovered invocations receive the same goal rather than an abbreviated
objective label. `HarnessStructuralLimits` contains
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

A `running` transition with zero newly proposed commands is valid only after
consuming one structured child result in a structured partial join where at
least one persisted `pending` or `claimed` sibling command still matches a
wait in the full replacement snapshot. Task 5 must prove that
persisted-command/snapshot-wait correspondence atomically before any write.
Any other `running` transition with no newly proposed command, a `returned`
snapshot with a command, or an authority disposition with any other command
shape fails closed.

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

## Direct-loop execution and recovery

The process constructs one `AgentKernel`, runs startup recovery through it,
and derives one `AgentCommandDispatcher` with a
`DirectLoopHarnessExecutor`. The application lifecycle methods and the process
scheduler share that same kernel. The scheduler processes legacy workflow work
and persisted agent commands, retains the configured batch size and 50 ms idle
delay, continues after bounded failures, and is cancellable during shutdown.

The direct-loop executor receives only a persisted `InvokeHarness`. It builds
one bounded model request from the frozen semantic role, input, full goal
projection, projected history, exact result schema, tool descriptors, and
structural limits. Its closed `complete_task` function validates one declared
outcome and that outcome's exact fields. `complete_task` completes only the
bounded harness invocation and returns command-bound
`harness.invocation_result` evidence; it does not mean task success and cannot
construct `CompleteGoal` or `BlockGoal`. Those two authority commands remain
Rust-runtime output validated by the kernel.

Only `InvokeHarness` is dispatched by the built-in direct loop. Unsupported
downstream commands and tool calls fail closed through durable command/task
state. `RequestContext` and `WaitForEvent` remain defensive dispatcher cases:
no valid Slice 2 runtime state can emit them because canonical context/event
wait and resolution semantics belong to later slices.

If a harness result, model failure, or unavailable-command failure arrives
after cancellation or another terminal race, the kernel retains it as bounded,
idempotent audit metadata on the already-terminal command. That late audit may
advance only command metadata/revision; it cannot advance task, goal, snapshot,
evidence, action, or event state. Exact replay is a no-op and conflicting late
content fails closed.

## Private lifecycle/read model

ACP v1, gated ACP v2, NDJSON, and WebSocket expose the same private methods:

```text
_kiteframe/agent/start
_kiteframe/agent/get
_kiteframe/task/pause
_kiteframe/task/resume
_kiteframe/task/cancel
_kiteframe/task/events
```

The authenticated connection owns both principal and session authorization;
wire payloads cannot supply a principal or select another connection's
session. Start/pause/resume/cancel require the task owner, exact task and goal
revisions, and idempotency. Get and ordered event replay are session scoped.
Event cursors reject negative `after` values and limits outside 1-1,000.

The execution read model keeps task lifecycle, operational status, and goal
phase distinct. It exposes the immutable accepted task and goal contract,
their digests and revisions, a redacted blocker, and only runtime envelope
identity/disposition/digest. Opaque runtime payload/history, evidence ids and
sources, assertion ids, and blocker evidence stay behind the authorization
boundary. Stable failures use fixed coarse transport codes/messages. Transport
request logs contain only the allowlisted method, coarse error code, transport,
and expected task revision when present; they do not record request content.

This is not a public plugin/provider protocol. External runtime transports,
registration and capability negotiation, connectors, semantic channels,
workers/leases, budgets/profiles, secrets, Codex integration, and editor
integrations remain later-slice work.
