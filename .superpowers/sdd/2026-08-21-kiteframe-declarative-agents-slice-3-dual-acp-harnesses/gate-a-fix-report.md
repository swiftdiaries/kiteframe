# Cross-task Gate A fix report — Slice 3 Tasks 1–4

## Scope and outcome

- Base: `d46623e` on `codex/declarative-agents-slice-3`.
- Scope: all blocking Cross-task Gate A findings for Slice 3 Tasks 1–4, in one path-scoped fix wave.
- Outcome: the Rust and repository-shape verification available in this checkout is green. Live PostgreSQL verification remains unavailable because `DATABASE_URL` is absent; the dedicated lock-order target records that limitation exactly below.
- Unrelated untracked `fuzz/` and `site/` paths were preserved and excluded.
- No subagents or reviewers were dispatched for this fix wave.

## Finding-by-finding changes

### 1. Critical — command/revision replay rejected a fresh invocation candidate

`PostgresStore::prepare_harness_invocation` now treats the generated invocation UUID as a disposable candidate when the `(command_id, command_revision)` uniqueness boundary finds an existing row. It validates the command id/revision, profile id, profile digest, and payload digest, then returns the stored row and its durable invocation UUID. The separate lookup by an already-existing invocation UUID still validates the UUID-bound command identity and returns `StoreError::IdempotencyConflict` if that UUID is reused for a different command/revision.

Coverage now includes:

- a pure store validation test for fresh-candidate replay versus UUID reuse;
- the database-gated prepared-row replay assertion;
- a database-gated terminal replay that generates another fresh candidate after settlement and recovers the stored terminal invocation.

### 2. Important — acceptance was committed before workspace/plugin work

The dispatcher no longer owns or eagerly commits prompt acceptance. A narrow `HarnessAcceptanceBarrier` is injected through the event-sink/invocation context without changing the frozen `HarnessPlugin::start_invocation` signature.

- The production PostgreSQL barrier owns `mark_harness_prompt_accepted(invocation_id, expected_attempt)`.
- `HarnessEventSender::commit_acceptance` is clone-safe and exactly-once through a shared async `OnceCell`.
- The built-in plugin persists `Started`, completes any permission exchange, checks cancellation, then commits acceptance immediately before the model request.
- The registry keeps a pre-accept `Retryable` result retryable, but converts any retryable result after a successful barrier to `harness_outcome_unknown`.
- A completed plugin that never durably started or never committed acceptance fails closed as a protocol violation.
- Workspace lookup and all pre-model plugin work remain before the durable barrier. The same injected contract is available for Task 5 ACP to commit immediately before writing prompt bytes after initialize/session-new.

Deterministic tests prove the built-in model observes exactly one committed barrier before dispatch, and prove the pre-accept retryable/post-accept uncertain split.

### 3. Important — legacy event-only digests bypassed invocation identity

The exported store path now accepts only the canonical v1 envelope digest bound to `protocolVersion`, `invocationId`, `sequence`, and `event`. The permanent legacy event-only digest compatibility branch was removed.

New migration `0016_harness_gate_a.sql`:

- supplies PostgreSQL canonical-v1 envelope and digest functions using PostgreSQL's core `sha256(bytea)`, `convert_to`, and `encode` functions;
- locks the relevant journal tables and rejects pre-existing sequence gaps;
- safely backfills 0014-era journals, inserting `Started` at sequence zero and shifting the earlier events when necessary;
- recomputes every stored event digest using the v1 identity-bound envelope;
- enforces `(sequence = 0) = (event.type = started)` and the canonical digest with database check constraints;
- serializes append order on the parent invocation and rejects sequence gaps in a database trigger;
- distinguishes an exact database replay from event/digest drift even when the caller uses `ON CONFLICT DO NOTHING`.

