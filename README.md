# Kiteframe

Kiteframe is a durable Rust workflow harness. The single
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
workflow.

## Design and plan

- [Minimal workflow engine design](docs/superpowers/specs/2026-08-11-kiteframe-minimal-workflow-engine-design.md)
- [Implementation plan](docs/superpowers/plans/2026-08-11-kiteframe-minimal-workflow-engine.md)

## Development verification

Run the baseline Rust checks from the repository root:

```console
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
python3 -W error::ResourceWarning -m unittest discover -s clients/python/tests -v
bun test clients/typescript/test
```

## License

Apache License 2.0. See [LICENSE](LICENSE).
