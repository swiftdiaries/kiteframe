# Kiteframe Minimal Workflow Engine Design

- **Status:** Approved in brainstorming; pending written-spec review
- **Date:** 2026-08-11
- **Repository:** `swiftdiaries/kiteframe`
- **Scope:** Clean replacement of the current Kiteframe architecture
- **Deployment:** One Rust service process and one PostgreSQL database

## Executive summary

Kiteframe V1 is a small, author-authored workflow language and durable runtime
for tool-using agents. It follows the useful core of Firetiger's NXL model:
focused tasks, typed inputs and outcomes, explicit control flow, isolated
context, durable traversal, and structured parallelism.

Kiteframe is not an agent framework adapter. It owns the workflow semantics and
runs a direct model-with-tools loop. Models may call only the tools declared for
the current task and may finish that task only through one of its author-defined
typed outcomes. Authors, not models, choose the task graph and every legal
transition.

V1 is one Rust binary with ordinary internal modules and PostgreSQL as its sole
workflow authority. The model boundary is a deliberately narrow,
OpenAI-compatible Responses protocol that can target any provider passing
Kiteframe's conformance tests. Kiteframe sends stateless `store: false`
requests and persists the complete ordered response-item history itself.

The runtime provides sequential calls, tail transitions, bounded parallel
calls with mandatory joins, durable human questions, cancellation, typed
failures, and crash recovery around model and tool calls. It does not provide
distributed workers, detached agents, model-authored graphs, arbitrary code,
provider-specific features, or a general-purpose programming language.

This is a clean replacement. The existing package resolver, multi-crate runtime
architecture, Python and PyO3 layers, Deep Agents adapter, LangGraph direction,
OpenFGA integration, and related compatibility surfaces are not carried
forward. Historical documents and commits remain evidence of what was built;
they are not contracts for the replacement.

## Status and supersession

This document supersedes the product direction in the previous Kiteframe V1
designs and plans, including the package-first Deep Agents architecture and the
model-authored per-request graph direction recorded by the 2026-08-04 design
state audit. It does not claim that this replacement is already implemented.

The current implementation remains historical evidence until the replacement
lands. Implementation planning begins only after this written specification is
reviewed and approved.

## Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**,
and **MAY** are normative. Examples illustrate the design but do not override
the contracts in this document.

## Product goals

V1 has seven goals:

1. Let an author define a typed agent workflow in one readable source file.
2. Keep each task a small, focused model-with-tools loop with isolated context.
3. Make all legal transitions explicit and author-controlled.
4. Support bounded structured parallelism without detached work.
5. Persist enough state in PostgreSQL to resume safely after process failure.
6. Suspend durably for human input and resume idempotently.
7. Use a small Responses-compatible model protocol without adopting an agent
   framework as the semantic core.

## Non-goals

V1 does not provide:

- model-authored or per-request-generated workflow graphs;
- a general-purpose language, arbitrary expressions, or user-supplied code;
- dynamic task names, reflection, imports, macros, inheritance, or generics;
- detached tasks, background agents, polling handles, or cancellation trees;
- multiple service replicas, distributed workers, leases, fencing, or queues;
- S3-backed workflow state or event-notification scheduling;
- automatic transcript summarization or provider-specific compaction;
- Python bindings, PyO3, Deep Agents, LangGraph, LangChain, or MCP integration;
- an agent package manager, capability lockfile, or public registry;
- OpenFGA, multi-tenant identity, or end-user account management;
- provider-hosted tools, provider conversations, background mode, or WebSocket
  model sessions;
- a web UI, replay debugger, or visual workflow editor;
- automatic compensation or exactly-once effects in arbitrary external
  systems; or
- compatibility with the old API, manifests, databases, or runtime adapters.

These are exclusions, not implied future commitments.

## Design principles

### Authors own control flow

The model can select only an outcome declared by the current task. It cannot
invent a task, transition, branch, tool, or child agent. Every outcome has one
author-defined continuation.

### One durable authority

PostgreSQL is the sole authority for workflow definitions, run state, model
history, tool-call state, and event order. Provider-side conversation storage,
S3 objects, process memory, and logs are never authoritative.

