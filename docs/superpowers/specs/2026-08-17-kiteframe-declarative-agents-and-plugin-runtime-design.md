# Kiteframe Declarative Agents and Plugin Runtime Design

- **Status:** Proposed; awaiting written-spec review
- **Date:** 2026-08-17
- **Repository:** `swiftdiaries/kiteframe`
- **Scope:** Declarative agents, durable goals, replaceable runtime and harness
  plugins, extensible event channels, and a Cursor/VS Code PR-babysitting
  vertical slice
- **Foundation:** The implemented minimal workflow engine on `main`

## Executive summary

Kiteframe evolves from a durable workflow harness into a small protocol kernel
for durable declarative agents. The existing Rust engine, PostgreSQL authority,
`.kite` compiler, ACP transports, MCP tool discovery, Cedar authorization, and
thin clients remain the foundation. This design does not replace that engine.

The agent language follows Firetiger's NXL model: an agent is a structured
graph of focused tasks. Each task has a narrow prompt, typed inputs and returns,
an explicit tool set, and declared transition edges. Call, structured
fork/join, and tail-return traversal make reusable multi-stage agents possible
without hiding control flow inside a monolithic prompt. Task edges also control
which history crosses a context boundary.

A user task carries an immutable `GoalContract`: a durable, evidence-checked
definition of what completion means, which constraints must remain true, and
what constitutes an honest blocker. The agent graph defines how to pursue that
goal. A coding harness receives only a narrower derived subgoal. A Codex,
Claude Code, or DeepSeek Harness goal verdict is invocation evidence, never
authority to complete the enclosing Kiteframe goal.

The Rust kernel owns canonical task and goal state, the durable command journal,
plugin routing, policy, budgets, idempotency, recovery, and audit. Four
replaceable plugin families surround it:

1. input plugins for IDE, chat, desktop, and webhook surfaces;
2. agent-runtime plugins for `.kite` or a future declarative runtime;
3. harness plugins for the built-in loop or an ACP coding harness; and
4. connector/control-plane plugins for channels, context, authentication,
   secrets, and bounded provider actions.

The first product slice is a single TypeScript extension for Cursor and VS
Code. A terminal-averse engineer selects **Babysit Current PR**. Kiteframe
durably watches GitHub review and required-check events, prepares bounded
repair work, asks for approval, and invokes Codex locally through ACP. The goal
completes when actionable reviews are resolved, required GitHub Actions checks
pass, and GitHub reports the pull request merged. V1 does not auto-merge or
babysit deployment after merge.

The event and channel contracts are provider-neutral from the beginning. V1
implements GitHub only, but a synthetic `service.slo.burn_rate_exceeded.v1`
conformance event proves that later service-monitoring agents can use the same
ingress, correlation, context, diagnosis, repair, and PR-handoff boundaries.

## Status and relationship to existing designs

The minimal workflow engine is implemented and remains the current execution
foundation. This document supersedes only the product boundaries that excluded
long-lived agents, external event channels, replaceable harnesses, and an IDE
worker path. It does not invalidate the minimal engine's durability, typed DSL,
ACP, MCP, Cedar, effect-recovery, or thin-client contracts unless this document
explicitly changes them.

Historical package-first, PyO3, Deep Agents, and model-authored graph designs
remain superseded. NXL is a design reference, not a syntax, runtime, or wire
compatibility target.

This document is a design artifact. It is not evidence that the plugin
architecture, agent language, IDE extension, or `babysit_pr` product has been
implemented.

## Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**,
and **MAY** are normative. Examples illustrate the design but do not override
the contracts in this document.

## Product goals

V1 has twelve goals:

1. Extend the NXL-inspired task language into complete declarative agent
   definitions without becoming a general-purpose programming language.
2. Give every user task an immutable, durable, evidence-checked completion
   contract.
3. Keep the Rust kernel authoritative for durable state, policy, budgets,
   routing, idempotency, recovery, and audit.
4. Make the existing `.kite` runtime replaceable behind a stable agent-runtime
   transition contract.
5. Make the existing model loop replaceable behind a stable harness contract.
6. Run Codex locally through a provider-neutral ACP adapter while preserving
   the existing built-in harness.
7. Normalize input and provider events through versioned semantic envelopes
   that do not expose channel-specific workflow semantics.
8. Provide an official Python connector/control-plane host for GitHub ingress,
   context construction, auth flows, secret references, and bounded provider
   actions.
9. Provide one TypeScript extension codebase for Cursor and VS Code with
   structured tasks, status, evidence, and approval interactions.
10. Enforce hierarchical structural, consumption, and exact-spend budgets at
    the harness boundary.
11. Keep source, Git credentials, and existing harness authentication on the
    developer machine for the interactive V1 path.
12. Deliver one narrow vertical slice that babysits a GitHub pull request until
    it is merged.

## V1 product boundary

### Target user

The primary V1 user is an IDE engineer who is terminal-averse but already has
access to an organization-approved coding harness subscription or enterprise
model gateway. The extension is the product interface. Users SHOULD NOT need
to paste repository identifiers, edit MCP configuration, or invoke terminal
commands for the normal PR-babysitting path.

### First vertical slice

V1 supports:

