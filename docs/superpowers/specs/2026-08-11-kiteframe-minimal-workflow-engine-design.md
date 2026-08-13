# Kiteframe Minimal Workflow Engine Design

- **Status:** Approved; implementation not started
- **Date:** 2026-08-11
- **Repository:** `swiftdiaries/kiteframe`
- **Scope:** Clean replacement of the current Kiteframe architecture
- **Deployment:** One Rust harness process and one PostgreSQL database

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

V1 is one Rust harness binary with ordinary internal modules and PostgreSQL as
its sole workflow authority. Python and TypeScript applications use first-party
thin clients that bundle and launch that binary for local `stdio` use or connect
to the same binary remotely. They do not embed, reimplement, or extend workflow
semantics.

The client boundary is Agent Client Protocol (ACP). Kiteframe negotiates stable
ACP v1 and draft ACP v2 per connection, uses the official Rust SDK, and exposes
the same Kiteframe extension methods over `stdio`, WebSocket, or Streamable HTTP.
The model boundary remains a deliberately narrow, OpenAI-compatible Responses
protocol. Kiteframe sends stateless `store: false` requests and persists the
complete ordered response-item history itself.

The runtime provides sequential calls, tail transitions, bounded parallel
calls with mandatory joins, durable human questions and permissions,
cancellation, typed failures, principal-scoped MCP tool discovery, session
capability policy through embedded Cedar, and crash recovery around model and
tool calls. It does not provide distributed workers, detached agents,
model-authored graphs, arbitrary code, provider-specific model features, or a
general-purpose programming language.

This is a clean replacement. The existing package resolver, multi-crate runtime
architecture, PyO3 bindings, Deep Agents adapter, LangGraph direction, OpenFGA
integration, and related compatibility surfaces are not carried forward. The
new Python and TypeScript packages are protocol clients for the Rust harness,
not continuations of those runtime adapters. Historical documents and commits
remain evidence of what was built; they are not contracts for the replacement.

## Status and supersession

This document supersedes the product direction in the previous Kiteframe V1
designs and plans, including the package-first Deep Agents architecture and the
model-authored per-request graph direction recorded by the 2026-08-04 design
state audit. It does not claim that this replacement is already implemented.

The current implementation remains historical evidence until the replacement
lands. The user approved autonomous implementation planning on 2026-08-11;
that approval does not authorize implementation itself.

## Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**,
and **MAY** are normative. Examples illustrate the design but do not override
the contracts in this document.

## Product goals

V1 has ten goals:

1. Let an author define a typed agent workflow in one readable source file.
2. Keep each task a small, focused model-with-tools loop with isolated context.
3. Make all legal transitions explicit and author-controlled.
4. Support bounded structured parallelism without detached work.
5. Persist enough state in PostgreSQL to resume safely after process failure.
6. Suspend durably for human input and resume idempotently.
7. Use a small Responses-compatible model protocol without adopting an agent
   framework as the semantic core.
8. Run one identical Rust harness behind Python, TypeScript, editor, local, and
   remote client surfaces.
9. Use one ACP message model over `stdio`, WebSocket, and Streamable HTTP.
10. Apply default-deny, session-scoped Cedar policy to principal-visible MCP
    tools before every invocation.

## Non-goals

V1 does not provide:

- model-authored or per-request-generated workflow graphs;
- a general-purpose language, arbitrary expressions, or user-supplied code;
- dynamic task names, reflection, imports, macros, inheritance, or generics;
- detached tasks, background agents, polling handles, or cancellation trees;
- multiple service replicas, distributed workers, leases, fencing, or queues;
- S3-backed workflow state or event-notification scheduling;
- automatic transcript summarization or provider-specific compaction;
- Python or TypeScript workflow runtimes, PyO3, Deep Agents, LangGraph, or
  LangChain integration;
- an agent package manager, capability lockfile, or public registry;
- OpenFGA, organization relationships, multi-tenant identity, deployment or
  workspace policy scopes, or end-user account management;
- provider-hosted tools, provider conversations, background mode, or WebSocket
  model sessions;
- in-flight steering; steering is a separate feature with its own design and
  implementation plan;
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

### One harness, many clients