### Small protocol, owned semantics

Kiteframe owns task frames, transitions, joins, suspension, retries, and
durability. A Responses-compatible provider supplies model inference and tool
call proposals; it does not own workflow state.

### Structured concurrency only

Parallel children are born under one explicit join. A parent cannot advance
until every child is terminal, and no child can outlive its join.

### Honest effect guarantees

Database transitions can be exactly once. Arbitrary external effects cannot.
Kiteframe therefore records intent before execution and requires idempotency or
status recovery before it retries an uncertain effect.

### One deployable, clear internal boundaries

V1 is a monolith, not a tangle. Modules have narrow interfaces, but they are
compiled, versioned, deployed, and operated as one service.

## System architecture

Kiteframe consists of one Rust package with these internal modules:

| Module | Responsibility |
|---|---|
| `dsl` | Lex, parse, type-check, validate, and compile workflow source. |
| `engine` | Schedule durable turns and interpret task transitions. |
| `store` | Own PostgreSQL transactions, migrations, claims, and recovery. |
| `model` | Implement the Responses-compatible inference boundary. |
| `tools` | Register descriptors, constrain presentation, dispatch calls, and recover effects. |
| `api` | Publish workflows, start and control runs, inspect state, and stream events. |
| `config` | Load deployment-owned database, provider, limits, credentials, and tool registry settings. |

The source tree SHOULD remain a single package:

```text
src/
  main.rs
  api.rs
  config.rs
  dsl/
  engine/
  model.rs
  store.rs
  tools.rs
migrations/
examples/
tests/
```

The service starts by validating configuration, applying its embedded
PostgreSQL migrations, loading the trusted tool registry, and checking database
readiness. It then recovers interrupted work and begins serving HTTP. Because
V1 permits only one service process, it needs no distributed lease or fencing
protocol.

## Workflow source

### One authored workflow per file

A source file contains exactly one workflow, zero or more named record types,
and one or more tasks. The workflow name is its stable publication identity.
Publishing assigns a monotonically increasing integer version and a definition
hash; the author does not manage versions in source.

Prompts are inline UTF-8 triple-quoted strings in V1. External prompt files,
includes, imports, and package assets are not supported.

### Example

```kite
type Evidence {
  source: string
  summary: string
  references: [string]
}

workflow support(ticket: string) -> string {
  start triage(ticket: ticket)

  limits {
    steps: 100
    model_turns_per_task: 12
    parallel_branches: 4
    active_execution_seconds: 300
  }
}

task triage(ticket: string, clarification: string?) -> string {
  prompt """
  Understand the ticket. Resolve it, request investigation,
  or ask the user for one missing detail.
  """

  tools [search_docs]

  on resolved(answer: string) {
    done answer
  }

  on investigate(query: string) {
    parallel {
      logs = call inspect_logs(query: query)
      metrics = call inspect_metrics(query: query)
    }

    goto compose(ticket: ticket, logs: logs, metrics: metrics)
  }

  on clarify(question: string) {
    answer = ask string question
    goto triage(ticket: ticket, clarification: answer)
  }
}

task inspect_logs(query: string) -> Evidence {
  prompt """Find log evidence relevant to the query."""
  tools [search_logs]

  on found(evidence: Evidence) {
    done evidence
  }

  on unavailable(reason: string) {
    fail "logs_unavailable" reason
  }
}

task inspect_metrics(query: string) -> Evidence {
  prompt """Find metric evidence relevant to the query."""
  tools [search_metrics]

  on found(evidence: Evidence) {
    done evidence
  }

  on unavailable(reason: string) {
    fail "metrics_unavailable" reason
  }
}

task compose(ticket: string, logs: Evidence, metrics: Evidence) -> string {
  prompt """Compose a concise answer from the supplied evidence."""
  tools []

  on composed(answer: string) {
    done answer
  }
}
```

### Type system

The DSL supports only:

- `string`, `integer`, `number`, and `boolean`;
- optional values written as `T?`;
- homogeneous lists written as `[T]`;
- named closed record types; and
- task outcomes, which form a task-local tagged union.

