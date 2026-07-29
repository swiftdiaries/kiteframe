# Kiteframe

Kiteframe is a Rust-first, runtime-neutral declarative agent layer.

## Wave 2: lock, resolve, and compile

Kiteframe strictly validates portable packages, selects exact capability
versions into a self-contained lock, and deterministically resolves a package,
runtime binding, and target component catalog into canonical `ResolvedAgent`
IR. The checked-in JSON schemas, resolved support-agent IR and digest record,
and redacted diagnostic corpus are frozen by exact-byte tests and checked for
drift in CI.

### Trust boundary

- `kiteframe lock` is the only command that mutates package state. It validates
  the complete selection before atomically replacing `capability.lock`. The
  default path remains package-local; an explicit `--output` must be outside
  the canonical package root and cannot alias the consumed catalog.
- `kiteframe check` and `kiteframe explain` are read-only. `kiteframe compile`
  emits canonical IR to stdout or an explicit output artifact; it does not
  rewrite the package, lock, or binding and does not construct runtime objects.
- Deterministic resolution narrows declared package requirements against an
  exact lock and explicit runtime metadata. Actor grants are not an input to
  resolution, and point-of-use authorization remains a runtime enforcement
  responsibility outside this compiler boundary.

Feature handling has two distinct deterministic phases. For schema
compatibility, `CapabilityLock.resolvedFeatures` stores the canonical
package-requested set (`required ∪ optional`), not a target-negotiated result;
lock verification exact-matches that set against the verified package.
Resolution then negotiates the verified request against the explicit target,
and only `ResolvedAgent` records the enabled and omitted target result.

## Design

- [Kiteframe declarative agent harness design](docs/superpowers/specs/2026-07-25-kiteframe-declarative-agent-harness-design.md)

## Development verification

Run the complete Wave 3R Rust, PyO3, schema, and stub matrix from the
repository root:

```console
uv run --project python/kiteframe python scripts/verify_wave3r.py
```

The checked-in command runs through the Python project managed by `uv`, rejects
Python versions below the package's 3.11 floor, and supplies that exact
interpreter and its base installation to PyO3. This avoids macOS system Python
selection for embedded-Python tests and stub generation.

## License

Apache License 2.0. See [LICENSE](LICENSE).