Tests reject event-only digests at the protocol/store boundary, require `Started` at zero, compare Rust and PostgreSQL digests for every closed v1 event shape, exercise exact database replay versus drift, and explicitly prove that zero followed by two is a rejected gap while zero replay is idempotent. Approval tests now use `Started` at sequence 0 and approval events at sequences 1 and 2.

### 4. Important — Slice 2 unsupported built-in tool semantics drifted

The plugin boundary continues to expose only the closed internal harness failure vocabulary. After the durable harness failure is settled, the dispatcher translates only `builtin-v1` plus `harness_capability_unsupported` back to the frozen direct-loop kernel contract:

- failed command;
- failure code `runtime_command_unavailable`;
- `runtime.command_unavailable` event;
- no legacy runtime work and no goal-authority mutation.

External ACP profiles retain the closed harness failure semantics. The direct-bridge regression now asserts the complete frozen Slice 2 behavior, and a pure routing test prevents the translation from applying to other profiles or other failure codes.

### 5. Promoted minor — built-in source revision was all zeros

Built-in registration is now deployment/build attested:

- `builtin_harness_profile` requires a nonzero full 40-hex source revision;
- profile JSON validation and the database both reject the all-zero revision;
- production startup requires compile-time `KITEFRAME_SOURCE_REVISION` through `option_env!` and validates it before opening the database;
- CI supplies `${{ github.sha }}` to every job that builds the Rust harness binary;
- local build documentation uses the exact clean-checkout `git rev-parse HEAD`, rather than fabricating a revision;
- profile digest changes with the source revision, so replay cannot silently relabel a build.

Migration 0016 handles the prior development registration explicitly. It deletes the all-zero `builtin-v1` row only when no durable invocation references it; otherwise migration stops with SQLSTATE `55000` and requires operator reconciliation. It then adds a nonzero database constraint. Future profile-id upgrades remain explicit because immutable registration replay still rejects a different digest/revision.

### 6. Promoted minor — no explicit sequence-gap proof

`event_sequence_gap_after_started_is_rejected_while_exact_replay_succeeds` now directly exercises sequence zero, exact zero replay, and a zero-to-two gap. The migration also statically and dynamically guards the same invariant.

## TDD evidence

### RED

Before the implementation changes, the new contract tests failed at their intended missing seams:

- `cargo test --test harness_plugin_protocol --no-run` failed because `HarnessEventSender::commit_acceptance` did not exist.
- `cargo test --test harness_builtin_conformance --no-run` failed because `builtin_harness_profile` did not exist.
- `cargo test --test harness_store -- --nocapture` failed because `migrations/0016_harness_gate_a.sql` did not exist.
- `cargo test --bin kiteframe-harness --no-run` failed because `builtin_profile_from_attestation` did not exist.

### GREEN

- `cargo test --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance -- --nocapture` — 7 + 17 + 4 passed.
- `cargo test --test harness_store -- --nocapture` — target compiled; all 17 test functions reported `ok`; live database bodies explicitly self-skipped with `SKIP harness_store: DATABASE_URL is not set`, while repository-shape migration tests executed.
- `cargo test --test harness_store gate_a_migration_backfills_v1_event_identity_and_removes_zero_attestation -- --exact --nocapture` — 1 passed.
- `cargo test --test agent_direct_bridge --test agent_goal_authority --test agent_transition_store -- --nocapture` — 21 + 15 + 16 passed.
- `cargo test --test sequential_runtime --test structured_runtime --test recovery -- --nocapture` — 9 + 5 + 6 passed.
- `cargo test --lib -- --nocapture` — 27 passed.
- `cargo test --bin kiteframe-harness -- --nocapture` — 4 passed; its database scheduler body is environment-gated.
- Final focused rerun: `cargo test --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance --lib -- --nocapture` — 27 + 4 + 17 + 7 passed.
- `cargo check --all-targets --all-features` — passed.
- `cargo check --no-default-features --lib --bin kiteframe-harness` — passed.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed.

## PostgreSQL limitation