Records MUST have statically named fields and MUST reject unknown fields. The
language has no maps, tuples, untagged unions, generics, subtyping, implicit
coercions, or arbitrary JSON escape hatch. Literals are limited to strings,
numbers, booleans, and `null` for optional values. An omitted optional argument
is bound as `null`.

Tool input and output JSON Schemas MUST fit the same closed subset: objects with
named properties, required fields, primitive values, homogeneous arrays, and
string or numeric enums. Unsupported schema features make the tool unavailable
to workflow publication.

### Tasks and outcomes

Every task declares:

- typed input fields;
- one result type;
- one inline prompt;
- a static list of ordinary tools, which may be empty; and
- one or more named, typed outcome clauses.

At inference time Kiteframe adds one internal `complete_task` function tool.
Its portable wire schema is one closed object containing an `outcome` string
enum plus the union of every outcome field as optional properties. Kiteframe
then validates that exactly the fields required by the selected outcome are
present and that fields belonging only to other outcomes are absent. This
represents a task-local tagged union without requiring provider support for
JSON Schema `oneOf` or discriminators.

A task remains active across ordinary model and tool turns until the model
calls `complete_task` with a valid outcome. Invalid output is returned to the
model as a validation error within the task's turn budget.

If one response contains `complete_task` together with an ordinary tool call,
the response is invalid. Ordinary tool calls in one response execute in source
item order. Author-declared `parallel` blocks are the only V1 mechanism for
concurrent agent work.

### Bindings and control operations

A block may bind only:

- a task input;
- an outcome field;
- a `call` result;
- a named result from `parallel`; or
- an `ask` answer.

There are no arithmetic, boolean, string, collection, or user-defined
expressions. A transition passes existing bindings or literals into named
arguments.

The six control operations are:

| Operation | Semantics |
|---|---|
| `call` | Push a child invocation, wait for it, and bind its typed result. |
| `goto` | Replace the current task inside an invocation without growing the stack. |
| `parallel` | Start a statically listed set of `call` children and join every child. |
| `ask` | Persist a typed question, suspend the run, and bind the validated answer after resume. |
| `done` | Complete the current invocation with its declared result type; completing the root invocation completes the workflow. |
| `fail` | Terminate the workflow with an author-provided stable code and message. |

Every outcome block MUST end in exactly one terminal control operation:
`goto`, `done`, or `fail`. It MAY perform sequential `call` or `ask` operations,
or one or more sequential `parallel` blocks, before that terminal operation.

The workflow entry task's result type MUST match the workflow result type. A
`goto` target's result type MUST match the current invocation's result type. A
called task completes its child invocation with `done`; the validated value is
then bound in its caller. A child `fail` fails the call and propagates to the
run.

`goto` cycles are allowed because they do not grow the call stack. Recursive
`call` chains, including indirect recursion, are rejected at publication.
Runtime budgets bound all cycles.

The optional workflow `limits` block accepts only positive integer values for
`steps`, `model_turns_per_task`, `tool_calls_per_response`,
`parallel_branches`, `response_item_bytes`, and
`active_execution_seconds`. Omitted values inherit deployment defaults. Values
above deployment ceilings make publication fail.

### Context isolation

Each task frame owns an independent model transcript. Its initial input
contains only:

- the task prompt;
- its typed argument values;
- descriptors for its declared tools; and
- the internal `complete_task` descriptor.

A child does not receive its parent's transcript. A parent receives only the
child's validated typed result. Sibling branches share no transcript or mutable
memory. There are no `keep`, `hide`, `summarize`, or custom projection policies
in V1.

## Compilation and publication

`POST /v1/workflows` accepts workflow source. Publication performs all parsing,
name resolution, type checking, graph validation, limit validation, and tool
resolution before inserting anything.

The compiler MUST reject:

- invalid syntax or duplicate declarations;
- missing entry or target tasks;
- missing, duplicate, or unsupported tools;
- type-mismatched arguments, outcomes, results, or answers;
- missing or duplicate outcome handlers;
- use of reserved names such as `complete_task` or `outcome`;
- incompatible types for the same field name across outcome variants;
- blocks without exactly one terminal transition;
- unbound names or duplicate bindings;
- recursive `call` chains;
- invalid or empty parallel blocks;
- parallel result-name collisions;
- unreachable tasks; and
- source or compiled definitions exceeding deployment limits.

