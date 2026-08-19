# Testing New Kiteframe Features

Kiteframe features often cross a pure state machine, a PostgreSQL authority
boundary, and protocol clients. Test the narrowest layer that proves the
behavior, then add durable integration coverage whenever a feature persists,
recovers, or competes for authority.

## Choose the right test layer

| Change | Required proof |
|---|---|
| Parser, compiler, DSL diagnostic, runtime reducer, or closed wire type | A focused pure Rust test that asserts the exact accepted and rejected shape. |
| Migration, store mutation, revision/CAS behavior, command journal, recovery, or principal isolation | A configured-PostgreSQL integration test against durable rows and transactions. |
| Public Python or TypeScript behavior | The affected client suite, plus the Rust protocol/transport coverage that owns the wire contract. |
| Cross-layer feature | Focused pure tests first, then PostgreSQL authority/recovery coverage and all affected client gates. |

Use deterministic fakes for models, tools, and transports. Live services may
supplement these checks, but never replace the deterministic suite.

## Test races by controlling the schedule

Concurrency tests should prove a specific interleaving. Do not coordinate a
race with arbitrary sleeps, tracing output, or an assumed task order.

1. Put a narrow, test-only hook at the authoritative boundary whose ordering
   matters—for example, after a completion probe observes no committed winner
   and before it decides the request is new.
2. Have the contender notify the test once it reaches that hook.
3. Wait for that one-shot notification with a bounded timeout. If it never
   arrives, fail with a useful message rather than hanging the suite.
4. Hold the contender at a bounded gate, run the winning mutation, then
   release the gate explicitly.
5. Arrange cleanup so the gate is released even when an assertion fails; no
   worker thread or join may be stranded by a failed test.
6. Assert the durable result: exactly one transition is committed, the runtime
   reduces once, and an identical losing request returns the winner's
   idempotent result rather than creating duplicate authority.

The lifecycle completion-race coverage uses this pattern with a test-support
hook, `Notify`, a bounded condition-variable gate, and release-on-drop cleanup.
It replaced coordination through incidental tracing behavior.

## Isolate durable fixtures

Stateful PostgreSQL and scheduler tests need isolation beyond fresh structs.

- Use a unique disposable database for restart and recovery scenarios.
- Generate unique principals, sessions, definition/task names, event ids, and
  idempotency keys for every fixture.
- Serialize only tests that share an unavoidable process-level resource, using
  a narrow async mutex rather than making unrelated tests globally serial.
- To test recovery, drop the kernel/store handles, open a fresh store against
  the same disposable database, and invoke recovery there. Reusing a live
  process proves neither persistence nor restart behavior.
- Keep tests self-cleaning: close connections before dropping a disposable
  database and release gates even during panics or timeouts.

## Assert durable outcomes

For rejected, raced, cancelled, or recovered operations, assert rows and
revisions—not logs or timing:

- Capture a fingerprint of the authoritative task, goal, snapshot, command,
  evidence, and event rows before an operation that must not mutate state.
- After a stale revision, conflicting idempotency key, unauthorized principal,
  or failed transaction, prove the fingerprint is unchanged.
- Exercise recovery for pending, claimed, terminal, and blocked work. Verify
  recovery reports the committed command states and never replays an already
  committed transition.
- Assert business invariants such as task/goal phase, revision, command state,
  event count, and evidence ownership. Do not assert wall-clock duration,
  scheduler order, incidental SQL query counts, or log wording.

## Feature-work checklist

Before considering a feature ready:

1. Add focused tests for its pure parsing, validation, reducer, or protocol
   behavior.
2. If it changes durable state, add configured-PostgreSQL coverage for the
   success path and every no-mutation rejection path.
3. If it can race, prove the intended interleaving with an explicit test hook
   and assert the durable winner and idempotent loser behavior.
4. If it must survive a restart, use a fresh store against the persisted
   database and cover each relevant command state.
5. Run the full Rust gate and every affected client gate from the repository
   root:

   ```console
   set -a; source .env; set +a
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all-features
   cd clients/python && uv run ruff check . && uv run pyright && uv run pytest
   cd ../typescript && bun run typecheck && bun test && bun run build
   ```

`DATABASE_URL` must target disposable PostgreSQL whenever a change needs
durable integration evidence. A skipped or unconfigured database test is not
evidence that migrations, transactions, recovery, or concurrency behavior is
correct.
