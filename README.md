# Kiteframe

Kiteframe is a durable Rust workflow harness under active implementation. The
single `kiteframe-harness` process owns PostgreSQL workflow state, workflow
compilation, typed Cedar policy evaluation, and local NDJSON stdio dispatch.

The Python and TypeScript packages are thin child-process clients. They do not
contain a workflow compiler, scheduler, policy evaluator, or durable state.

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

It accepts one NDJSON request per line. The envelope owns `session_id`; command
payloads cannot replace the connection principal or session binding. Remote
WebSocket and Streamable HTTP ACP serving, MCP restart attachment, and
status-first tool recovery are not complete yet.

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
