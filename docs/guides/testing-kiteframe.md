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

## Slice 3 dual-harness release gate

The built-in Responses harness, Codex ACP, and DeepSeek Harness ACP share one
`HarnessPlugin` contract. Deterministic conformance proves the portable core:
fresh session, text prompt, one-shot permission, cancellation, and committed
output. Run these focused checks from the repository root before the broader
release matrix:

```bash
cargo test --features test-support --test harness_acp_conformance --test harness_acp_recovery
cargo test --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance
cargo test --test harness_store
```

### Live ACP certification setup

The process environment passed to an ACP child is deliberately explicit: it is
not inherited from the shell. Production deployments may validly configure an
empty child environment, but the normative live-certification tests require an
explicit, nonempty authentication readiness declaration. This prevents an
accidentally logged-in developer session from being counted as live-provider
evidence.

Obtain the credentials in the current session from the deployment's secret
manager or other approved session injection. Do not put them in a checked-in
`.env` file, command history, test output, or debug log; do not enable `set
-x` while setting these variables. The maps below use placeholders only, not
real credentials.

For Codex ACP, pass the selected authentication variable in the JSON object
and name exactly that nonempty key in the required-key list. This profile uses
an absolute executable path, so `PATH` is not needed for the command shown.

```bash
export KITEFRAME_CODEX_ACP_ENV_JSON='{"OPENAI_API_KEY":"<obtain-from-secret-manager-or-session>"}'
export KITEFRAME_CODEX_ACP_REQUIRED_ENV_JSON='["OPENAI_API_KEY"]'
KITEFRAME_CODEX_ACP_COMMAND_JSON='["/absolute/codex-acp"]' KITEFRAME_CODEX_ACP_SOURCE_REVISION='296069e841634cd4bb9bc4515602d836e49231ec' cargo test --test harness_acp_live codex_acp_live_core -- --exact --nocapture
```

For DeepSeek Harness ACP, pass its authentication key in the map and list it
as the required key. The checked-in command starts with the bare `pnpm`
executable, so its explicit map must also include a nonempty absolute `PATH`.
Replace `/absolute/bin` with the trusted directory that contains the intended
`pnpm`, preserving any additional required absolute directories.

```bash
export KITEFRAME_DSH_ACP_ENV_JSON='{"DEEPSEEK_API_KEY":"<obtain-from-secret-manager-or-session>","PATH":"/absolute/bin"}'
export KITEFRAME_DSH_ACP_REQUIRED_ENV_JSON='["DEEPSEEK_API_KEY"]'
KITEFRAME_DSH_ACP_COMMAND_JSON='["pnpm","--dir","/absolute/deepseek-harness","run","demo:acp"]' KITEFRAME_DSH_ACP_SOURCE_REVISION='141eb6fef83422698aef7a981029e843e8161534' cargo test --test harness_acp_live deepseek_harness_acp_live_core -- --exact --nocapture
```

`*_ACP_ENV_JSON` must be a JSON object whose keys and values are strings;
`*_ACP_REQUIRED_ENV_JSON` must be a JSON array of unique, nonempty key names.
Every listed key must occur in the map with a nonempty value. The example
authentication key names are the expected keys for these checked-out provider
commands; if a pinned provider revision documents different names, update both
the map and the required-key list together. Values are redacted by Kiteframe's
configuration debug output, but operators must still avoid printing either
JSON variable.

Each configured executable and checkout must be built from the exact checked-in
repository and full commit named by its profile. The source-revision environment
value is a deployment attestation checked before process spawn. A different
revision, or an incompatible observed ACP protocol or capability set, fails
before invocation. Self-reported implementation names and versions are audit
metadata only; they do not establish deployment identity.

The two live commands are independent certification gates. If a provider command
or its required explicit authentication environment is absent, record that
provider's exact `SKIP` line. Do not convert a skip into live-adapter
implementation evidence. Use an isolated disposable PostgreSQL instance for
the database suites below, then remove it after the matrix.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo check --all-targets --all-features
cargo test --features test-support --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance --test harness_acp_conformance --test harness_acp_recovery --test repository_shape
cargo test --test harness_store --test postgres_lock_order
cargo test --all-features --test agent_runtime_contract --test agent_lifecycle_store --test agent_start --test kite_agent_runtime --test agent_transition_store --test agent_goal_authority --test agent_direct_bridge --test agent_protocol --test e2e_agent_restart --test acp_conformance --test transports
cargo test --all-features
cargo check --no-default-features --lib --bin kiteframe-harness
uv run --directory clients/python pytest
bun --cwd clients/typescript test
bun run --cwd clients/typescript typecheck
git diff --check
```