The Rust binary owns compilation, orchestration, persistence, model calls, tool
dispatch, and policy enforcement. Clients own process launch, transport,
presentation, permission interaction, and client-native MCP servers. The only
cross-language boundary is ACP plus explicitly advertised `_kiteframe/*`
extensions.

### Discovery is not permission

An authenticated MCP server decides which tools a principal can discover.
Workflow source decides which discovered tools a task can request. Cedar then
decides whether the current session may invoke a requested tool with validated
arguments. A tool MUST pass all three boundaries.

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

Kiteframe consists of one Rust harness package with these internal modules:

| Module | Responsibility |
|---|---|
| `dsl` | Lex, parse, type-check, validate, and compile workflow source. |
| `engine` | Schedule durable turns and interpret task transitions. |
| `store` | Own PostgreSQL transactions, migrations, claims, and recovery. |
| `model` | Implement the Responses-compatible inference boundary. |
| `tools` | Resolve in-process and principal-scoped MCP tools, constrain presentation, dispatch calls, and recover effects. |
| `mcp` | Connect authenticated servers, snapshot visible catalogs, receive change notifications, and call bound tools. |
| `policy` | Compile typed session rules to Cedar, validate them against visible tool schemas, and authorize invocations. |
| `protocol` | Negotiate ACP v1/v2, translate `_kiteframe/*` methods, and serve all transports. |
| `config` | Load deployment-owned database, provider, limits, credentials, and trusted capability mappings. |

The source tree SHOULD remain a single package:

```text
src/
  main.rs
  config.rs
  dsl/
  engine/
  mcp/
  model/
  policy/
  protocol/
  store/
  tools/
migrations/
clients/
  python/
  typescript/
examples/
tests/
```

The harness starts by validating configuration, applying its embedded
PostgreSQL migrations, loading in-process tools and trusted capability
mappings, and checking database readiness. It then recovers interrupted work
and serves either one local ACP connection over `stdio` or remote ACP
connections on `/acp`, plus health endpoints. Because V1 permits only one
harness process, it needs no distributed lease or fencing protocol.

## Harness and client protocol

### ACP versions and Rust SDK

Kiteframe pins `agent-client-protocol = "=2.0.0"` and
`agent-client-protocol-http = "=2.0.0"`. It uses the SDK protocol router to keep
ACP v1 and v2 wire types separate while mapping both into one version-neutral
application command layer.

ACP v1 remains enabled for ecosystem compatibility. ACP v2 support is additive,
selected only through `initialize` negotiation, and guarded by explicit runtime
configuration while the overall v2 surface remains draft. One connection
speaks exactly one negotiated ACP version. Unknown extension values and `_meta`
are preserved where ACP requires forwarding or durable replay.

Kiteframe advertises its custom surface in initialization metadata. Custom
requests and notifications use the `_kiteframe/` namespace:

```text
_kiteframe/workflow/publish
_kiteframe/run/start
_kiteframe/run/get
_kiteframe/run/answer
_kiteframe/run/cancel
_kiteframe/session/policy/update
_kiteframe/run/event
```

Steering is intentionally absent. It will be designed as a separate extension
without changing the V1 policy or transport contracts.

### Transports

All transports carry the same ACP JSON-RPC messages and lifecycle:

- `stdio` is the local default. A thin client launches its bundled harness as a
  child process and exchanges newline-delimited ACP messages.
- WebSocket is the preferred remote profile on `GET /acp` with an upgrade.
- Streamable HTTP is the remote fallback on the same `/acp` endpoint, using
  POST requests and the SDK's streaming response channel.

Clients try WebSocket first and fall back to Streamable HTTP only for transport
or upgrade incompatibility. Authentication failure, policy denial, malformed
messages, and application errors MUST NOT trigger fallback. Durable run and
session identities survive transport disconnects; connection-local MCP
credentials do not.

### First-party thin clients and bundled binary

PyPI publishes `kiteframe`, installed with `uv add kiteframe`. Each supported
platform wheel contains the matching Rust harness executable. The Python API
offers `Kiteframe.local(...)` for bundled `stdio` launch and
`Kiteframe.connect(...)` for remote ACP.

