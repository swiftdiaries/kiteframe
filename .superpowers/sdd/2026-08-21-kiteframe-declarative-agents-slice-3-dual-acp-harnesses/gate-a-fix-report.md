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
