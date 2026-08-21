# Gate B fix report

Base: `a9ef2ce` (`docs: correct client release commands`)

## P1: selected external profile without a configured command

Resolved. A deployment route can select a valid checked-in external profile while its command is
absent. Previously the dispatcher claimed the agent command and then asked only the in-memory
registry for the profile digest. That lookup returned `harness_profile_unavailable` before the
invocation journal existed, leaving the command claimed.

The fix separates durable profile identity from runtime plugin availability:

- startup always persists the two checked-in external profile registrations, even when their
  commands are absent;
- dispatch prepares or reuses the immutable command-bound invocation from the durable registration;
- an absent runtime plugin settles the canonical terminal
  `harness_profile_unavailable: selected harness profile is not registered` through normal command
  failure authority;
- the invocation UUID supplies the stable `harness-terminal:<uuid>` idempotency key, and the command
  completion digest is verified from the exact `command_failed` event;
- restart after prepare reuses the same invocation, while a principal cancellation wins the terminal
  race and settles the invocation as cancelled;
- the unavailable path invokes no plugin, appends no harness events, creates no evidence, and does
  not mutate goal state;
- unsupported runtime commands continue to use the distinct `runtime_command_unavailable` authority
  path; profile route syntax and selection semantics are unchanged.

## TDD evidence

- RED: direct bridge returned
  `InvalidData("harness_profile_unavailable: selected harness profile is not registered")` after
  claim; the cancellation regression timed out before terminal authority.
- RED: startup/config regression found no durable `codex-acp-v1` registration when its command was
  absent.
- GREEN: three direct bridge regressions cover exact terminal/key/digest, second tick and recovery
  no-op, restart reuse, cancellation authority, and absence of plugin/evidence/goal side effects.
- GREEN: startup/config regression proves the actual selected route retains both checked-in durable
  identities while the selected profile remains absent from the runtime registry.

## Verification evidence

- Live isolated PostgreSQL 16:
  - `agent_direct_bridge`: 29 passed.
  - `agent_goal_authority`: 15 passed.
  - `harness_acp_recovery`: 20 passed.
  - `harness_profiles`: 15 passed.
  - `harness_acp_live`: 2 passed (both provider-command gates skipped as explicitly unconfigured).
  - `harness_store`: 37 passed serially.
  - `recovery`: 6 passed serially.
  - `kiteframe-harness` binary tests: 6 passed serially.
- No-database focused matrix: all targets passed; database cases emitted only their explicit skips.
- `harness_builtin_conformance`: 6 passed.
- `harness_plugin_protocol`: 19 passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo build --all-targets --all-features`: passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

One parallel `harness_store` run observed another test's recoverable row in
`recovery_limit_is_total_and_exclusion_makes_bounded_scans_progress`; the exact case and the full
target passed on the isolated database with `--test-threads=1`.

Scope note: existing untracked `fuzz/` and `site/` remain untouched and excluded from the fix.
