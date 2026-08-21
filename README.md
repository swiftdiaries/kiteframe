# Kiteframe

Kiteframe is a durable Rust workflow and declarative-agent harness. The single
`kiteframe-harness` process compiles author-authored `.kite` definitions,
persists authority in PostgreSQL, evaluates typed Cedar policy, discovers MCP
tools, and serves the Agent Client Protocol (ACP). Python and TypeScript are
thin clients: the compiler, scheduler, policy evaluator, and durable state all
remain in the Rust kernel.

## What works today

Workflow compilation, immutable publication, execution, and restart recovery
are implemented. Workflows can model typed tasks, model outcomes, MCP tools,
questions, child calls, parallel work, limits, and terminal results. Start with
the small [support workflow](examples/support.kite).

Declarative-agent work is complete through the first three V1 slices:

| Completed slice | Available outcome |
|---|---|
| 1. Authoring and durable intent | Versioned `agent` definitions, immutable publication, canonical `UserTask` submission, and one frozen immutable `GoalContract` per accepted task. |
| 2. Authoritative agent lifecycle | Exact-version agent start, the built-in `kite.v1` runtime, revisioned task/goal/snapshot transitions, command journaling, evidence-backed complete/block authority, pause/resume/cancel, recovery, and the bounded direct-loop harness executor. |
| 3. Dual ACP harnesses | Built-in Responses, Codex ACP and DeepSeek Harness ACP implement one `HarnessPlugin` contract with deployment-owned profiles, portable ACP conformance, durable invocation recovery, and fail-closed capabilities. |

For a practical overview, read the [developer guide](docs/guides/building-on-kiteframe.md).
For feature work, use the [testing guide](docs/guides/testing-kiteframe.md).

## Local stdio

The local harness requires PostgreSQL, an authenticated local principal, and a
Responses-compatible model configuration. Builds must also receive
`KITEFRAME_SOURCE_REVISION` as the exact full 40-hex commit represented by a
clean source tree; startup rejects missing, malformed, or placeholder
provenance, including the former all-zero development revision:

```console
export KITEFRAME_SOURCE_REVISION="$(git rev-parse HEAD)"
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
executor. Slice 3 places that built-in Responses path, Codex ACP, and DeepSeek
Harness ACP behind the same versioned `HarnessPlugin` contract. The main
process scheduler owns the durable agent dispatcher and builds it from the
application's shared kernel and the same model client used by `Engine`.

Codex ACP and DeepSeek Harness ACP share the portable required core: a fresh
session, text prompt, one-shot permission, cancellation, and committed output.
Richer Codex capabilities are optional and fail closed when a selected profile
does not advertise them. DeepSeek uses fresh, connection-owned sessions; it
does not promise resume, editor, MCP, or rich progress surfaces. A harness
terminal supplies bounded invocation evidence only. The Rust kernel alone
commits durable task and goal state.

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

Cordis auto-composability, dynamic package lifecycle, marketplaces, hot reload,
and runtime-generated authority are deferred from V1. GitHub connectors, IDE
delivery, guarded worker leases, Slice 4 context/event infrastructure, Slice 5
work delivery, Slice 6 connector control plane, Slice 7 editor path, and Slice
8 `babysit_pr` E2E completion are not implemented by Slice 3. The example is a
reviewed language/runtime fixture, not a claim that the product vertical is
operational.

## Planned V1 slices

The remaining slices are product intent, not currently available integration
surfaces:

| Slice | Intended outcome |
|---|---|
| 4. Context and event ingress | Provenance-bearing context, semantic events, bindings, deduplication, correlation, and durable waits. |
| 5. Guarded local work delivery | Work items, approvals, IDE worker offers, leases, fencing, and uncertain-outcome recovery. |
| 6. Governed connector control plane | Connector identity, secret references, data classification, profiles, and budget enforcement. |
| 7. GitHub, coding harnesses, and editor path | GitHub connectors, Codex- and DeepSeek-Harness-verified ACP, and VS Code with a thin Cursor adapter. |
| 8. `babysit_pr` release | An end-to-end, auditable review-and-repair loop through pull-request merge, verified once per V1 harness. |

The [developer guide](docs/guides/building-on-kiteframe.md) explains how to
build against the completed boundary without depending on these future seams.

## Design and plan

- [Minimal workflow engine design](docs/superpowers/specs/2026-08-11-kiteframe-minimal-workflow-engine-design.md)
- [Implementation plan](docs/superpowers/plans/2026-08-11-kiteframe-minimal-workflow-engine.md)
- [Declarative agents and plugin runtime design](docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md)
- [Declarative-agent foundation plan](docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-1.md)
- [Declarative-agent lifecycle plan](docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-2.md)
- [Dual-harness Slice 3 plan](docs/superpowers/plans/2026-08-21-kiteframe-declarative-agents-slice-3-dual-acp-harnesses.md)
- [Developer guide](docs/guides/building-on-kiteframe.md)
- [Feature-work testing guide](docs/guides/testing-kiteframe.md)

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