Publication stores the source, compiled definition, definition hash, and the
descriptor hash of each referenced tool in one transaction. The definition
hash is SHA-256 over RFC 8785 canonical JSON containing the exact UTF-8 source
the fixed DSL version `1`, and the referenced tool descriptors ordered by tool
name. Tool descriptor hashes use the same canonical JSON and SHA-256 rule.
Re-publishing identical source under identical tool descriptors for the same
workflow name returns the existing definition. A source or descriptor change
creates the next version.

A run can start only if the active tool registry still contains every captured
descriptor hash. A mismatch rejects run creation as an API validation error; it
does not silently substitute a different tool contract. On restart, a mismatch
for an active run makes readiness fail and prevents that run from advancing
until the exact registered contract is restored.

Compiled definitions are immutable. Existing runs always reference the exact
definition version selected when they started.

## Model protocol

### Portable Responses profile

Kiteframe has one `ModelClient` interface and one HTTP implementation. Provider
configuration supplies:

- one Responses API base URL ending at the provider's API prefix, normally
  `/v1`;
- credentials and provider headers;
- the model identifier; and
- request timeouts.

V1 configures one provider and one model per deployment. Kiteframe sends
requests to `{base_url}/responses`; tasks do not select a provider or model.

The provider MUST implement Kiteframe's portable Responses profile:

- non-streaming `POST /v1/responses` requests;
- `model`, `instructions`, `input`, `tools`, `tool_choice`, and `store: false`;
- ordered response output items;
- assistant message, refusal, reasoning, function-call, and function-output
  item preservation;
- strict function tools using Kiteframe's supported JSON Schema subset; and
- stable function call identifiers within a response history.

Kiteframe acts only on message, refusal, and function-call items it
understands. It preserves unknown response items opaquely and in order so they
can be replayed, but unknown items cannot trigger workflow behavior.

The portability claim means that a provider passes a Kiteframe-owned
conformance suite. It does not mean that every nominally OpenAI-compatible
endpoint implements the required semantics.

### Stateless history

Every request sets `store: false`. Kiteframe does not use provider conversation
objects, `previous_response_id`, or provider-side compaction. PostgreSQL stores
the full ordered item sequence for each task frame, including reasoning items,
function calls, and function outputs, and replays the sequence on the next
request.

On every turn, `instructions` contains the immutable task prompt plus
Kiteframe's fixed outcome-completion instructions. The initial `input` item is
a user message containing the task's validated arguments as canonical JSON.
Subsequent requests replay that item and every persisted provider or tool item
in order.

Provider-specific item fields are stored as opaque JSON. Application logs and
events MUST NOT copy prompt, message, reasoning, tool argument, or tool result
content by default.

## Durable execution model

### Run and frame states

A run has one of these states:

```text
queued | running | waiting_for_answer | outcome_unknown |
succeeded | failed | cancelled
```

A task frame has one of these states:

```text
runnable | waiting_for_model | waiting_for_tool |
waiting_for_children | waiting_for_answer |
outcome_unknown | completed | failed | cancelled
```

Only persisted state may make work runnable. In-memory futures are execution
mechanisms, not state.

Parallel branches may independently reach `ask`, so a run can have multiple
open suspensions. The run remains `running` while any frame can still advance
and becomes `waiting_for_answer` only when no executable work remains and one
or more frames await answers. `GET /v1/runs/{run_id}` returns the complete list
of open suspensions.

### Durable turn pattern

Every model or tool turn follows four steps:

1. In a short transaction, validate the current frame revision, record the
   intended attempt, and commit.
2. Perform the external call outside the transaction.
3. In a short transaction, record the complete result and its ordered items.
4. In the same result transaction, update the frame and make the next work
   runnable.

No database transaction remains open during a network call. Revision checks,
unique attempt identities, and PostgreSQL constraints prevent two local
executor tasks from advancing the same frame.

### Model-call recovery

