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

---

# Final Review Fix: Isolated Python Client CI Smoke

Date: 2026-08-22

Status: COMPLETE

Review base: `3887425`

Commit message: `ci: isolate client smoke verification`

## Findings closed

The Rust job ran its complete PostgreSQL suite against `kiteframe_ci`, then
launched the production harness for the Python live-client smoke against that
same database. Test rows could therefore influence a real harness startup and
the live client test. Separately, the standalone Python job ran Pyright from
the repository root, which made it type-check unrelated fixture code rather
than only the Python client project.

## Resolution

- Added `tests/ci/isolated_database.py`, which validates a generated,
  identifier-safe database name; uses argument-vector `createdb`/`dropdb`
  calls; derives the new PostgreSQL URL while preserving connection options;
  and writes the URL only to GitHub's output file.
- The Rust job now creates a run-scoped database after Rust tests, passes that
  URL only to the Python live smoke, and force-drops it in an `always()` step.
  Harness startup and migrations therefore still execute against a real empty
  PostgreSQL database.
- Added a helper unit test and a repository-shape assertion that requires the
  Rust-test -> create -> Python-smoke -> cleanup order plus the corrected
  Pyright working directory.
- Pyright now runs as `uv run pyright` from `clients/python`; its existing Ruff
  and test commands remain unchanged.

## TDD evidence

The new URL-helper test was first run without its implementation and failed
with the expected missing `isolated_database.py` error. The repository-shape
test then failed because the workflow had no isolated-database stage. Both
passed after the minimal helper and workflow changes.

The prior Pyright command reproduced RED with one unrelated fixture error:
`tests/fixtures/responses_ready.py` overrides `BaseHTTPRequestHandler`
`log_message` using `_format` rather than `format`. The corrected
client-directory command is GREEN with zero errors.

## Verification

- CI YAML parsed successfully.
- Isolated-database helper unit test passed.
- Full repository-shape suite passed.
- `cargo fmt --all --check` and
  `cargo clippy --all-targets --all-features -- -D warnings` passed.
- Python client Ruff and corrected Pyright passed with zero diagnostics.
- Local CI-sequence reproduction used two disposable PostgreSQL 16 databases:
  a baseline with a deliberate `ci_contamination_marker`, and a fresh
  live-smoke target. The fresh target had no marker, acquired real migrations,
  and ran the compiled harness plus all Python client tests: **25 passed**.
  Both databases were force-dropped and their absence confirmed afterward.

## Scope

This fix changes only CI isolation/typecheck wiring, its small PostgreSQL
helper and checks, and this report. It does not change production Rust, client
behavior, migrations, or the TypeScript smoke paths. Untracked `fuzz/` and
`site/` remain untouched and unstaged.

---

# Final Review Fix Round 1: Harden Isolated Database Verification

Date: 2026-08-22

Status: COMPLETE

Review base: `fa07acd`

Commit message: `ci: harden isolated database verification`

## Findings closed

The initial helper supplied `createdb` and `dropdb` with a PostgreSQL URL in
argv. A process failure could then include credentials in a
`CalledProcessError` traceback. The initial repository-shape guard also
searched raw workflow text, so a comment or unrelated job could accidentally
satisfy its checks.

## Resolution

- The helper now parses PostgreSQL URLs and invokes libpq tools with only the
  non-secret maintenance database in argv. Host, port, user, decoded password,
  and recognised connection options are supplied through `PG*` environment
  variables; credential-bearing subprocess stdout/stderr are discarded and
  failures raise only a fixed public error.
- The derived smoke URL preserves the original scheme, percent encoding, IPv6
  authority, and query options. It is emitted as a GitHub `add-mask` command
  immediately before being written to `GITHUB_OUTPUT`.
- The helper test now covers encoded credentials, IPv6, query options,
  secret-free argv, output masking, and a failing command whose stdout,
  stderr, and exception all omit its credential sentinel.