The npm package is also named `kiteframe`, installed with `bun add kiteframe`.
It exports its typed API from `kiteframe/client` and resolves the harness from a
version-matched platform package. It offers the same local and remote behavior.

Client and bundled-binary versions MUST match. Initialization verifies client,
harness, ACP, and `_kiteframe` extension versions before creating a session.
The clients handle process lifecycle, transport fallback, typed request and
event mapping, and permission prompts. They contain no DSL compiler, scheduler,
model loop, policy authorizer, or durable workflow state. Bundling the binary
does not bundle PostgreSQL; local launch still requires valid harness
configuration and a reachable database.

## Sessions, MCP catalogs, and capability policy

### Durable ACP sessions

ACP v1 `session/new` and `session/load`, and ACP v2 `session/new`,
`session/list`, `session/resume`, and `session/close`, map to a version-neutral
`HarnessSession` stored in PostgreSQL. ACP v1 has no standard list or close
method; a v1 client retains a known session ID for `session/load`, and the
harness applies deployment-owned retention rather than inventing incompatible
replacement methods. A session owns its principal binding, working directory,
MCP server bindings, catalog epochs, typed capability policy, accumulated
permission decisions, and runs. One session has one validated principal
binding in V1; changing the principal or MCP credentials requires reconnecting
and creating a fresh catalog epoch.

Secrets used to authenticate MCP connections are connection-local. They MUST
NOT be written to PostgreSQL, events, diagnostics, or logs. A resumed client
must supply fresh MCP connection configuration. The harness reconnects and
re-lists tools before allowing affected runs to advance.

### Principal-scoped MCP discovery

`tools/list` is evaluated through the authenticated MCP connection and MAY
return a different catalog for each invoking principal. Kiteframe MUST NOT
fetch an administrator or global superset and then rely on Cedar to hide it.
Tool names, descriptions, and schemas may themselves be sensitive.

For every configured MCP server, session creation or resume:

1. establishes an authenticated connection without taking identity from tool
   arguments or mutable request data;
2. follows every `tools/list` pagination cursor under configured count and byte
   limits;
3. qualifies each name as `<server-alias>/<tool-name>` and rejects collisions;
4. validates each descriptor against Kiteframe's closed schema subset;
5. stores a canonical descriptor snapshot and digest as a new catalog epoch;
6. generates and validates the session's Cedar action schema from exactly that
   visible snapshot; and
7. records only a non-secret connection binding identity with the epoch.

Visibility is not invocation permission. The model-visible tool surface for a
task is the intersection of the principal-visible catalog and the immutable
workflow definition's declared tools and descriptor hashes. Cedar is evaluated
only after the model proposes a validated call, immediately before invocation.
This lets an otherwise requestable, default-denied tool produce a permission
request. A tool is actually invoked only when discovery, workflow binding, and
the current Cedar decision all allow it.

When a server advertises tool-list change notifications, Kiteframe subscribes
and re-lists through the same authenticated connection. Additions become
eligible only for publications or runs created after the resulting automatic or
explicit catalog refresh. Removal revokes future calls immediately. A
descriptor change never mutates an active run's binding; an affected attempted
call fails closed with `tool_contract_changed`. The MCP server remains
responsible for authorizing every `tools/call`; Cedar is an additional
client-side boundary, not a cache or replacement for server authorization.

### Trusted capability classification

Every tool has one or more capability tags, such as `fs_read`, `fs_write`,
`shell`, `web_fetch`, `mcp`, or `mcp_unknown`. Capability policy can target a
tag once and cover every mapped tool.

An MCP descriptor's metadata MAY suggest tags but MUST NOT establish them as a
security fact. The session configuration or deployment-owned mapping confirms
the tags before the model sees the tool. An unmapped MCP tool receives only
`mcp_unknown` and therefore requires an explicit exact-tool grant before use.
The model cannot change tool tags, server bindings, policy rules, or catalog
epochs.

### Typed session policy

Python and TypeScript callers submit the same versioned structure rather than
raw Cedar text:

```json
{
  "version": 1,
  "rules": [
    {
      "ruleId": "allow-project-reads",
      "effect": "permit",
      "target": {
        "type": "capability",
        "names": ["fs_read"]
      },
      "input": {
        "/root": { "oneOf": ["src", "tests"] }
      }
    },
    {
      "ruleId": "deny-shell",
      "effect": "forbid",
      "target": {
        "type": "tool",
        "names": ["workspace/run_shell"]
      }
    }
  ]
}
```