A model call has no authority to perform an external effect. If the service
crashes after sending a request but before persisting a complete response, the
attempt is marked interrupted during startup recovery and a new attempt may be
issued. This can create an extra provider charge or a different response, but
an unpersisted response can never influence workflow state.

### Tool-call recovery

Before invoking a tool, Kiteframe stores the validated name, arguments, and a
stable idempotency key derived from the run, frame, and function-call identity.
The tool receives that identity with its invocation context.

A tool descriptor declares whether the tool is read-only or effectful and
whether it supports idempotent invocation or status lookup. Effectful tools
MUST support at least one of those recovery mechanisms.

After an interrupted call, Kiteframe asks the tool for status when supported.
Read-only calls may be retried. Effectful calls are retried automatically only
when the status proves that no effect completed or idempotent invocation is
supported. If an effect may have happened and its outcome cannot be recovered,
the frame and run enter `outcome_unknown` and require operator intervention
outside the V1 API. Kiteframe never guesses, repeats the effect blindly, or
reports success.

### Tool interface

Trusted deployment code registers tools at process startup. Each tool exposes
three conceptual operations:

```text
descriptor() -> ToolDescriptor
invoke(ToolCall) -> ToolResult
status(IdempotencyKey) -> ToolStatus
```

`ToolDescriptor` contains the stable name, description, closed input and output
schemas, effect classification, and recovery support. `ToolCall` includes the
run and frame identities, validated arguments, and idempotency key. `ToolResult`
and `ToolStatus` are typed and validated before entering model context.

The model has no generic filesystem, shell, HTTP, code execution, or MCP tool.
External access exists only inside trusted registered tool implementations.

### Structured parallelism

Entering a `parallel` block creates every child frame and their shared join
identity in one transaction. Children then advance independently, bounded by
the deployment concurrency limit.

The parent remains `waiting_for_children` until every child is terminal.
Successful results are assembled by source order, never completion order.

When one child fails:

1. children that have not started are cancelled;
2. children already in an external call are allowed to settle;
3. no new work is scheduled for a cancelled child; and
4. after every child is terminal, the join fails and propagates to the parent.

Cancellation does not compensate completed effects. A child can neither detach
from nor outlive its join.

### Human questions

`ask` stores the question, expected type, frame revision, and suspension
identity before changing that frame to `waiting_for_answer`. Other parallel
frames continue. When no frame can advance, the run becomes
`waiting_for_answer`, and the executor releases it while it waits.

Resumption supplies the suspension identity, answer, and a client-generated
idempotency key. Kiteframe validates the answer, accepts exactly one resolution,
records an event, binds the value, and makes the frame runnable in one
transaction. Repeating the same idempotency key returns the original result;
attempting a second distinct answer returns a conflict.

### Cancellation

Cancellation is cooperative and idempotent. It prevents new model, tool, and
child work from being scheduled. An already-running external call may finish;
its result is recorded for audit and recovery but cannot advance the cancelled
run. Cancellation performs no compensation.

## PostgreSQL persistence

PostgreSQL is the only V1 workflow authority. The schema has six logical
tables:

| Table | Durable responsibility |
|---|---|
| `workflow_definitions` | Immutable source, compiled form, name, version, definition hash, and tool descriptor hashes. |
| `runs` | Definition reference, typed input, current state, revision, limits, output or failure, and timestamps. |
| `frames` | Task identity, parent and join identity, bindings, state, result, suspension, and revision. |
| `model_items` | Complete ordered Responses items and attempt identity for each frame. |
| `tool_calls` | Validated arguments, idempotency identity, state, result, error, and recovery metadata. |
| `events` | Append-only, per-run ordered operational history. |

Join and human-suspension state live on `frames`; they do not require separate
subsystems. Constraints MUST enforce:

- unique workflow name and version;
- idempotent same-source publication;
- immutable definition contents;
- one run per workflow-scoped start idempotency key;
- one ordered item position per frame;
- one tool call per stable idempotency key;
- one resolution per human suspension;
- monotonic frame and run revisions; and
- monotonic event sequence per run.

Current-state rows are the execution source of truth. Events are for inspection,
streaming, and audit; the engine does not reconstruct state by replaying the
entire event log.

