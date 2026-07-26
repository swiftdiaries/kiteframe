# Kiteframe

Kiteframe is a Rust-first, runtime-neutral declarative agent layer.

## Wave 1: portable package checking

Wave 1 validates and hashes portable package bytes only. Its Rust crates
strictly parse the package manifests, load only referenced UTF-8 non-symlink
assets beneath the package root, and produce deterministic portable digests.
The checked-in `AgentManifest` and `RuntimeBinding` JSON schemas are generated
from those Rust contract types and checked for drift in CI.

This boundary does not resolve capabilities, verify capability locks, build
runtime objects, or authorize actors. Runtime integration, capability
resolution, lock verification, and actor authorization belong to later
delivery waves.

## Design

- [Kiteframe declarative agent harness design](docs/superpowers/specs/2026-07-25-kiteframe-declarative-agent-harness-design.md)

## License

Apache License 2.0. See [LICENSE](LICENSE).
