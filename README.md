# Kiteframe

Kiteframe is a durable Rust workflow and declarative-agent harness. The single
`kiteframe-harness` process compiles author-authored `.kite` workflows, stores
workflow state in PostgreSQL, evaluates typed Cedar policy, discovers MCP
tools, and serves the Agent Client Protocol (ACP).

The engine persists runs, frames, questions, permission requests, model
attempts, tool calls, and run events. It schedules model turns, durable tool
calls, child tasks, parallel joins, questions, cancellation, and recovery. The
Python and TypeScript packages are thin clients: they do not carry a workflow
compiler, scheduler, policy evaluator, or durable state.

## Local stdio

The local harness requires PostgreSQL, an authenticated local principal, and a
Responses-compatible model configuration:

```console
export KITEFRAME_DATABASE_URL=postgres://postgres:postgres@localhost/kiteframe
export KITEFRAME_PRINCIPAL=local-user
export KITEFRAME_RESPONSES_BASE_URL=https://api.example.com/v1
export KITEFRAME_RESPONSES_API_KEY=...
export KITEFRAME_RESPONSES_MODEL=...
cargo run --bin kiteframe-harness
```

It accepts one ACP message per line over NDJSON. The authenticated connection
owns the principal, and the session binding cannot be replaced by a command
payload.

## Remote ACP

Set `KITEFRAME_BIND` to serve ACP over WebSocket and Streamable HTTP. Remote
mode requires TLS to be terminated upstream and one or more bearer-token to
principal bindings:

```console
export KITEFRAME_BIND=127.0.0.1:8080
export KITEFRAME_TLS_TERMINATED=true
export KITEFRAME_BEARER_BINDINGS='[{"secret":"replace-me","principal":"service-user"}]'
cargo run --bin kiteframe-harness
```

The optional ACP v2 adapter is compile-time gated and must also be enabled at
runtime with `KITEFRAME_ENABLE_ACP_V2=true`.

## Workflow authoring

Workflows declare typed tasks, model outcomes, MCP tools, transitions,
questions, child calls, parallel work, limits, and terminal results. See the
[support workflow example](examples/support.kite) for a small end-to-end
workflow. Workflow compilation, immutable publication, execution, and restart
recovery are implemented.

## Declarative agents

Kiteframe compiles and immutably publishes versioned `agent` definitions. An
authenticated principal can submit a bounded GitHub pull-request task that
atomically freezes one exact agent version and one immutable `GoalContract`,
then start and control that task through the authoritative Rust kernel. See the
[babysit PR authoring example](examples/babysit_pr.kite), the
[language reference](docs/reference/kite-language.md), and the
[task and goal contract](docs/reference/user-task-and-goal-contract.md). The
[runtime and goal lifecycle reference](docs/reference/agent-runtime-and-goal-lifecycle.md)
defines the kernel/runtime boundary.

The implemented Slice 2 path provides immutable exact-version start, a pure
built-in `kite.v1` runtime, revisioned task/goal/snapshot transitions, an
ordered command journal, evidence-backed complete/block authority,
pause/resume/cancel, restart recovery, and a bounded direct-loop harness
executor. The process scheduler shares the same kernel, model client, and
durable dispatcher used by the application.

These ACP extensions are private application methods, not a public provider or
plugin protocol:

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

Root `done`, a successful harness invocation, task success, task failure,
cancellation, and goal completion are distinct states. A direct-loop
`complete_task` result completes only one bounded harness invocation and can
mint only `harness.invocation_result` evidence. Only Rust-validated
`CompleteGoal` or `BlockGoal` commands can complete or block the durable goal.
The GitHub-typed `babysit_pr` goal therefore cannot complete end to end until
later context and connector slices provide authorized GitHub evidence.

No stable plugin/provider surface, external runtime transport, semantic
channel binding, context/event resolver, worker/lease system, hierarchical
budget/profile system, provider connector, Codex integration, editor worker,
or Cursor/VS Code extension is implemented. The example is a reviewed
language/runtime fixture, not a claim that the product vertical is operational.

## Design and plan

- [Minimal workflow engine design](docs/superpowers/specs/2026-08-11-kiteframe-minimal-workflow-engine-design.md)
- [Implementation plan](docs/superpowers/plans/2026-08-11-kiteframe-minimal-workflow-engine.md)
- [Declarative agents and plugin runtime design](docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md)
- [Declarative-agent foundation plan](docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-1.md)
- [Declarative-agent lifecycle plan](docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-2.md)

## Development verification

Configure `DATABASE_URL` in the ignored worktree-local `.env`, install the
locked client dependencies, and run the complete checks from the repository
root:

```console
set -a; source .env; set +a
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cd clients/python && uv run ruff check . && uv run pyright && uv run pytest
cd ../typescript && bun run typecheck && bun test && bun run build
```

## License

Apache License 2.0. See [LICENSE](LICENSE).
