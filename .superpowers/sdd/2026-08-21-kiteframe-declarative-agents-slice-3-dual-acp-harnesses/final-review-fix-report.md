# Final Review Fix Report: Owned Harness Reservation Errors

Date: 2026-08-22

Status: COMPLETE

Review base: `424ffda`

Commit message: `fix: reconcile owned harness reservation errors`

## Finding closed

The strict `Prepared -> Started` reservation correctly prevented a late
dispatcher from stealing a live invocation, but the winning dispatcher still
used fallible store, cancellation-poll, event, registry, settlement, and
lifecycle operations after that durable transition. An ordinary `StoreError`
escaped while leaving the invocation in `Started`; the scheduler logged and
retried the error, and every subsequent tick refused to steal the live
reservation. The command therefore remained claimed and produced a bounded
50ms hot loop until process restart.

## Resolution

- Added one owned-reservation reconciliation boundary immediately after the
  successful `Prepared -> Started` transition. Other dispatchers that observe
  `Started` retain the existing no-steal behavior.
- Reconciliation reloads the durable invocation and command winner. Before
  durable prompt acceptance, it consumes exactly the stored attempt, advances
  to the stored-limit retry ordinal, or commits the canonical exhaustion
  failure. After acceptance, it commits canonical `OutcomeUnknown` and never
  re-dispatches the external work.
- Durable cancellation remains authoritative. A cancellation winner is
  observed under the existing command-first store boundary and is never
  overwritten by retry, failure, or uncertainty settlement.
- Cancellation-poll errors now cancel the exact registry invocation, await the
  managed lifecycle, and finish its owned shutdown before durable
  reconciliation. The live fake-ACP proof confirms the child is bounded and
  reaped without a duplicate process or prompt.
- Added explicit `HarnessDispatcherError::ReconciliationFailed` fatal
  classification. Ordinary scheduler/store errors continue to preserve
  service availability, but an owned reservation whose reconciliation also
  fails terminates the scheduler so stdio/remote service coordination performs
  bounded shutdown and the service supervisor can restart it instead of
  swallowing a permanent hot loop.

## TDD evidence

RED was established before implementation with deterministic tests importing
the missing dispatcher fault boundary and reconciliation barrier. Compilation
failed on the absent `HarnessDispatcherTestFault`,
`HarnessDispatcherTestFaultPoint`, and `BeforeOwnedReconciliation` contracts.

GREEN fault coverage executes after a real PostgreSQL reservation:

- Workspace load, invocation-event load, event persistence, and registry
  boundary failures all leave no `Started` row. Stored-limit retries advance
  once and execute once; a one-attempt limit commits the canonical exhausted
  failure without invoking the model.
- A cancellation-poll failure after durable acceptance cancels and finishes
  the owned lifecycle, settles `OutcomeUnknown`, and cannot spin, re-prompt, or
  duplicate model/process work.
- Principal cancellation while reconciliation is paused remains the durable
  task, command, and invocation winner.
- Injecting both a post-reservation dispatch error and reconciliation-store
  outage makes `run_scheduler` return the explicit fatal error within a bounded
  timeout instead of entering another tick.
- The existing live late-dispatcher case remains green for attempt limits one
  and three, proving this fix does not grant recovery authority to a second
  live dispatcher.

## Verification

All database suites ran against an isolated live PostgreSQL 16 container; no
database-gated test skipped.

- `agent_direct_bridge`: 34 passed.
- `agent_goal_authority`: 15 passed.
- `harness_acp_recovery`: 21 passed.
- `harness_store`: 37 passed.
- `harness_acp_conformance`: 36 passed.
- `harness_plugin_protocol`: 19 passed.
- Focused fatal scheduler regression: 1 passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo build --all-targets --all-features`: passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

## Scope

The fix changes only the dispatcher/scheduler error boundary, its public error
and test-support exports, and the direct/live regression tests. No migration,
store schema, plugin protocol, or late-dispatcher recovery policy changed.
Untracked `fuzz/` and `site/` were preserved and remain unstaged.

---

# Runtime Fix Round 1: Finish ACP Lifecycle on Settlement Errors

Date: 2026-08-22

Status: COMPLETE

Review base: `1a46135`

Commit message: `fix: finish ACP lifecycle on settlement errors`

## Reviewer blocker closed

After a managed ACP invocation reported `Completed`, the dispatcher attempted
durable terminal settlement before calling `ManagedHarnessInvocation::finish`.
A settlement `StoreError` returned through `?`, dropping the managed lifecycle.
Drop aborted the adapter task and relied on child `kill_on_drop`, but did not
await the ACP shutdown deadline, reap the child, or preserve a shutdown-failure
audit.

The dispatcher now retains the managed lifecycle across all terminal
persistence outcomes. It captures the settlement or cancellation-poll result,
calls `finish` exactly once, preserves any changed shutdown terminal for audit,
reconciles the original durable error, and only then writes the shutdown audit.
The original dispatch/reconciliation error keeps precedence. A lifecycle-audit
write that still fails after bounded shutdown is classified explicitly as
fatal `HarnessDispatcherError::LifecycleAuditFailed` instead of being swallowed
after an invocation-state refresh.

## TDD evidence

RED was established before production changes by adding live fake-ACP tests
against the wished-for `TerminalSettlement`, `LifecycleAudit`, and
`LifecycleAuditFailed` contracts. Compilation failed with five expected
missing-variant errors.

GREEN executes a real ACP process after durable acceptance and completion:

- The adapter is held at `BeforeAdapterShutdown`; terminal settlement fails;
  the invocation remains `Accepted`, the command remains claimed, evidence is
  zero, and the tick cannot return while shutdown ownership is held.
- After release, the nonzero fake ACP child exits and is reaped within the
  bounded timeout. Reconciliation commits canonical `OutcomeUnknown`, the
  shutdown audit is appended, and a second tick creates no process, prompt,
  evidence, or goal-authority mutation.
- An injected lifecycle-audit write failure is returned as the explicit fatal
  classification only after the child has exited. The already canonical
  command/invocation winner remains stable and a second tick does not duplicate
  evidence.

## Verification

All database-backed cases ran against disposable PostgreSQL 16 instances.

- Both new exact PostgreSQL/fake-ACP regressions passed.
- `harness_acp_recovery`: 23 passed.
- `harness_acp_conformance`: 36 passed.
- `harness_plugin_protocol`: 19 passed.
- Fresh repository-wide
  `KITEFRAME_SOURCE_REVISION=<exact-HEAD> DATABASE_URL=<isolated-postgres> cargo test --all-features --quiet -- --test-threads=1`:
  passed with no failed target.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo build --all-targets --all-features`: passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

The first broad attempt correctly failed the three child-process tests because
the required compile-time `KITEFRAME_SOURCE_REVISION` was absent. A subsequent
attempt on the already-used base database hit the fixture's capped first-64
dispatch scan. The reported final gate is the revision-correct serial run on a
fresh empty database.

## Scope

This round changes only `src/engine/harness.rs`, the live ACP recovery tests,
and this report. No schema, adapter protocol, retry/ownership policy, or goal
authority contract changed. Untracked `fuzz/` and `site/` remain untouched and
unstaged.
