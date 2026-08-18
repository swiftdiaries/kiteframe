# UserTask and GoalContract reference

This reference freezes the consolidated Slice 2 submission, lifecycle,
evidence, and explicit goal-authority contract for durable user tasks.

GoalContract is immutable after atomic task acceptance.
The authenticated connection supplies the principal.
The accepted task freezes one exact published agent definition.
Harness completion is invocation evidence, not goal completion authority.
Slice 2 owns goal phases, evidence assertions, blockers, and completion commands.

## Validation

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
`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`.

The repository is exactly two non-empty GitHub path segments using ASCII
letters, digits, `.`, `_`, or `-`, with no leading or trailing slash. Text is
trimmed only for validation and stored byte-for-byte. Every JSON object denies
unknown fields.

The kernel allocates the UUID v7 task id. An explicit agent version freezes
that version. An omitted version selects the latest visible version inside the
acceptance transaction. An idempotent replay returns the original selection.

## Persistence and current API boundary

Acceptance atomically inserts one immutable task and one immutable goal
contract. Rejected validation, missing or wrong-kind definitions, principal or
session mismatches, closed sessions, and conflicting replays do not leave
partial rows. Retrieval is session-scoped, survives a store restart, and
recomputes the goal digest before returning the accepted intent. Acceptance
does not create runs, frames, events, model items, tool calls, or permission
requests.

The authenticated application exposes `_kiteframe/task/submit` and
`_kiteframe/task/get` as private Kiteframe ACP extension methods. Mutable
execution uses `_kiteframe/agent/start`, `_kiteframe/agent/get`,
`_kiteframe/task/pause`, `_kiteframe/task/resume`,
`_kiteframe/task/cancel`, and ordered `_kiteframe/task/events`. ACP v1, gated
ACP v2, NDJSON, and WebSocket return the same bounded read model and coarse
fixed failures. Along with `_kiteframe/definition/publish`, these are private
application methods rather than a stable plugin protocol.

The connection owns the principal and active session. Lifecycle payloads deny
unknown fields and cannot carry a wire principal. Start, pause, resume, and
cancel also pass the connection principal to the authoritative kernel; get and
event replay remain session scoped. Event replay accepts a non-negative cursor
and a limit from 1 through 1,000.

## Lifecycle and optimistic commands

The task lifecycle is one of `accepted`, `running`, `succeeded`, `failed`, or
`cancelled`. Operational status is independently one of `not_started`,
`runnable`, `waiting_for_context`, `waiting_for_event`,
`waiting_for_harness`, `waiting_for_approval`, `paused`, `blocked`, or
`finished`. Goal phase is independently one of `active`, `paused`, `blocked`,
or `complete`.

`StartAgent`, `PauseUserTask`, `ResumeUserTask`, and `CancelUserTask` identify
one session and accepted task, carry exact expected task and goal revisions,
and require an idempotency key. Pause carries a required reason. Cancel carries
an optional reason. A revision mismatch or conflicting replay fails without a
partial state transition.

The authoritative state matrix is exact:

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

`derived from pending command` maps context, event, harness, and approval
requests to their corresponding waiting status; a runtime with runnable work
uses `runnable`.

## Evidence, completion, and blockers

Every evidence record has an opaque UUID `EvidenceId`, owning `UserTaskId`,
declared evidence type, source identity, and lowercase content digest. The
closed source identity is `builtin_harness` with the exact `AgentCommandId`
that produced it. Harnesses cannot supply arbitrary source principals.

A `CriterionAssertion` contains only `criterionId` and `evidenceId`.
`CompleteGoal` contains an ordered assertion list. The kernel resolves those
ids against durable evidence for the same task and validates criterion ids,
evidence types, coverage, and revisions before changing goal phase.

`BlockGoal` supplies `blockedWhenIndex`, stable `reasonCode`, explanation,
ordered evidence ids, and non-empty ordered unlock conditions. Once accepted,
the durable `GoalBlocker` also records the resulting `goalRevision`. The index
selects an existing immutable `GoalContract.blockedWhen` entry; free-form text
cannot create a new blocking condition.

A `HarnessResult` supplies an invocation outcome only; even a successful root
outcome needs explicit `complete goal` plus kernel-validated evidence to change
the goal phase. Root `done` can succeed the task while leaving the goal phase
unchanged. Harness text, a model claim, or a successful tool response is never
goal-completion authority by itself.

The direct-loop harness receives the durable full goal projection on every
invocation: objective; each success criterion's id, human statement, and
evidence type; constraints; and blocked conditions. Its closed `complete_task`
result completes only the bounded invocation and can mint only
`harness.invocation_result` evidence. Only Rust-runtime `CompleteGoal` and
`BlockGoal` commands can exercise goal authority.

Slice 2 implements durable command/evidence persistence, revisioned
transitions, lifecycle authority, direct-loop dispatch, restart recovery, and
the private read/control surface. It does not implement canonical
context/event resolution, a public plugin/provider lifecycle or external
runtime transport, semantic channels, workers/leases, budgets/profiles,
connectors, secrets, Codex integration, or editor integrations. In particular,
the GitHub-typed `babysit_pr` goal cannot complete from Slice 2's
harness-invocation evidence alone; later context/connector work must supply
authorized GitHub evidence.

Late harness results and bounded failures that lose a cancellation/terminal
race are retained idempotently on the already-terminal command for audit. They
cannot advance task, goal, snapshot, evidence, action, or event state; exact
replay is a no-op and conflicting late content fails closed.
