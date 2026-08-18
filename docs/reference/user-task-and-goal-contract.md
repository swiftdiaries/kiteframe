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
`_kiteframe/task/get` as private Kiteframe ACP extension methods. Along with
`_kiteframe/definition/publish`, these are not a stable plugin protocol and do
not grant runtime authority.

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

This task freezes the contract surface. Later Slice 2 tasks add durable
command/evidence persistence, plugin lifecycle, semantic channels, workers,
budgets, connectors, and editor integrations without redefining these types.
