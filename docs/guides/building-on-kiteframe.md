# Building on Kiteframe

Kiteframe is easiest to extend when its durable-authority boundary stays
clear: a runtime or harness can propose work, but only the Rust kernel validates
and commits durable task, goal, snapshot, command, policy, and evidence state.

## Start here

Choose the path that matches your work:

| If you are… | Read this first |
|---|---|
| Authoring a workflow or agent | [Kite language reference](../reference/kite-language.md), then [examples/babysit_pr.kite](../../examples/babysit_pr.kite) |
| Submitting or controlling durable agent work | [UserTask and GoalContract reference](../reference/user-task-and-goal-contract.md) |
| Changing runtime, kernel, store, or recovery behavior | [Agent runtime and goal lifecycle reference](../reference/agent-runtime-and-goal-lifecycle.md), then the [testing guide](testing-kiteframe.md) |

## Build with Kiteframe today

The completed Slice 1–3 surface supports these durable building blocks:

1. Author a `workflow` or `agent` definition in `.kite`.
2. Publish an immutable version of that definition.
3. For an agent, submit a bounded `UserTask`; acceptance freezes one exact
   published agent version and one immutable `GoalContract`.
4. Start and control the accepted task through the authenticated lifecycle
   surface, using expected revisions and idempotency keys.
5. Read the bounded lifecycle view or ordered task events from the same
   session-scoped authority.

For bounded harness work, Built-in Responses, Codex ACP, and DeepSeek Harness
ACP implement one versioned `HarnessPlugin` contract. Codex and DeepSeek share
the portable ACP core: fresh session, text prompt, one-shot permission,
cancellation, and committed output. Deployment-owned `HarnessProfile` IDs
select the implementation; `.kite` source remains provider-free. Rich Codex
features are optional and fail closed when requested from a profile that does
not support them. DeepSeek uses fresh, connection-owned sessions and does not
promise resume, editor, MCP, or rich progress surfaces.

Harness output is bounded invocation evidence. Only the Rust kernel can commit
durable task, goal, snapshot, command, policy, and evidence state. Cordis auto-composability is deferred from V1, as are dynamic package lifecycle,
marketplaces, hot reload, and runtime-generated authority.

The application currently exposes the following private ACP extension methods:

```text
_kiteframe/definition/publish
_kiteframe/task/submit
_kiteframe/task/get
_kiteframe/agent/start
_kiteframe/agent/get
_kiteframe/task/pause
_kiteframe/task/resume
_kiteframe/task/cancel
_kiteframe/task/events
```

These are private Kiteframe application methods, not a stable plugin or
provider protocol. Use them for a deployed Kiteframe application you control;
do not publish a general integration that assumes their wire shapes are a
long-term compatibility promise.

### Completion is explicit

Task lifecycle, operational status, and goal phase are separate state axes.
Root `done` can succeed a task, and a direct-loop `complete_task` result can
complete one harness invocation, but neither completes the durable goal. Only
Rust-validated `CompleteGoal` and `BlockGoal` commands have that authority.
The task and goal reference defines the exact lifecycle matrix and evidence
rules.

## Extend the core safely

Core changes must preserve the kernel as the sole durable authority. In
practice, that means:

- Keep `AgentRuntime` deterministic and side-effect free. It validates a
  frozen definition and proposes a replacement snapshot plus closed commands;
  it does not call stores, transports, tools, clocks, or models.
- Carry exact task and goal revisions and an idempotency key through every
  mutation. A stale or conflicting request must leave authoritative rows
  unchanged.
- Preserve canonical snapshot validation, command ceilings, recovery without
  replay, and the global lock order: session, user task, goal state, frozen
  definition, snapshot, then commands in stable order.
- Preserve legacy workflow bytes, hashes, protocol behavior, and client
  compatibility while adding agent behavior.

The runtime lifecycle reference is normative for these rules. The testing
guide explains how to prove them with deterministic and PostgreSQL-backed
coverage.

## Current boundary and future seams

| Slice | Status | Intended seam |
|---|---|---|
| 1. Authoring and durable intent | Complete | Immutable agent definitions and accepted task/goal intent. |
| 2. Authoritative lifecycle | Complete | Rust-owned transitions, commands, evidence, recovery, and bounded direct-loop execution. |
| 3. Replaceable harness and plugins | Complete | Built-in Responses plus Codex ACP and DeepSeek Harness ACP profiles behind one versioned `HarnessPlugin` contract and portable ACP core. |
| 4. Context and events | Planned | Authenticated context/event ingress, correlation, and durable waits. |
| 5. Guarded work delivery | Planned | Work items, approvals, leases, fencing, and uncertain-outcome reconciliation. |
| 6. Connector control plane | Planned | Profiles, classification, secrets, connector policy, and budgets. |
| 7. GitHub, coding harnesses, and editors | Planned | GitHub evidence, Codex- and DeepSeek-Harness-compatible ACP, VS Code, and a thin Cursor adapter. |
| 8. `babysit_pr` release | Planned | The complete, evidence-backed review-and-repair product loop, verified once per V1 harness. |

Do not add connector/provider identity to `.kite`, semantic-channel,
worker/lease, budget/profile, secret, context/event, work-delivery, connector
control-plane, editor, or Cordis package-lifecycle fields to the Slice 3
contracts. Those are later-slice design commitments, not incomplete fields
waiting to be filled in.

## Common mistakes

| Mistake | Correct model |
|---|---|
| Treating `_kiteframe/*` methods as a public SDK | They are private application extensions; the stable plugin protocol belongs to Slice 3. |
| Letting model text or a successful tool result complete a goal | A goal completes only through a kernel-validated command with exact authorized evidence. |
| Treating `done`, task success, and goal completion as synonyms | They are distinct transitions on separate state axes. |
| Adding provider, credential, endpoint, or model identity to agent source or Slice 2 runtime data | Authoring stays semantic and provider-free until the governed control-plane work. |
| Reconstructing state by replaying old runtime transitions on startup | Recovery loads the committed snapshot and pending/claimed commands without replaying authority. |