`effect` is exactly `permit` or `forbid`. `target` is a tagged union selecting
one or more exact capability names or qualified tool names. Optional `input`
constraints map RFC 6901 JSON Pointers to non-empty `oneOf` lists containing
only string, integer, number, boolean, or `null` values. Every rule has a unique
caller-chosen `ruleId`; empty selectors, unknown capabilities, invisible tools,
unsupported input paths, duplicate IDs, and values outside the referenced tool
schema are validation errors.

The Rust harness deterministically compiles this structure into Cedar policies,
entities, and a schema. It pins `cedar-policy = 4.11.0` and
`cedar-policy-mcp-schema-generator = 0.6.0`. It adopts the schema-generation,
session-context, consent, and input-restriction patterns from
`cedar-policy/cedar-for-agents`, but does not depend on the unpublished
`cedar-policy-agent-builder` crate.

An exact-tool rule is compiled against both the qualified name and the
descriptor digest visible when the rule is accepted. If that tool disappears,
the rule becomes inactive; a different descriptor later using the same name
does not inherit it. After every catalog refresh, Kiteframe deterministically
rebuilds the active Cedar artifact: exact-tool rules activate only on their
captured digest, while capability rules cover the current visible tools whose
tags were confirmed by trusted configuration. Historical typed rules remain
immutable for audit.

The Cedar request uses `Kiteframe::Session::<session-id>` as principal, the
qualified tool and descriptor digest as resource, and the exact tool action
under its trusted capability parent actions. Context contains the validated
input and durable run, frame, tool-call, policy-revision, and session counters.
Cedar is default deny, and any matching `forbid` overrides permits.

The immutable workflow tool list and Rust safety invariants are authority
ceilings outside Cedar. A Cedar permit cannot add an undeclared tool, accept an
invalid argument, bypass effect recovery, or widen an MCP server's visible
catalog.

### Permission decisions

Before every ordinary tool invocation, Kiteframe evaluates the current policy
revision. An allow proceeds. A deny caused by a matching `forbid` returns a
stable denied tool result to the model without prompting. A default deny with
no matching forbid creates a durable permission request and moves only that
frame to `waiting_for_permission`; other structured parallel work may continue.

The client receives the version-appropriate ACP permission request with four
Kiteframe-defined choices:

```text
allow_once | allow_tool_for_session |
deny_once  | deny_tool_for_session
```

An `allow_once` decision creates a permit scoped to the exact durable tool-call
identity. `allow_tool_for_session` appends a permit for that exact qualified
tool, its current descriptor digest, and all schema-valid inputs for the
remainder of the session.
`deny_once` resolves only the pending call as denied.
`deny_tool_for_session` appends an exact-tool `forbid`. Capability-wide or
input-constrained changes use `_kiteframe/session/policy/update` so the client
must present the complete typed rule explicitly.

Permission decisions and policy updates require client-generated idempotency
keys. They append immutable rules, increment a monotonic policy revision, record
metadata-only events, and wake affected frames transactionally. Policy changes
apply to future calls and calls not yet externally invoked; they cannot cancel
or reinterpret an external effect already in flight.

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

On strict function-tool wires, every object property is listed as required.
`T?` is encoded as the single permitted type union, `[T, "null"]`; omission is
normalized to an explicit `null` before a value reaches the provider.

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
Its portable wire schema is one closed object containing a required `outcome`
string enum plus the union of every outcome field. Every union field is required
but nullable. Kiteframe then validates that fields required by the selected
outcome are non-null, its optional fields have their declared value or `null`,
and fields belonging only to other outcomes are `null`. This represents a
task-local tagged union without requiring provider support for JSON Schema
`oneOf` or discriminators.

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