- GitHub pull requests;
- unresolved actionable review threads;
- required GitHub Actions checks;
- a local interactive Codex worker over ACP;
- approval before repository mutation;
- repeated repair and review cycles; and
- completion when GitHub reports the pull request merged.

### Non-goals

V1 does not provide:

- service-monitoring or SLO-setup agents;
- Honeycomb, Sentry, Prometheus, Langfuse, deployment, or GitOps connectors;
- unattended or server-side coding execution;
- agent-to-agent successor execution;
- post-merge deployment babysitting;
- Claude Code or DeepSeek Harness production certification;
- Slack, generic chat, or desktop frontends;
- dynamic plugin installation, a marketplace, or third-party plugin sandbox;
- in-process Python or TypeScript plugin loading;
- a generic executable goal-predicate language;
- mutation of an accepted goal contract;
- automatic merging, required-check bypass, or approval bypass;
- arbitrary model-authored graphs or user-supplied code; or
- identical SLO thresholds across services.

These exclusions are not implied future commitments. The design includes only
the seams and conformance evidence needed to avoid blocking a later
service-monitoring product.

## Design principles

### Goals define completion; agents define pursuit

A goal states what must be true, how completion is evidenced, which constraints
must remain true, and when the task is honestly blocked. The agent graph owns
the legal path toward that goal. A task prompt or harness goal MUST NOT silently
replace the user-authorized completion contract.

### Tasks are graph states, not prompt sections

Each task changes the active prompt, tools, typed inputs and returns, and
history presentation. Permission and attention boundaries are runtime
structure, not warnings embedded in a growing prompt.

### Plugins propose; the kernel commits

Plugins can supply authenticated events, context artifacts, runtime
transitions, harness progress, and action results. Only the Rust kernel can
authorize and durably commit a task, goal, budget, or effect transition.

### Channels carry events, not workflow semantics

A channel authenticates and transports a signal. A versioned semantic event
describes what happened. A deployment binding selects the agent entrypoint.
The `.kite` source never depends on Slack, webhooks, Honeycomb, or another
transport name.

### Persist before dispatch

Every outbound command and external-effect intent is durable before dispatch.
At-least-once delivery is paired with idempotency, compare-and-set revisions,
leases, and fencing.

### Honest external effects

Database state can be exactly once; arbitrary GitHub, observability, and
repository mutations cannot. An uncertain external outcome suspends visibly
and is reconciled before any retry.

### One V1 product, future-safe seams

V1 ships one PR-babysitting path, one provider family, one verified external
harness, and one extension codebase. Future provider examples appear only as
protocol or conformance fixtures.

## System architecture

```text
Input plugin
Cursor / VS Code / future chat or webhook
                    |
                    v
Kiteframe Rust kernel
  task + goal authority
  durable journal + routing
  policy + budgets + audit
        |                 |                  |
        v                 v                  v
Agent-runtime plugin  Harness plugin   Connector/control-plane plugin
.kite runtime         built-in loop    channels + context + actions
future runtime        ACP / Codex      Python integration host
```

### Rust kernel responsibilities

The Rust kernel owns:

- canonical `UserTask`, `GoalContract`, and task-event contracts;
- durable task, goal, command, work-item, invocation, budget, and event state;
- plugin registration, capability negotiation, lifecycle, and routing;
- authenticated principal and deployment-policy enforcement;
- hierarchical budget reservation and settlement;
- approval and cancellation authority;
- idempotency, compare-and-set revisions, leases, and fencing;
- stable failures and `outcome_unknown` suspension;
- recovery and ordered audit events; and
- the decision whether a proposed command may change durable state.

The kernel MUST NOT own provider-specific OAuth flows, GitHub payload parsing,
IDE presentation, or the internal reasoning strategy of an alternative
declarative runtime.

### Plugin deployment model

V1 includes the `.kite` agent runtime and direct model loop as built-in Rust
implementations behind the same logical contracts used by external plugins.
The official Python connector host and TypeScript input/worker are
out-of-process. Alternative agent runtimes and external harnesses communicate
through versioned protocol messages.

Plugins are registered by deployment configuration. V1 has no runtime package
installer, discovery marketplace, stable Rust ABI, or in-process Python or
TypeScript loader. Plugins MUST NOT call each other directly or mutate
kernel-owned PostgreSQL tables.

## Canonical user task and goal contract

Every input plugin normalizes a request into the same envelope:

```yaml
task_id: task_123
agent:
  name: babysit_pr
  version: 1
principal: user_456

target:
  type: github_pull_request
  repository: acme/payments
  number: 142

preferences:
  harness_role: coding

goal:
  objective: Address actionable review feedback and see the PR through merge.
  success:
    - id: reviews_resolved
      statement: All actionable review threads are resolved.
      evidence: github.unresolved_reviews
    - id: checks_passed
      statement: Required GitHub Actions checks pass.
      evidence: github.required_checks
    - id: pull_request_merged
      statement: The pull request reaches merged state.
      evidence: github.pull_request_state
  constraints:
    - Never merge the pull request.
    - Never bypass required checks.
    - Repository mutation requires approval.
  blocked_when:
    - Required user or reviewer action remains unavailable.
    - No policy-permitted repair path remains.
```

