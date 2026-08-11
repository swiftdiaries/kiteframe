# Kiteframe

Kiteframe is a Rust harness replacement under implementation. No workflow or
service runtime behavior exists yet; the `kiteframe-harness` binary currently
only prints the package version.

## Design and plan

- [Minimal workflow engine design](docs/superpowers/specs/2026-08-11-kiteframe-minimal-workflow-engine-design.md)
- [Implementation plan](docs/superpowers/plans/2026-08-11-kiteframe-minimal-workflow-engine.md)

## Development verification

Run the baseline Rust checks from the repository root:

```console
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## License

Apache License 2.0. See [LICENSE](LICENSE).