Startup recovery changes interrupted model attempts back to runnable. It
recovers interrupted tool attempts through their declared status/idempotency
contract. It reconstructs runnable joins and frames solely from committed
PostgreSQL state.

S3 MAY later store large immutable attachments or archives, but it MUST NOT
decide V1 workflow state or scheduling.

## Limits and failure model

### Limits

Deployment configuration defines ceilings for:

- source and compiled-definition size;
- workflow steps;
- active model turns per task;
- tool calls per model response;
- parallel branches per block;
- concurrent branches per run and process;
- request and response-item size;
- model and tool call timeout; and
- active run execution time.

Time spent durably waiting for a human answer does not consume active execution
time. A workflow MAY declare lower limits but MUST NOT raise deployment
ceilings.

V1 does not compact or summarize a transcript automatically. Exceeding a turn,
size, or context limit fails visibly as `limit_exceeded`.

### Stable run failures

Run failures use this small stable taxonomy:

| Code | Meaning |
|---|---|
| `definition_invalid` | Persisted or supplied data violates the compiled workflow contract. |
| `workflow_failed` | An author-declared `fail` ended the workflow with its stable code and message. |
| `model_unavailable` | The configured provider remained unavailable after allowed retries. |
| `model_output_invalid` | The model repeatedly failed the task's response contract. |
| `tool_failed` | A tool returned a definitive terminal failure. |
| `outcome_unknown` | An external effect may have occurred but cannot be resolved safely. |
| `limit_exceeded` | A deployment or workflow budget was exhausted. |
| `cancelled` | A client cancelled the run. |
| `internal` | Kiteframe violated an invariant or encountered an unclassified defect. |

`outcome_unknown` is both a stable condition and a suspended run state. It is
never converted automatically into `tool_failed` or success.

Transient model and tool transport errors use capped exponential backoff with
jitter. Retry counts and delays are deployment configuration, not DSL syntax.
Author-defined catch blocks and retry policies are excluded from V1.

## HTTP API

The core API is:

```text
POST /v1/workflows
POST /v1/workflows/{name}/runs
GET  /v1/runs/{run_id}
GET  /v1/runs/{run_id}/events?after={sequence}
POST /v1/runs/{run_id}/answers
POST /v1/runs/{run_id}/cancel
GET  /healthz
GET  /readyz
```

### Publish workflow

`POST /v1/workflows` accepts the DSL source. It returns the workflow name,
assigned version, definition hash, and compiler diagnostics. Publication is
atomic: no definition exists when validation fails.

### Start run

`POST /v1/workflows/{name}/runs` accepts typed workflow input and an optional
definition version. It requires an `Idempotency-Key` header scoped to the
workflow name. With no version, the transaction selects the latest successfully
published version at that instant. Repeating the same key and request returns
the original run; reusing the key with different input or version returns a
conflict. The response contains the run identity and selected definition
version.

### Inspect and stream

`GET /v1/runs/{run_id}` returns the current run state, selected definition,
all open suspensions, final output, or stable failure.

The events endpoint returns historical JSON by default. With
`Accept: text/event-stream`, it sends the same events over SSE and continues
waiting for new events. `after` is an exclusive durable sequence cursor, so a
client can reconnect without gaps or dependence on process memory.

### Answer and cancel

The answers endpoint accepts a suspension identity, typed answer, and
an `Idempotency-Key` header. The cancellation endpoint accepts the same header
and an optional reason. Both operations are transactional and safe to repeat.

### Trust boundary

V1 assumes one trusted deployment boundary. TLS termination and caller
authentication belong to deployment infrastructure. Kiteframe stores provider
credentials only in process configuration and never in workflow source or the
database.

This boundary does not authorize a public, unauthenticated deployment. It means
that tenant identity, user accounts, and fine-grained API policy are outside the
V1 product rather than partially implemented inside it.

## Observability

V1 provides:

- ordered run events;
- structured JSON service logs;
- stable run, frame, model-attempt, and tool-call correlation identities; and
- liveness and database/tool-registry readiness endpoints.