The canonical `GoalContract` contains:

- one non-empty objective;
- one or more stable, named success criteria;
- the evidence type required for each criterion;
- zero or more constraints that must remain true;
- zero or more blocked conditions; and
- the contract version.

The goal contract is immutable after task acceptance. A user can pause, resume,
answer a question, or cancel the task. Changing the objective creates a new
successor task in a future agent-to-agent handoff feature; V1 users cancel and
start a new task.

The goal belongs to one task/run. It is not global memory, project
instructions, or a mutable prompt fragment.

## Goal lifecycle and completion authority

The durable goal phases are:

```text
active ----> paused ----> active
   |
   +-------> blocked ---> active
   |
   +-------> complete
```

Their semantics are:

| Phase | Meaning |
|---|---|
| `active` | The objective remains live, including while execution waits. |
| `paused` | An authorized user or policy deliberately stopped progress. |
| `blocked` | No safe, permitted path remains without a named external change. |
| `complete` | Every success criterion has authorized supporting evidence; terminal and immutable. |

Operational waits do not become goal phases. An active goal can be
`runnable`, `waiting_for_event`, `waiting_for_worker`,
`waiting_for_approval`, `waiting_for_plugin`, or `waiting_for_budget`.
Budget exhaustion is neither completion nor failure.

Cancellation terminates the containing task/run and records a cancellation
event. It MUST NOT masquerade as goal completion.

The agent-runtime plugin can propose:

```text
CompleteGoal(goal_revision, criterion_evidence)
BlockGoal(goal_revision, reason_code, explanation,
          evidence_refs, unlock_conditions)
```

The kernel accepts these commands only when:

- the goal identity and revision are current;
- the agent definition permits the transition;
- every referenced artifact exists and belongs to the task;
- every success criterion has an assertion and required evidence reference;
- each assertion source is authorized for its declared evidence type; and
- current policy permits the transition.

The kernel validates structural completeness, provenance, and authority. The
`.kite` runtime evaluates typed connector fields and graph conditions. V1 does
not add a generic predicate language or a generic LLM completion evaluator.

A harness-native goal is a bounded implementation detail of one invocation.
Its completion verdict becomes an invocation result. It cannot directly
complete, block, pause, or cancel the enclosing Kiteframe goal.

## Declarative agent language

### NXL-shaped agent graph

A `.kite` source defines exactly one agent with an entry task and tasks nested
inside the agent:

```kite
agent babysit_pr {
  entry inspect_pr

  input {
    repository: string
    pull_request: integer
  }

  events {
    github.pull_request.changed.v1
    github.review.submitted.v1
    github.required_checks.changed.v1
  }

  limits {
    steps: 200
    model_turns_per_task: 12
    parallel_branches: 4
    active_execution_seconds: 300
    elapsed_seconds: 604800
  }

  task inspect_pr {
    model reasoning

    tools {
      github_context
      task[context=kept]:repair_reviews
    }

    prompt {
      Inspect the pull request and determine its next required action.
    }

    returns {
      task[context=kept]:repair_reviews
      | wait github.pull_request.changed.v1 -> return:inspect_pr
      | output {
          merged_commit: string
        }
    }
  }

  task repair_reviews {
    model coding

    input {
      review_ids: [string]
    }

    prompt {
      Address only the supplied actionable review threads and verify the
      affected behavior.
    }

    returns {
      output {
        commit: string
        tests: [string]
      }
      | ask {
          question: string
        } -> return:repair_reviews
    }
  }
}
```

The example fixes semantics, not final punctuation. The implementation plan
MUST include a focused grammar review before parser work.

### Agent, task, goal, and harness distinctions

| Concept | Responsibility |
|---|---|
| Goal | User-authorized completion contract for one run. |
| Agent | Immutable versioned graph that durably pursues a goal. |
| Task | Focused graph state with prompt, tools, typed input and return edges. |
| Harness invocation | One bounded piece of model-driven work for a task. |

An agent is not a larger task. It owns durable traversal, waits, event
correlation, goal progress, and task transitions. Tasks remain small reasoning
states with isolated authority.

### Task traversal

Kiteframe adopts three NXL-style calling conventions:

| Convention | Semantics |
|---|---|
| `call` | Enter a child task, wait for its typed result, then return to the caller. |
| `fork` / `join` | Start bounded concurrent children under one structured join; no child outlives the join. |
| `return` | Tail-transition permanently to another task without growing the stack. |

Kiteframe intentionally does not expose an opaque background token for a model
to poll. Long-lived external waiting uses the deterministic event primitive;
concurrent task work remains structured.

### Context presentation

Each task edge declares how history crosses the boundary:

| Mode | V1 semantics |
|---|---|
| `isolated` | Default. The child receives typed inputs, projected goal fields, declared artifacts, and no caller transcript. |
| `kept` | The child receives the relevant ordered current-task history in addition to typed inputs. |

`summarized` is excluded from V1 because it introduces a model-mediated
transformation with separate evidence, budget, privacy, and failure semantics.

A task receives only goal fields and evidence explicitly projected by the agent
definition. Sibling forks share no mutable transcript or memory.