`DATABASE_URL` is absent, and no PostgreSQL server/client commands are available in this environment. Therefore the new migration and all database-gated store/replay tests were compiled and statically inspected but not executed against PostgreSQL here.

The required dedicated command was run:

```text
cargo test --test postgres_lock_order -- --nocapture
test result: FAILED. 1 passed; 15 failed
DATABASE_URL is required for PostgreSQL lock-order tests: NotPresent
```

The one repository-shape/global-order test passed. All 15 live lock-order cases failed immediately at the common environment precondition; none reached their database assertions. CI already provisions PostgreSQL and `DATABASE_URL`, so it remains the authoritative live migration/store/lock-order gate.

## Files changed

- `.github/workflows/ci.yml`
- `README.md`
- `migrations/0016_harness_gate_a.sql`
- `src/engine/harness.rs`
- `src/harness/builtin.rs`
- `src/harness/mod.rs`
- `src/harness/plugin.rs`
- `src/harness/profile.rs`
- `src/harness/registry.rs`
- `src/harness/types.rs`
- `src/main.rs`
- `src/store/harness.rs`
- `src/store/models.rs`
- `tests/agent_direct_bridge.rs`
- `tests/harness_builtin_conformance.rs`
- `tests/harness_plugin_protocol.rs`
- `tests/harness_store.rs`
- `.superpowers/sdd/2026-08-21-kiteframe-declarative-agents-slice-3-dual-acp-harnesses/gate-a-fix-report.md`

## Self-review

- Confirmed the frozen `HarnessPlugin::start_invocation` signature is unchanged.
- Confirmed the dispatcher no longer marks acceptance before workspace/plugin work.
- Confirmed a fresh command/revision candidate and reuse of an existing invocation UUID take different validation paths.
- Confirmed no legacy digest compatibility helper or call remains.
- Confirmed the SQL canonical key order matches the Rust canonical JSON envelope and all closed v1 event shapes; live equivalence is covered by a database-gated test.
- Confirmed the SQL insert trigger serializes on the parent invocation, enforces zero/Started and no gaps, accepts exact replay, and rejects replay drift before conflict handling.
- Confirmed unsupported built-in tools alone translate to the frozen Slice 2 unavailable-command contract; external profiles and other plugin failures do not.
- Confirmed production source provenance is supplied by deployment input, with no fabricated runtime fallback, and invalid/missing attestation fails closed.
- Confirmed `fuzz/` and `site/` remain untouched and untracked.
- Remaining concern is environmental only: PostgreSQL execution must run in CI or another checkout with `DATABASE_URL` before Gate A can be accepted as live-database green.

---

## Gate A re-review fix wave — round 2

### Outcome

Round 2 closes the re-review API, attestation-lifecycle, migration lock-order, and barrier-race findings without changing either frozen `HarnessRegistry::invoke` or `HarnessPlugin::start_invocation`. The only outstanding verification limitation remains live PostgreSQL execution in this environment.

### 1. Frozen event-sink API restored

- `HarnessEventSink` is append-only again; the round-1 acceptance and sequence methods were removed from the public trait.
- `PostgresHarnessEventSink::new(store)` is restored to its frozen one-argument constructor.
- The unchanged frozen `HarnessRegistry::invoke(request, approvals, events)` remains available and uses an internal immediate barrier for non-production/frozen callers.
- A separate `HarnessRegistry::invoke_with_acceptance_barrier` control path supports deterministic adapters/tests, while the crate-internal production path additionally receives the recovered event sequence.
- The dispatcher constructs the PostgreSQL barrier separately and injects it immediately before invocation. `HarnessEventSender` still carries the barrier internally, so the plugin start signature remains unchanged.
- A compile-time compatibility test implements an append-only sink and binds the exact one-argument PostgreSQL constructor type.

### 2. Built-in registrations now coexist immutably across builds