Logs and events contain metadata, status, timing, token usage when supplied,
and stable error codes. They exclude prompt content, model content, reasoning,
tool arguments, tool results, credentials, and human answers by default.

OpenTelemetry export, Prometheus metrics, content capture, replay debugging,
and a web UI are deferred.

## Clean replacement

The replacement removes rather than preserves parallel architectures. The
implementation plan will delete the current:

- multi-crate workspace structure;
- agent package manifest, resolver, lock, and compiled package IR;
- Python packages and PyO3 projections;
- Deep Agents adapter and LangGraph coupling;
- OpenFGA and multi-layer admission machinery;
- runtime-provider abstraction layers; and
- obsolete tests, fixtures, scripts, and generated schemas tied only to those
  contracts.

The new engine may reuse small pieces of well-tested Rust code only when they
fit this specification directly and do not preserve an old abstraction. The
tool descriptor, typed validation, idempotency, and effect-status ideas survive
through the new `tools` boundary; their previous package and provider machinery
does not.

Historical specifications remain in Git. Documentation visible as current
product guidance MUST be updated or marked superseded when implementation
lands.

There is no old-database migration, compatibility endpoint, dual-write period,
or gradual runtime cutover. The replacement starts with a new PostgreSQL schema
and new V1 API.

## Verification strategy

### Compiler tests

- Golden parse and compiled-definition fixtures for valid workflows.
- One focused rejection fixture for every compiler diagnostic class.
- Type, reachability, recursive-call, binding, outcome, and parallel-join
  adversarial tests.
- Deterministic definition hash and same-definition publication tests.

### Engine tests

- State-machine tests with fake `ModelClient` and tool registry implementations.
- Ordinary tool loops and typed `complete_task` validation.
- `call`, `goto`, `done`, `fail`, and bounded-cycle behavior.
- Parallel completion in every ordering, failure propagation, and cancellation.
- Typed `ask`, duplicate answer, conflicting answer, and resume tests.

### Persistence and crash tests

- PostgreSQL transaction tests for every uniqueness and revision constraint.
- Restart after intent commit and before result commit for model and tool calls.
- Idempotent tool retry, status recovery, and `outcome_unknown` behavior.
- Restart while children run, while a join waits, and while a run awaits a
  human answer.
- SSE cursor reconnect without gaps or duplicate state transitions.

### Provider conformance

A fake Responses server verifies exact outbound requests, `store: false`, full
ordered item replay, unknown-item preservation, tool schemas, function call
outputs, refusals, timeouts, malformed responses, and retry classification.

Real-provider smoke tests are optional deployment checks, not deterministic CI
gates and not a substitute for protocol conformance tests.

## V1 acceptance scenario

V1 is complete only when one end-to-end test proves all of the following:

1. An author publishes the example-style workflow through the HTTP API.
2. A caller starts a run with typed input.
3. The engine executes persisted model and ordinary tool turns.
4. The workflow starts bounded parallel task calls and joins their typed
   results in source order.
5. The workflow durably asks for human input and resumes idempotently.
6. Forced process restarts during model, tool, parallel, and suspended states do
   not corrupt or double-advance the run.
7. The caller observes ordered events and receives a typed final result.
8. An uncertain external effect produces `outcome_unknown`, never guessed
   success or an unsafe blind retry.

Passing unit tests without this PostgreSQL-backed scenario is insufficient.

## Deferred ideas

The following may be considered only after V1 evidence shows a need:

- external prompt assets;
- transcript compaction or author-defined context projection;
- multiple model profiles;
- distributed service replicas and workers;
- artifact storage in S3;
- MCP tool adapters;
- richer authorization and tenant isolation;
- OpenTelemetry or Prometheus export;
- author-defined retry/catch/compensation behavior;
- workflow visualization; and
- provider-specific Responses extensions.

They are not roadmap promises and MUST NOT shape the V1 abstractions in
advance.

## Implementation-planning boundary

This specification defines the replacement contract. It does not authorize
implementation before written review. After approval, the implementation plan
should deliver the replacement in cumulative, independently reviewable slices,
fully detailing only the next active slice rather than recreating the previous
large speculative wave structure.