`active_execution_seconds` bounds time spent executing model, tool, connector,
and transition work. `elapsed_seconds` bounds the complete wall-clock lifetime
of an agent run, including durable waits. Time waiting for an IDE worker,
approval, event, plugin, or budget counts only toward `elapsed_seconds`.

### Semantic harness roles

`model reasoning` and `model coding` name semantic execution roles. Deployment
policy binds those roles to the built-in harness, Codex ACP, or another
compatible harness profile. `.kite` source MUST NOT name provider credentials,
API endpoints, subscription identities, or raw model names.

### Durable event wait

An agent can suspend on a declared semantic event without consuming model
budget:

```kite
wait github.pull_request.changed.v1(
  repository: repository,
  pull_request: pull_request,
  after: cursor
)
```

The kernel persists the wait and correlation keys before acknowledging the
registration. Event delivery resumes the exact agent version and frame
idempotently. Webhook delivery and limited polling repair are connector
implementation details.

### Publication and versioning

Publishing produces an immutable, monotonically versioned agent definition.
Existing runs remain pinned to their definition and plugin-protocol versions.
New starts select the latest permitted published version unless the caller
requests an exact version.

V1 continues to accept existing `workflow` sources with their current feature
set and emits a deprecation diagnostic. `workflow` is not a literal alias for
`agent`: it cannot declare durable goals, semantic events, or plugin roles.
Removal is outside V1 and requires a separate breaking-change design. New
durable-goal and event behavior is authored through `agent`.

## Agent-runtime transition contract

The agent runtime behaves as a state machine:

```text
ValidateAgent(definition) -> ValidatedAgent
Start(task, definition) -> Transition
Handle(runtime_state, event) -> Transition
```

A transition contains a complete new versioned runtime-state snapshot and zero
or more kernel commands. V1 commands are:

| Command | Purpose |
|---|---|
| `RequestContext` | Resolve named evidence through an authorized connector. |
| `WaitForEvent` | Persist a semantic subscription and suspend. |
| `InvokeHarness` | Request one bounded model-driven invocation. |
| `RequestApproval` | Ask an authorized principal for a typed decision. |
| `CompleteGoal` | Propose evidence-backed goal completion. |
| `BlockGoal` | Propose an honest blocker and unlock conditions. |
| `FailTask` | Terminate with a stable failure. |

The Rust kernel validates the expected task and goal revisions, command schema,
plugin capability, policy, budget, evidence ownership, and legal durable phase
before committing a transition. An external runtime's opaque internal state
does not grant authority to bypass those checks.

Agent-to-agent successor creation is a future kernel command. A service
diagnosis plugin or agent MUST NOT call `babysit_pr` directly.

## Stable plugin protocol

### Common message envelope

Every plugin message includes:

```text
protocol_version
payload_schema_version
message_id
task_id
causation_id
correlation_id
expected_revision
idempotency_key
```

Payload schemas are closed and versioned. Unsupported protocol or schema
versions fail closed. Reusing an idempotency key with different normalized
content is a conflict.

Commands are committed before dispatch. Results are accepted idempotently
against the expected revision. Plugin-local memory and queues are never
authoritative.

### Harness plugin

The harness contract is:

```text
DescribeCapabilities
StartInvocation
CancelInvocation
InvocationEvent
```

The kernel sends a bounded invocation containing:

```yaml
invocation_id: invocation_321
task_id: task_123
goal:
  objective: Address review threads 17 and 23 and verify the affected tests.
  parent_goal_revision: 1
context_artifacts:
  - context_789
workspace:
  repository: acme/payments
  revision: abc123
permissions:
  - workspace.read
  - workspace.write
  - terminal.test
budget_grant:
  metric: model_turns
  limit: 8
```

Harness events normalize session start, progress, approval requests, files
changed, tests completed, completion, failure, cancellation, and uncertain
outcomes.

The existing direct Responses-compatible loop is the built-in harness plugin.
The generic ACP adapter is the first external harness plugin. Codex is the only
V1 externally verified harness. Claude Code and DeepSeek Harness MAY use the
same adapter only after passing the same capability and conformance suite; V1
does not claim their production support.

### Connector/control-plane plugin

The official Python host exposes three narrower contracts:

| Contract | Responsibility |
|---|---|
| Channel | Authenticate ingress and normalize provider payloads into semantic events. |
| Context | Resolve immutable, provenance-bearing evidence artifacts. |
| Action | Perform one declared, policy-authorized provider mutation. |

Provider actions include creating an SLO monitor or opening a pull request.
They pass through the same kernel authorization, approval, idempotency, budget,
and recovery boundaries as other effects. The Python service cannot execute
undeclared mutations autonomously.

The Python host also handles provider OAuth, webhook endpoints, connection
registration, secret-manager integration, and bounded context construction. It
does not own goals, workflow transitions, budgets, routing, or durable task
state.

### Input plugin

IDE, chat, desktop, and future Slack implementations use:

```text
SubmitTask
SubscribeToTaskEvents
AnswerApproval
PauseTask
ResumeTask
CancelTask
```

Input plugins present state and collect authenticated intent. They cannot
construct agent-runtime state, complete a goal, or bypass a pending approval.

## Semantic channels and events