- A validated built-in profile now has identity `builtin-v1-<normalized-full-40-hex-source-revision>`; the exact source revision remains part of the immutable profile and digest.
- Different revisions register under different profile IDs and coexist in the in-memory registry and durable registration table. Uppercase input is normalized to the same lowercase commit identity.
- Production startup passes the current attested profile ID explicitly through `HarnessRouteTable`; the frozen two-argument `with_harness_registry` remains for the one-built-in compatibility case.
- Built-in unavailable-command translation no longer recognizes a hard-coded profile ID. It reads the immutable registration transport and translates only `transport = builtin` plus `harness_capability_unsupported`.
- Before selecting a current route for new work, the dispatcher checks for an existing `(command_id, command_revision)` invocation. An existing invocation retains its immutable stored profile and digest (validated against its durable registration) and reaches terminal replay without requiring the current plugin profile. New work selects the current deployment-owned route.
- The migration still removes only the old all-zero `builtin-v1` development row when it is unreferenced. A referenced legacy row stops migration for explicit operator reconciliation; no referenced provenance is deleted or relabeled.

Coverage proves two built-in revisions coexist, a route selects the current revision, startup derives the versioned current ID, and the database-gated direct bridge replays an old terminal invocation without a model call or profile rewrite after the current build changes.

### 3. Migration lock order matches runtime order

Migration 0016 now acquires `harness_plugin_registrations` in `ACCESS EXCLUSIVE` mode before taking the invocation and event table locks. This matches the runtime registration/prepare prefix and prevents the migration from holding invocation state while waiting on registration. A repository-shape assertion requires the explicit order:

```text
harness_plugin_registrations -> harness_invocations -> harness_invocation_events
```

### 4. Deterministic cloned-barrier race coverage

Three direct `HarnessEventSender` tests exercise the real shared `tokio::sync::OnceCell` behavior:

- concurrent cloned callers observe one in-flight durable call and share its successful result;
- a barrier error is cached and returned identically to every clone without another call;
- cancellation/drop of the initializing future releases initialization, allowing a later clone to retry successfully.

The PostgreSQL acceptance transition itself remains idempotent for the same invocation/attempt, so a retry after cancellation cannot create a second accepted state transition.

### Round-2 RED evidence

- `cargo test --test harness_plugin_protocol --test harness_builtin_conformance --no-run` failed because an append-only sink was missing the round-1 required `acceptance_barrier`, the constructor had two arguments, and `invoke_with_acceptance_barrier` did not exist.
- `cargo test --test harness_store gate_a_migration_backfills_v1_event_identity_and_removes_zero_attestation -- --exact --nocapture` failed with `registration table lock is explicit` because 0016 locked invocation/events before ever locking registrations.
- The three promoted barrier tests passed against the existing shared `OnceCell`, characterizing and preserving the already-correct cancellation/cache/concurrency semantics before the API refactor.

### Round-2 GREEN evidence

- `cargo test --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance -- --nocapture` — 7 + 18 + 5 passed.
- `cargo test --lib harness::types::tests -- --nocapture` — 3 passed.
- `cargo test --lib -- --nocapture` — 30 passed.
- `cargo test --bin kiteframe-harness -- --nocapture` — 4 passed; the database scheduler body remains environment-gated.
- `cargo test --test agent_direct_bridge -- --nocapture` — target compiled and 21 functions reported `ok`; database-backed bodies returned at their absent-`DATABASE_URL` gate.
- `cargo test --test harness_store -- --nocapture` — target compiled and 17 functions reported `ok`; live bodies emitted `SKIP harness_store: DATABASE_URL is not set`; repository-shape tests executed.
- `cargo test --test agent_goal_authority --test agent_transition_store -- --nocapture` — 15 + 16 passed.
- `cargo test --test sequential_runtime --test structured_runtime --test recovery -- --nocapture` — 9 + 5 + 6 passed.
- `cargo check --all-targets --all-features` — passed.
- `cargo check --no-default-features --lib --bin kiteframe-harness` — passed.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed.