`_kiteframe/workflow/publish` accepts workflow source inside an ACP session.
Publication performs all parsing, name resolution, type checking, graph
validation, limit validation, and principal-visible tool resolution before
inserting anything.

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
qualified server/tool identity, catalog epoch, and descriptor hash of each
referenced tool in one transaction. The definition hash is SHA-256 over RFC
8785 canonical JSON containing the exact UTF-8 source, the fixed DSL version
`1`, and the referenced tool descriptors ordered by qualified tool name. Tool
descriptor hashes use the same canonical JSON and SHA-256 rule. Re-publishing
identical source under identical tool descriptors for the same workflow name
returns the existing definition. A source or descriptor change creates the
next version.

A definition is reusable by another session only when that session's
principal-scoped visible catalog contains every qualified tool with the exact
captured descriptor hash. A run stores its own immutable tool bindings. A
mismatch rejects run creation as a `_kiteframe` validation error; it does not
reveal other tools, silently substitute a contract, or reuse another
principal's catalog.

On restart or ACP session resume, the harness reconnects affected MCP servers
with freshly supplied credentials and rebuilds catalog epochs. Active runs
advance only after every bound tool is visible with the exact captured
descriptor. A removal or digest change fails the affected call closed rather
than making global readiness fail for unrelated sessions and runs.

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
queued | running | waiting_for_answer | waiting_for_permission |
outcome_unknown | succeeded | failed | cancelled
```

A task frame has one of these states:

```text
runnable | waiting_for_model | waiting_for_tool |
waiting_for_children | waiting_for_answer | waiting_for_permission |
outcome_unknown | completed | failed | cancelled
```

Only persisted state may make work runnable. In-memory futures are execution
mechanisms, not state.

Parallel branches may independently reach `ask` or a permission request, so a
run can have multiple open suspensions. The run remains `running` while any
frame can still advance. When no executable work remains, it becomes
`waiting_for_answer` if only human answers are pending or
`waiting_for_permission` if any permission is pending. `_kiteframe/run/get`
returns every open suspension without exposing policy or tool content to a
different session.

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

The engine consumes one internal `BoundTool` interface implemented by either a
trusted in-process tool or a session/principal-scoped MCP adapter. Each bound
tool exposes three conceptual operations:

```text
descriptor() -> ToolDescriptor
invoke(ToolCall) -> ToolResult
status(IdempotencyKey) -> ToolStatus
```

`ToolDescriptor` contains the qualified stable name, visible MCP descriptor or
in-process schema, trusted capability tags, effect classification, and recovery
support. `ToolCall` includes session, run, frame, policy revision, validated
arguments, and idempotency key. `ToolResult` and `ToolStatus` are typed and
validated before entering model context.

MCP does not standardize effect recovery. An MCP binding therefore needs a
trusted session/deployment recovery profile that classifies the tool as
read-only, idempotent, status-recoverable, or unsafe-to-retry. Server-provided
metadata is a hint only. An effectful tool without idempotent invocation or a
declared status operation cannot be referenced by a published workflow.

An MCP invocation uses the same authenticated server binding that produced the
principal-visible catalog. Kiteframe evaluates Cedar immediately before
`tools/call`; the MCP server then performs its own authorization. The model has
no generic filesystem, shell, HTTP, code execution, or unrestricted MCP
surface. It sees only exact tools surviving discovery, workflow binding, and
session policy.

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

PostgreSQL is the only V1 workflow authority. The schema has eleven logical
tables:

| Table | Durable responsibility |
|---|---|
| `sessions` | ACP identity, validated principal binding, lifecycle state, working-directory metadata, current catalog and policy revisions, and timestamps. |
| `session_policy_rules` | Immutable typed rules, their compiled Cedar form, caller idempotency key, policy revision, source, and decision metadata. |
| `mcp_catalog_epochs` | Per-session, per-server visible descriptor snapshot, canonical digest, trusted capability mapping, and non-secret connection-binding identity. |
| `workflow_definitions` | Immutable source, compiled form, name, version, definition hash, and tool descriptor hashes. |
| `runs` | Session and definition references, typed input, current state, revision, limits, output or failure, and timestamps. |
| `run_tool_bindings` | Immutable qualified tool, server, catalog epoch, descriptor digest, capabilities, and recovery profile selected for a run. |
| `frames` | Task identity, parent and join identity, bindings, state, result, suspension, and revision. |
| `model_items` | Complete ordered Responses items and attempt identity for each frame. |
| `tool_calls` | Validated arguments, idempotency identity, state, result, error, and recovery metadata. |
| `permission_requests` | Pending invocation, requested tool and input digest, policy revision, offered choices, idempotent resolution, and resulting rule reference. |
| `events` | Append-only, per-run ordered operational history. |

Join and human-answer suspension state live on `frames`; permission requests
are separate because their resolution may append a durable session policy
rule. Constraints MUST enforce:

- immutable session principal binding;
- unique policy rule ID and update idempotency key within a session;
- monotonic session policy revision;
- catalog epochs scoped to one session and authenticated server binding;
- unique canonical catalog digest per session, server, and refresh identity;
- unique workflow name and version;
- idempotent same-source publication;
- immutable definition contents;
- one run per workflow-scoped start idempotency key;
- immutable run-to-session and run-to-tool bindings;
- one ordered item position per frame;
- one tool call per stable idempotency key;
- one resolution per human suspension;
- one idempotent resolution per permission request;
- monotonic frame and run revisions; and
- monotonic event sequence per run.

Current-state rows are the execution source of truth. Events are for inspection,
streaming, and audit; the engine does not reconstruct state by replaying the
entire event log.

Startup recovery changes interrupted model attempts back to runnable. It
recovers interrupted tool attempts through their declared status/idempotency
contract. It reconstructs runnable joins, questions, permission requests, and
frames solely from committed PostgreSQL state. MCP-dependent work remains
blocked after restart until a resumed client supplies fresh connection
configuration, principal-visible catalogs are rebuilt, and every immutable run
binding is verified against the new catalog epoch.

S3 MAY later store large immutable attachments or archives, but it MUST NOT
decide V1 workflow state or scheduling.

## Limits and failure model

### Limits

Deployment configuration defines ceilings for:

- source and compiled-definition size;
- sessions and active runs per process;
- MCP servers per session, catalog pages, visible tools, descriptor bytes, and
  total catalog bytes;
- typed policy rule count, policy bytes, Cedar evaluation time, and unresolved
  permission requests;
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
| `tool_contract_changed` | A bound MCP tool disappeared or its visible descriptor no longer matches the immutable run binding. |
| `outcome_unknown` | An external effect may have occurred but cannot be resolved safely. |
| `limit_exceeded` | A deployment or workflow budget was exhausted. |
| `cancelled` | A client cancelled the run. |
| `internal` | Kiteframe violated an invariant or encountered an unclassified defect. |

`outcome_unknown` is both a stable condition and a suspended run state. It is
never converted automatically into `tool_failed` or success.

Transient model and tool transport errors use capped exponential backoff with
jitter. Retry counts and delays are deployment configuration, not DSL syntax.
Author-defined catch blocks and retry policies are excluded from V1.

## ACP application surface

ACP provides initialization, authentication negotiation, session lifecycle,
MCP server attachment, permission requests, and transport-independent JSON-RPC
framing. Kiteframe adds only workflow-specific methods:

| Method | Semantics |
|---|---|
| `_kiteframe/workflow/publish` | Compile and atomically publish source against the calling session's current visible catalog. |
| `_kiteframe/run/start` | Start a selected immutable definition with typed input and a client-generated idempotency key. |
| `_kiteframe/run/get` | Return current state, definition identity, open questions and permissions, result, or stable failure for a run owned by the session. |
| `_kiteframe/run/answer` | Resolve one human question with a typed answer and idempotency key. |
| `_kiteframe/run/cancel` | Cooperatively cancel one run with an idempotency key and optional reason. |
| `_kiteframe/session/policy/update` | Validate and append typed rules using an idempotency key and expected policy revision. |
| `_kiteframe/run/event` | Notify the client of one durable run event with an exclusive sequence cursor. |

With no requested workflow version, `_kiteframe/run/start` selects the latest
successfully published version in the start transaction. Repeating an
idempotency key with the same normalized request returns the original result;
reusing it with different content returns a conflict. Answer, permission,
policy-update, and cancellation operations follow the same rule.

Each ACP version adapter maps its session and permission wire types to the same
version-neutral application commands. `_kiteframe/run/event` carries the
durable sequence number; after reconnect or `session/resume`, the client
supplies its last observed sequence and receives missing persisted events
before live notifications. Event correctness never depends on a transport
connection remaining open.

Remote deployments expose ACP on `/acp` using WebSocket upgrade or Streamable
HTTP and expose only `GET /healthz` and `GET /readyz` outside ACP. There is no
parallel REST control API or product-specific SSE endpoint in V1.

For local `stdio`, the thin client creates the process and the session principal
from local authenticated client context. Remote ACP requires deployment-owned
TLS and caller authentication; the harness derives the session principal from
validated connection identity, never workflow input, tool arguments, an MCP
descriptor, or a caller-supplied principal field. V1 deliberately excludes
user-directory, tenant-relationship, and deployment authorization products,
but it does not permit anonymous remote sessions.

## Observability

V1 provides:

- ordered run events;
- structured JSON service logs;
- stable session, catalog-epoch, policy-revision, permission-request, run,
  frame, model-attempt, and tool-call correlation identities; and
- liveness and database readiness endpoints plus per-session MCP binding status.

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
- old Python runtime packages and PyO3 projections, replaced by a thin Python
  ACP client that bundles the harness;
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
and new ACP application surface. The TypeScript client is new; neither thin
client preserves an old runtime contract.

## Verification strategy

### Compiler tests

- Golden parse and compiled-definition fixtures for valid workflows.
- One focused rejection fixture for every compiler diagnostic class.
- Type, reachability, recursive-call, binding, outcome, and parallel-join
  adversarial tests.
- Deterministic definition hash and same-definition publication tests.
- Principal-visible tool resolution, exact descriptor binding, and catalog
  mismatch rejection tests.

### Engine tests

- State-machine tests with fake `ModelClient`, `BoundTool`, MCP catalog, and
  Cedar authorizer implementations.
- Ordinary tool loops and typed `complete_task` validation.
- `call`, `goto`, `done`, `fail`, and bounded-cycle behavior.
- Parallel completion in every ordering, failure propagation, and cancellation.
- Typed `ask`, duplicate answer, conflicting answer, and resume tests.
- Default-deny permission waits, explicit-forbid tool results, idempotent
  decisions, and policy-revision race tests.

### Persistence and crash tests

- PostgreSQL transaction tests for every uniqueness and revision constraint.
- Restart after intent commit and before result commit for model and tool calls.
- Idempotent tool retry, status recovery, and `outcome_unknown` behavior.
- Restart while children run, while a join waits, and while a run awaits a
  human answer or permission.
- Session, policy-rule, catalog-epoch, and permission-resolution constraint
  tests.
- ACP event-cursor resume without gaps or duplicate state transitions.

### ACP and transport conformance

- ACP v1 initialize, `session/new`, `session/load`, tool-call permission, and
  connection-teardown fixtures.
- ACP v2 initialize, required session lifecycle, generic permission, resume,
  and close fixtures.
- Extension version negotiation and rejection of incompatible client/harness
  combinations.
- Identical application behavior over `stdio`, WebSocket, and Streamable HTTP.
- WebSocket-to-HTTP fallback only on transport or upgrade incompatibility.
- Reconnect and session resume with fresh MCP connection configuration and a
  durable event cursor.

### MCP and Cedar policy tests

- Two principals receiving different paginated `tools/list` results without
  catalog, descriptor, event, or error leakage.
- Catalog additions, removals, descriptor changes, and
  `notifications/tools/list_changed` handling.
- Generated Cedar schema validation against the exact visible catalog.
- Trusted capability mappings, `mcp_unknown`, exact-tool rules, input
  constraints, default deny, and forbid-overrides-permit behavior.
- Proof that Cedar cannot widen workflow bindings and that every permitted MCP
  call is still authorized by its server.

### Thin-client and binary packaging tests

- Python wheels and npm platform packages contain the expected version-matched
  executable for every supported target.
- Python and TypeScript local clients launch the bundled binary, negotiate ACP,
  propagate shutdown, and surface startup diagnostics.
- Remote clients prefer WebSocket, fall back to Streamable HTTP under the
  defined conditions, and never fall back on authentication or application
  errors.

### Provider conformance

A fake Responses server verifies exact outbound requests, `store: false`, full
ordered item replay, unknown-item preservation, tool schemas, function call
outputs, refusals, timeouts, malformed responses, and retry classification.

Real-provider smoke tests are optional deployment checks, not deterministic CI
gates and not a substitute for protocol conformance tests.

## V1 acceptance scenario

V1 is complete only when one PostgreSQL-backed acceptance suite proves all of
the following:

1. A Python application installed with `uv add kiteframe` launches the bundled
   harness over `stdio`, negotiates ACP, and creates a durable session.
2. A TypeScript application installed with `bun add kiteframe` and importing
   `kiteframe/client` connects remotely over WebSocket; a second test forces
   only the valid Streamable HTTP fallback condition.
3. Two authenticated principals see different MCP catalogs, and neither can
   infer the other's hidden tool names, descriptions, or schemas.
4. An author publishes the example-style workflow through
   `_kiteframe/workflow/publish`; publication binds exact visible tool
   descriptors and a caller starts a run with typed input.
5. The engine executes persisted model and ordinary tool turns only after the
   declared workflow binding and current Cedar policy both allow them.
6. A default-denied call creates a durable permission request; an idempotent
   typed decision updates the session policy and resumes the frame, while an
   explicit forbid returns a denial without prompting.
7. The workflow starts bounded parallel task calls and joins their typed
   results in source order.
8. The workflow durably asks for human input and resumes idempotently.
9. Forced process restarts during model, tool, parallel, catalog refresh, human
   question, and permission states do not corrupt or double-advance the run.
10. ACP v1 and gated v2 clients observe the same ordered durable events and
    receive the same typed final result.
11. Removal or descriptor mutation of a bound tool prevents future invocation
    and produces `tool_contract_changed` without mutating the run contract.
12. An uncertain external effect produces `outcome_unknown`, never guessed
   success or an unsafe blind retry.

Passing unit tests without this PostgreSQL-backed scenario is insufficient.

## Deferred ideas

The following may be considered only after V1 evidence shows a need:

- external prompt assets;
- transcript compaction or author-defined context projection;
- multiple model profiles;
- distributed service replicas and workers;
- artifact storage in S3;
- in-flight steering, including queue ordering, inference interruption,
  durability, and cross-transport semantics, in a separate design and plan;
- workspace, deployment, organization, and multi-tenant policy scopes;
- OpenFGA or another relationship-authorization service;
- richer Cedar conditions and public raw-Cedar authoring;
- ACP v2-only operation after the protocol becomes stable and ecosystem support
  makes v1 compatibility unnecessary;
- OpenTelemetry or Prometheus export;
- author-defined retry/catch/compensation behavior;
- workflow visualization; and
- provider-specific Responses extensions.

They are not roadmap promises and MUST NOT shape the V1 abstractions in
advance.

## Implementation-planning boundary

After approval, this specification defines the replacement contract. Its
implementation plan should deliver the replacement in cumulative,
independently reviewable slices rather than recreating the previous large
speculative wave structure.
The minimal workflow engine, ACP harness, clients, principal-scoped MCP
catalogs, and session capability policy belong in one revised V1 plan because
they form one invocation authority boundary. Steering remains a separate
feature and MUST have a separate design and implementation plan.

## Current external contracts

This design is grounded in the following current upstream material:

- [Kiro: One Agent, Everywhere](https://kiro.dev/blog/one-agent/) for the
  single-harness, thin-client, ACP, transport, client-tool, and steering model;
- [ACP protocol](https://agentclientprotocol.com/) and the
  [ACP v2 migration guide](https://agentclientprotocol.com/protocol/v2/migration)
  for negotiated v1/v2 wire contracts;
- the official Rust crates
  [`agent-client-protocol`](https://crates.io/crates/agent-client-protocol) and
  [`agent-client-protocol-http`](https://crates.io/crates/agent-client-protocol-http);
- the [MCP tools specification](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)
  and [MCP authorization specification](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization)
  for principal-scoped discovery and server-side authorization; and
- [`cedar-for-agents`](https://github.com/cedar-policy/cedar-for-agents/tree/main)
  and its [Rust workspace](https://github.com/cedar-policy/cedar-for-agents/tree/main/rust)
  for agent policy patterns and MCP-to-Cedar schema generation.