### Channel, event, binding, and context

The four concepts are separate:

- a **channel** authenticates and transports an external signal;
- an **event** describes the provider-neutral fact that occurred;
- a **binding** maps an event type to an agent entrypoint; and
- a **context source** resolves detailed evidence after an agent is triggered.

### Canonical event envelope

```yaml
event_id: evt_123
type: service.slo.burn_rate_exceeded.v1
subject:
  service_id: checkout-api
  environment: production
occurred_at: 2026-08-17T12:30:00Z
observed_at: 2026-08-17T12:30:03Z
severity: critical
correlation:
  incident_fingerprint: latency-checkout-production
classification: restricted
data:
  slo_id: checkout-availability
  window: 1h
  observed_burn_rate: 14.2
artifacts:
  - honeycomb-query-result-789
```

The envelope contains stable semantic routing and correlation fields. Detailed
provider payloads remain in immutable artifacts. This avoids exposing raw
vendor schemas to every agent without forcing all observability evidence into
one lossy universal payload.

Event definitions are deployment-owned, versioned schema objects registered
with the kernel. Each definition fixes the closed subject, correlation, and
data schemas plus allowed artifact types and size limits. Agent publication
fails if it references an unregistered or incompatible event definition. A
definition is immutable within one version; incompatible change requires a new
event-type version.

### Delivery and correlation

Channel delivery is at least once. The kernel:

1. authenticates the channel binding;
2. validates the event schema and size;
3. deduplicates by channel binding and provider event ID;
4. stores the normalized payload digest;
5. rejects reuse of the ID with different content;
6. persists the event before acknowledgement;
7. correlates it to an existing task or a declared start binding; and
8. advances at most one expected task revision per accepted delivery.

Malformed, unauthorized, or schema-invalid events are retained for bounded
inspection but cannot advance an agent.

### Deployment bindings

Channel names remain outside `.kite` source:

```yaml
bindings:
  - event: service.slo.burn_rate_exceeded.v1
    agent: diagnose_service
    correlate_by:
      - subject.service_id
      - subject.environment
      - correlation.incident_fingerprint
```

Repeated alerts with the same correlation identity resume the existing
incident task. Resolution events can wake the same task.

### Future service-monitor setup

A later `setup_service_monitoring` agent will create an approved
`ServiceProfile`:

```yaml
service_id: checkout-api
owners:
  - payments-platform
repositories:
  application: acme/checkout
  deployment: acme/service-deployments
environments:
  - production
slos:
  - id: checkout-availability
    indicator: successful_request_ratio
    objective: 99.9
    window_days: 30
    alert_policy: multi_window_burn_rate
```

That future setup workflow will discover candidate SLIs, propose SLOs and burn
thresholds, obtain service-owner approval, create provider-specific monitors,
store opaque monitor bindings, and verify a test event. Portable profiles hold
semantic intent; provider queries and credentials stay in connector
configuration.

The later incident flow is:

```text
provider alert
  -> semantic channel event
  -> durable diagnose_service goal
  -> logs, metrics, traces, deployments, and recent-change context
  -> approved repair in application or deployment repository
  -> repair pull request
  -> successor babysit_pr goal
```

V1 implements none of this product flow. It includes a synthetic
`service.slo.burn_rate_exceeded.v1` fixture only to prove schema registration,
authentication, deduplication, correlation, and generic dispatch.

## `babysit_pr` data flow

The V1 task proceeds as follows:

1. The extension derives the repository, branch, and pull request from the
   workspace and submits the canonical user task.
2. The kernel validates principal, agent version, goal contract, policy,
   harness profile, and limits before atomically accepting the task.
3. The `.kite` runtime requests `github.pull_request`,
   `github.unresolved_reviews`, and `github.required_checks` context.
4. The Python connector authenticates to GitHub and returns immutable context
   artifacts with source, collection time, expiry, classification, schema
   version, and content digest.
5. The agent either completes with current merge evidence, suspends on a
   semantic GitHub event, requests user input, or asks for a repair invocation.
6. The kernel constructs a bounded coding subgoal with exact artifact,
   workspace, permission, approval, and budget grants.
7. If no eligible IDE worker is connected, the work item remains durably
   `waiting_for_worker`.
8. The extension displays the proposed action and requests approval before any
   repository mutation.
9. On approval, the extension launches Codex locally through ACP against the
   selected workspace.
10. The harness returns normalized progress and a structured result containing
    files changed, tests run, branch or commit identity, remaining issues, and
    evidence references.
11. The agent refreshes GitHub evidence and repeats the wait, review, and repair
    cycle.
12. When GitHub reports the pull request merged and every goal criterion has
    current evidence, the agent proposes `CompleteGoal`.

The extension or connector MUST NOT merge the pull request. If a repository
change requires a new pull request rather than updating the watched branch,
the task reports that bounded result and waits for explicit user direction in
V1.

## Installation and one-time setup

Installation is a separate workflow from normal agent execution:

1. install the Cursor or VS Code extension;
2. authenticate the user to the Kiteframe deployment through OIDC;
3. connect and authorize GitHub;
4. select an organization-approved harness profile;
5. verify existing Codex authentication or enterprise gateway access;
6. configure applicable budget policy;
7. verify repository visibility and workspace trust; and
8. persist only non-secret connection metadata and opaque credential
   references.