### Round-2 PostgreSQL limitation

The required `cargo test --test postgres_lock_order -- --nocapture` was rerun. The result remains exactly `1 passed; 15 failed`: the repository-shape/global-order test passed, and all 15 live cases failed immediately with `DATABASE_URL is required for PostgreSQL lock-order tests: NotPresent`. The new migration, registration coexistence, and old-profile terminal replay paths compiled but could not execute against PostgreSQL locally. CI with its PostgreSQL service remains the live database gate.

### Round-2 files and self-review

Changed paths are limited to migration 0016, the harness plugin/registry/types/built-in and dispatcher/store/startup implementation, the four focused test files, and this report. CI/build attestation configuration remains unchanged and continues to pass `${{ github.sha }}` as the full build revision.

- Confirmed frozen `HarnessEventSink`, `PostgresHarnessEventSink::new`, `HarnessRegistry::invoke`, and `HarnessPlugin::start_invocation` signatures.
- Confirmed the production barrier is separate from the event sink and still commits immediately before built-in model dispatch.
- Confirmed production contains no fixed `builtin-v1` routing or translation assumption; remaining exact `builtin-v1` strings are legacy-migration handling or isolated test fixtures.
- Confirmed versioned profile selection applies only to new work, while an existing command/revision uses its stored profile and validates its digest against durable registration.
- Confirmed migration lock acquisition cannot wait on registration after acquiring invocation/event locks.
- Confirmed `fuzz/` and `site/` remain untouched and untracked.
- Remaining concern is environmental only: live PostgreSQL migration/store/replay/lock-order execution is still required before final Gate A approval.

---

## Gate A re-review fix wave — round 3

### Outcome

Round 3 closes the remaining default-acceptance and closed-event-shape findings. The frozen registry entry point now fails closed before plugin or model work unless its caller uses the explicit durable-barrier path, and migration 0016 validates the exact canonical v1 event object before computing or accepting a digest. The only remaining verification limitation is live PostgreSQL execution in this environment.

### 1. Default registry invocation fails closed without durable acceptance

- The frozen `HarnessRegistry::invoke(request, approvals, events)` signature remains unchanged, but it now returns stable `harness_profile_unavailable` failure detail `durable harness acceptance barrier is required` without registering active work, starting a plugin, emitting an event, or calling a model.
- The successful no-op `ImmediateAcceptanceBarrier` was removed. Work-producing deterministic callers use the explicit `invoke_with_acceptance_barrier` path with a real or test barrier.
- Production remains on its separate PostgreSQL acceptance-barrier path. The built-in plugin still commits that barrier immediately before model dispatch, and pre-accept plugin failures remain retryable while post-accept failures remain outcome-unknown.
- A real built-in/model regression test proves the default entry point cannot call the model or return `Completed`; the existing observed-barrier test proves explicit injection completes only after the commit.

### 2. Migration 0016 enforces an exact closed v1 event object

- `harness_event_is_closed_v1(JSONB)` first requires a top-level object, then accepts only the six canonical variants and their exact key sets: `Started` and `Cancelled` contain only `type`; `Progress.message`, `AssistantMessage.text`, `ApprovalRequested.requestId/title`, and `ApprovalResolved.requestId/decision` are required JSON strings; no variant admits extra keys.
- Approval decisions are limited to `allow_once` and `reject_once`. Unknown types, missing/null/coerced fields, and extra keys return false.
- `harness_event_canonical_v1` raises SQLSTATE `55000` on an invalid event before canonicalization or hashing, and the event table has a durable `harness_invocation_events_v1_event_check` constraint in addition to its envelope-digest check.
- Rust v1 wire serialization now uses the canonical camel-case `requestId`. The migration converts valid 0014-era approval rows from `request_id` before replacing the old 0014 shape constraint, preserving their provenance while making the new Rust and SQL digest inputs identical.
- Repository-shape tests require every per-variant exact-key/type guard. Database-gated coverage contains extra-key, numeric-string-field, null, unknown-type, invalid-decision, and missing-key cases, plus Rust/SQL digest equivalence for every event variant and both one-shot approval decisions.