- Replaced raw text matching with a `yaml_serde` test-only parser. The guard
  validates ordered mappings in `jobs.rust`, exact create/cleanup names, ids,
  command arguments, shared target environment, smoke output consumption,
  `always()` cleanup, and the exact Python Pyright working-directory command.
  Negative mutations prove comments and a non-`rust` decoy job cannot pass.

## TDD evidence

The new helper regressions first failed because the existing command had no
libpq environment and embedded the credential-bearing URL in argv; the
failure-sentinel case also had no sanitised command error. The structural test
first failed with its missing parser/guard contracts. Both suites passed after
the minimal helper, parser, and structural assertions were added.

## Verification

- Helper regression suite: 4 passed.
- Parsed CI structural guard: 8 repository-shape tests passed, including both
  negative mutations.
- CI YAML parsed successfully.
- `cargo fmt --all --check` and strict
  `cargo clippy --all-targets --all-features -- -D warnings` passed.
- Python client Ruff and Pyright passed with zero diagnostics.
- A live PostgreSQL 16 simulation seeded a contaminated baseline and started
  the compiled harness against a separate fresh target. The target contained
  no contamination marker, received real migrations, and ran all Python
  client tests: **25 passed**. Both explicitly named disposable databases were
  deleted and confirmed absent afterward.

## Scope

This round changes only the CI smoke helper/tests, the structural workflow
test and its test-only YAML dependency, and this report. Production Rust,
client behavior, migrations, TypeScript smoke paths, and untracked `fuzz/`
and `site/` remain untouched.

---

# Final Review Fix: Reproducible ACP Live Authentication Gates

Date: 2026-08-22

Status: COMPLETE

Review base: `817f65e`

Commit message: `docs: make ACP live gates reproducible`

## Finding closed

The public live-certification commands set only each ACP command and source
revision. The live harness intentionally requires both an explicit child
environment map and a nonempty required-key list before it may count a
provider as authentication-ready, so copied commands always emitted the
authentication skip line instead of exercising the provider.

## Resolution

- Documented complete, secret-safe Codex and DeepSeek Harness setup with the
  exact command/revision attestations retained verbatim.
- Named both environment-map and required-key variables for both providers;
  showed string-to-string JSON maps, unique required-key lists, placeholder
  values only, and the DeepSeek bare-`pnpm` absolute `PATH` requirement.
- Explicitly distinguished valid production empty child environments from the
  normative live-certification readiness requirement, and directed operators
  to approved secret/session injection without committing, printing, or
  tracing values.
- Extracted the live test's skip-versus-ready decision into a non-spawning
  readiness function. Its regression proves the exact missing-command and
  missing-auth skip lines, while a complete placeholder configuration reaches
  the authenticated runnable branch without launching a provider.
- Added a repository-shape guard requiring the public testing guide to name
  all four live authentication configuration variables. Existing profile
  coverage continues to prove explicit environments are redacted, required
  values are nonempty, and bare executables require `PATH`.

## TDD evidence

The readiness test was first run without the extracted gate and failed at
compile time because `LiveTarget::readiness` and `LiveReadiness` did not yet
exist. The documentation guard then failed with the expected missing
`KITEFRAME_CODEX_ACP_ENV_JSON` assertion. Both passed after the minimal gate
extraction and complete setup guide.

## Verification

- `cargo test --test repository_shape`: 9 passed.
- `cargo test --test harness_profiles`: 15 passed.
- `cargo test --test harness_acp_live -- --nocapture`: 3 passed; current
  unconfigured provider targets emitted their exact honest command-absent
  `SKIP` lines.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo build --all-targets --all-features`: passed.
- `git diff --check`: passed.

## Scope

This fix changes only the public testing guide, its structural guard, the
live-test readiness seam and regression, and this append-only report. No
provider process was spawned; no secret values were supplied or logged.
Untracked `fuzz/` and `site/` remain untouched and unstaged.