Later service-monitor setup MAY use a provider skill through a bounded harness
invocation to inspect and instrument a repository. Such a skill is setup input;
it is not embedded into the long-running monitoring or PR-babysitting agent.

## Durable work items and IDE workers

A harness work item contains:

```yaml
work_item_id: work_123
task_id: task_123
role: coding
goal: Address review threads 17 and 23 and verify affected tests.
repository: acme/payments
revision: abc123
context_artifacts:
  - context_789
required_capabilities:
  - workspace.read
  - workspace.write
  - terminal.test
approval: required
classification: restricted
executor_profile: approved-local-coding
```

Delivery semantics are:

- outbound worker connection from the IDE;
- persist before notification;
- at-least-once work offering;
- explicit accept or reject;
- short worker lease, heartbeat, and fencing token;
- idempotent progress and completion;
- no reassignment of a potentially mutating invocation before reconciliation;
  and
- `waiting_for_worker` while the laptop or IDE is unavailable.

If the IDE disconnects before execution starts, the offer can safely return to
the queue. Once a mutating invocation starts, expiration of the worker lease
does not prove that the mutation did not occur.

## Budgets and metering

### Three independent limit classes

Kiteframe distinguishes:

1. structural limits such as workflow steps, harness turns, tool calls,
   parallelism, and active execution time;
2. consumption limits such as tokens, requests, or provider quota; and
3. exact monetary spend where a harness can report billable usage.

### Hierarchical policy

Budget policy is deployment-owned and can overlap:

```yaml
budgets:
  - scope:
      organization: acme
    metric: usd
    limit: 10000
    window: month

  - scope:
      agent: babysit_pr
    metric: model_turns
    limit: 100
    window: day

  - scope:
      principal: user_456
    metric: usd
    limit: 10
    window: day
```

Applicable organization, team, principal, agent, credential, and run budgets
all constrain a request. Agent source, task input, and plugin configuration may
narrow a budget but MUST NOT raise deployment policy.

### Reservation and settlement

Before every billable harness turn, the kernel:

1. finds every applicable budget;
2. computes a conservative maximum charge;
3. atomically reserves capacity against every applicable budget;
4. dispatches only if every reservation succeeds;
5. settles against actual reported usage;
6. releases unused capacity; and
7. deduplicates settlement by invocation and provider request identity.

Harness plugins advertise one metering guarantee:

```text
exact_cost
tokens_only
provider_quota
none
```

A hard USD budget requires `exact_cost`. Subscription-backed Codex or Claude
usage can be constrained by turns, tokens, time, or exposed quota windows, but
Kiteframe MUST NOT present estimated equivalent cost as an enforceable dollar
cap.

When a budget is exhausted, deterministic monitoring continues, new billable
work stops, and the active goal becomes `waiting_for_budget`. Execution resumes
after reset or an explicit bounded override. Overrides record approver, reason,
additional allowance, affected task, and expiry.

Optional proxies such as Headroom can reduce consumption or enforce a local
defense-in-depth cap. The centralized Kiteframe ledger remains authoritative
across agents, users, credentials, and machines.

## Authentication and data residency

### Harness profiles

Agent definitions name semantic roles. Deployment policy binds a role to a
complete harness profile:

```yaml
harness_profiles:
  approved-coding:
    adapter: acp
    authentication: enterprise_gateway
    endpoint: https://models.acme.internal
    allowed_harnesses:
      - codex
    allowed_models:
      - organization-approved-coding-model
    executor_location: developer_machine
    budget_policy: engineering-default
```

V1 supports three authentication shapes:

- an existing local harness subscription;
- a personal API key stored in the operating-system or editor secret store and
  injected only into the local harness process; and
- an enterprise gateway using a short-lived user- and device-scoped token while
  retaining the upstream provider key server-side.

The enterprise gateway is preferred when centralized revocation, model
allowlists, budgets, residency, and audit are required.

### Secret handling

Raw credentials MUST NOT appear in:

- `.kite` source or a `GoalContract`;
- plugin messages, task events, or context artifacts;
- PostgreSQL task, goal, invocation, or audit rows;
- logs, traces, or coding transcripts; or
- workspace or ordinary IDE settings.

Kiteframe stores opaque credential-profile and secret-reference identities.
Python connectors resolve server-side secrets through the enterprise vault.
Local credentials remain on the developer machine.

### Data classification and executor policy

Every task, event, artifact, and work item carries a data classification and
permitted executor profiles. Dispatch fails closed if executor location,
endpoint, authentication, retention, or capability policy is incompatible.

For the interactive IDE executor:

- the source checkout and Git credentials stay with the workspace;
- only bounded context artifacts are sent to the worker;
- model egress uses the approved harness endpoint;
- full coding transcripts are not returned to the control plane by default;
  and
- structured outcomes, evidence identities, branch, commit, and pull-request
  references are retained for orchestration and audit.

A future unattended executor requires a distinct server-side profile,
workload identity, repository materialization policy, secret boundary, and
approval policy. Enabling the interactive path does not authorize unattended
execution.