### Round-3 RED evidence

- `cargo test --test harness_builtin_conformance default_registry_invoke_fails_before_builtin_or_model_work_without_a_barrier -- --exact --nocapture` failed because the default entry point completed through the successful immediate barrier.
- `cargo test --test harness_plugin_protocol v1_approval_event_wire_fields_are_camel_case_and_closed -- --exact --nocapture` failed with Rust `request_id` versus required canonical `requestId`.
- `cargo test --test harness_store gate_a_migration_backfills_v1_event_identity_and_removes_zero_attestation -- --exact --nocapture` failed because `harness_event_is_closed_v1` and its exact-shape guards did not exist.

### Round-3 GREEN evidence

- Focused exact reruns for the three RED tests passed.
- `cargo test --test harness_store -- --nocapture` — 18 passed; repository-shape tests executed, while database-backed bodies explicitly emitted `SKIP harness_store: DATABASE_URL is not set`.
- `cargo test --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance -- --nocapture` — 7 + 19 + 6 passed.
- `cargo test --test agent_direct_bridge --test agent_goal_authority --test agent_transition_store -- --nocapture` — 21 + 15 + 16 passed.
- `cargo test --test sequential_runtime --test structured_runtime --test recovery -- --nocapture` — 9 + 5 + 6 passed.
- `cargo test --lib -- --nocapture` — 30 passed, including acceptance-barrier concurrency, cached-error, cancellation/retry, dispatcher replay, and frozen Slice 2 unavailable-command regressions.
- `cargo test --bin kiteframe-harness -- --nocapture` — 4 passed.
- `cargo check --all-targets --all-features` — passed.
- `cargo check --no-default-features --lib --bin kiteframe-harness` — passed.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed before the report append and is rerun in the final diff check.

### Round-3 PostgreSQL limitation

`cargo test --test postgres_lock_order -- --nocapture` exited 101 with exactly `1 passed; 15 failed`. The repository-shape/global-order test passed; all 15 live cases stopped at the common precondition `DATABASE_URL is required for PostgreSQL lock-order tests: NotPresent`. The malformed-event database cases and valid SQL/Rust digest-equivalence cases likewise compiled but self-skipped in `harness_store`. CI or another environment with PostgreSQL and `DATABASE_URL` remains the authoritative live migration/store/lock-order gate.

### Round-3 files and self-review

Round-3 changes are limited to `migrations/0016_harness_gate_a.sql`, `src/harness/registry.rs`, `src/harness/types.rs`, `tests/harness_builtin_conformance.rs`, `tests/harness_plugin_protocol.rs`, `tests/harness_store.rs`, and this report.

- Confirmed frozen `HarnessRegistry::invoke`, `HarnessPlugin::start_invocation`, append-only `HarnessEventSink`, and one-argument `PostgresHarnessEventSink::new` signatures remain intact.
- Confirmed no `ImmediateAcceptanceBarrier` or other successful no-op production acceptance path remains.
- Confirmed the only default `.invoke(...)` harness test is the fail-closed proof; all tests intending actual work select an explicit barrier.
- Confirmed production dispatch still injects the PostgreSQL barrier and never uses the frozen fail-closed entry point.
- Confirmed migration validation precedes digest construction, exact event keys and JSON types match Rust canonical serde, and valid 0014 approval events are converted before the old constraint is replaced.
- Confirmed migration registration/invocation/event lock order and versioned built-in coexistence logic from round 2 remain unchanged.
- Confirmed `fuzz/` and `site/` remain untouched and untracked.
- Remaining concern is environmental only: live PostgreSQL migration/store/replay/lock-order execution is still required before final Gate A approval.