### Inbound identity

Webhook signatures, IDE OIDC sessions, connector workload identities, and
future Slack identities are independently authenticated and mapped to a kernel
principal before a task or event is accepted. A caller-supplied principal field
is never authoritative.

## Cursor and VS Code extension

### One codebase, two editor adapters

The TypeScript extension targets the stable VS Code Extension API. Cursor uses
the same core artifact plus a thin optional adapter for `vscode.cursor`
capabilities:

```text
extension core
  Kiteframe protocol client
  task and approval state
  Codex ACP worker
  editor-neutral view models
  EditorHost
    VS Code adapter
    Cursor adapter
```

V1 task execution MUST NOT depend on Cursor-only APIs. Cursor-specific plugin
or MCP registration can improve discovery without becoming an execution
contract.

### User experience

The primary interface is structured rather than chat-first:

- `Kiteframe: Sign In`;
- `Kiteframe: Babysit Current PR`;
- `Kiteframe: Show Active Tasks`;
- an activity view for current goal, phase, wait, budget, and worker state;
- approval cards for repository mutation;
- notifications for blockers, budget waits, and completed work; and
- links to evidence, checks, diffs, commits, and pull requests.

The extension derives repository, branch, and pull-request context from the
workspace. It SHOULD NOT ask the user to paste values that the editor or GitHub
connector can determine unambiguously.

### Editor security

- Status viewing MAY work in an untrusted workspace.
- Repository inspection, harness launch, and mutation require workspace trust.
- Authentication material uses the editor's encrypted secret storage.
- Provider credentials remain in the Codex login or operating-system store.
- The Codex worker runs in the extension host associated with the workspace.
- One source tree produces marketplace-specific packages and metadata; editor
  behavior is implemented through adapters rather than forks.

## Failures, recovery, and delivery guarantees

### Plugin availability

Temporary plugin unavailability moves work to `waiting_for_plugin` and uses
capped retry with jitter. It does not immediately block the goal. Protocol or
payload incompatibility fails closed and reports the required version.

After kernel restart, plugins re-register and pending commands are redelivered
idempotently. The kernel reconstructs runnable tasks, waits, approvals, work
offers, budget reservations, and uncertain effects from committed state.

### External action lifecycle

Harness and connector mutations use:

```text
prepared -> dispatched -> running -> succeeded | failed | outcome_unknown
```

`outcome_unknown` means an external mutation may have occurred but cannot be
verified. The task suspends. A connector status operation or explicit human
resolution is required before progress. Kiteframe MUST NOT blindly retry the
mutation or classify it as a definitive failure.

### Policy and user outcomes

- Approval rejection becomes an agent event; the runtime may choose another
  path or block.
- Authorization denial is not retried until a relevant policy revision.
- Budget exhaustion waits instead of failing.
- A stable external obstacle may block the goal only with evidence and explicit
  unlock conditions.
- Cancellation records late results for audit but prevents them from advancing
  the cancelled task.
- Internal invariant violations terminate visibly and never trigger an
  unrestricted fallback.

### Stable failures

The extended runtime preserves the minimal engine's stable failures and adds
only plugin-layer distinctions required for actionability:

| Code | Meaning |
|---|---|
| `plugin_unavailable` | A required plugin exceeded its bounded retry policy. |
| `plugin_incompatible` | Protocol, schema, or capability negotiation failed. |
| `event_invalid` | An authenticated event violated its registered schema or immutable identity. |
| `harness_failed` | The harness returned a definitive terminal failure. |
| `authorization_denied` | Current policy denied the requested operation. |
| `outcome_unknown` | An external effect may have occurred and requires reconciliation. |

Goal `blocked`, operational waits, approval rejection, and budget waiting are
not stable task failures.

## Persistence model

PostgreSQL remains the sole execution authority. The existing workflow tables
are evolved or supplemented with logical records for:

| Record | Durable responsibility |
|---|---|
| Agent definitions | Immutable source, compiled graph, runtime binding, version, and hash. |
| User tasks | Principal, target, selected agent version, state, and revisions. |
| Goal contracts | Immutable contract, phase, evidence assertions, blocker, and revisions. |
| Plugin registrations | Protocol, capabilities, deployment identity, and lifecycle. |
| Commands | Persisted transition commands, dispatch attempts, and results. |
| Context artifacts | Provenance, classification, schema, digest, expiry, and storage reference. |
| Channel events | Authenticated normalized envelope, digest, delivery identity, and correlation. |
| Event waits | Event type, correlation fields, frame, and resolution. |
| Work items | Required capabilities, approval, lease, fencing, and terminal result. |
| Harness invocations | Profile, goal, artifacts, permissions, metering, status, and recovery. |
| Budget accounts | Scope, metric, window, limit, reservation, settlement, and override. |

Current-state rows remain the source of truth. Ordered events support
inspection, UI delivery, and audit; the engine does not reconstruct authority
by replaying arbitrary plugin events.

## Testing and acceptance evidence

### Protocol conformance

Golden fixtures cover every plugin command, event, schema version, capability
negotiation, idempotency field, and stable failure. The same contract suite runs
against:

- the `.kite` agent runtime;
- the built-in harness;
- the generic ACP harness adapter;
- the Python connector host; and
- the TypeScript IDE worker.

Codex has a dedicated live-adapter integration suite. Other harnesses MUST pass
the same suite before Kiteframe advertises support.

### State-machine and recovery tests

Model-based tests exercise all valid and invalid transitions for:

- goal lifecycle;
- agent-runtime commands;
- event waits and correlation;
- worker offers, leases, and fencing;
- harness and connector actions;
- approvals and cancellation; and
- budget reservation and settlement.

Crash injection stops the kernel before and after each durable boundary and
then verifies recovery without duplicate events, harness work, repository
mutations, or budget settlement.

### Channel neutrality

Two synthetic event families prove ingress is not GitHub-specific:

```text
github.review.submitted.v1
service.slo.burn_rate_exceeded.v1
```

V1 implements the GitHub path. The SLO fixture verifies schema registration,
authentication, deduplication, correlation, and generic agent dispatch without
shipping a service-monitoring product.

### End-to-end V1 scenario

Release evidence proves:

1. the Cursor or VS Code extension starts `babysit_pr`;
2. GitHub context is collected with provenance;
3. actionable review feedback produces a bounded work item;
4. an offline IDE leaves the work durably queued;
5. approval launches Codex locally through ACP;
6. structured changes, tests, and evidence return;
7. duplicate webhook delivery causes no duplicate transition;
8. a second review triggers another bounded repair;
9. required checks pass and GitHub reports the pull request merged; and
10. the goal completes with criterion-by-criterion evidence.

Additional suites verify principal isolation, data-classification enforcement,
secret redaction, workspace trust, authorization denial, concurrent budget
races, cancellation, plugin incompatibility, and `outcome_unknown` recovery.

Live GitHub and Codex smoke tests supplement deterministic fakes but do not
replace them.

## Cumulative V1 feature slices

V1 is delivered as small, independently reviewable slices:

1. Add NXL-shaped `agent` syntax with nested tasks and a deprecated `workflow`
   alias.
2. Add canonical `UserTask` and immutable `GoalContract`.
3. Add durable goal lifecycle and evidence-backed completion commands.
4. Extract the agent-runtime transition contract around the existing `.kite`
   engine.
5. Wrap the existing direct model loop as the built-in harness plugin.
6. Add versioned out-of-process plugin registration and messaging.
7. Add immutable context artifacts and provenance.
8. Add the semantic event envelope, channel bindings, deduplication, and
   correlation.
9. Add durable harness work items, IDE worker leases, approvals, and fencing.
10. Add authentication profiles and hierarchical budget reservation.
11. Build the Python connector/control-plane host.
12. Implement GitHub pull-request, review, and Actions connectors.
13. Build the generic ACP harness adapter and verify Codex.
14. Build one VS Code extension artifact with a thin Cursor adapter.
15. Implement `babysit_pr` through merged-pull-request completion.
16. Add recovery, security, conformance, and end-to-end release evidence.

Each slice preserves prior behavior and ends with a demonstrable contract or
user outcome. Detailed implementation plans are written incrementally as each
slice becomes active rather than speculating all task-level detail now.

## V1 acceptance criteria

V1 is complete only when all of the following are true:

- one `.kite` agent source defines nested focused tasks, typed transitions,
  context modes, and durable event waits;
- every accepted user task has one immutable goal contract;
- no harness-native goal can directly complete a Kiteframe goal;
- the kernel durably validates and commits every plugin transition;
- the existing built-in model loop operates through the harness contract;
- the generic ACP adapter completes the Codex conformance suite;
- GitHub webhooks and polling repair produce the same semantic events;
- event delivery is authenticated, persisted, deduplicated, and correlated;
- context artifacts carry provenance, classification, schema, and digest;
- worker leases and fencing prevent duplicate reassignment after uncertainty;
- every billable invocation reserves and settles all applicable budgets;
- secrets are absent from source, protocol payloads, durable task state, logs,
  and traces;
- one extension codebase operates in Cursor and VS Code without requiring
  Cursor-only execution APIs;
- the full `babysit_pr` release scenario completes with auditable evidence;
- the synthetic SLO event passes channel-neutral conformance; and
- no excluded service-monitoring, unattended-execution, auto-merge, Slack,
  marketplace, Claude Code, or DeepSeek production claim appears in V1 release
  evidence.

## References

- [Firetiger: NXL, a declarative language for agents](https://blog.firetiger.com/nxl-a-declarative-language-for-agents/)
- [Agent Client Protocol](https://agentclientprotocol.com/)
- [Codex ACP adapter](https://github.com/agentclientprotocol/codex-acp)
- [Using Goals in Codex](https://developers.openai.com/cookbook/examples/codex/using_goals_in_codex)
- [Claude Code goals](https://code.claude.com/docs/en/goal)
- [DeepSeek Harness goal subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/goal.md)
- [VS Code Extension API](https://code.visualstudio.com/api)
- [Cursor Extension API](https://cursor.com/docs/extension-api)
- [Headroom](https://github.com/headroomlabs-ai/headroom)
- [Kiteframe minimal workflow engine design](2026-08-11-kiteframe-minimal-workflow-engine-design.md)
