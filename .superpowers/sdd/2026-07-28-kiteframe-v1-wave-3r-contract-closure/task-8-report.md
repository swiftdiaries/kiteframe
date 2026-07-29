# Task 8 Report: Cross-Language No-Loss and Wave 4/5 Consumer Gates

## Status

Implemented the Wave 3R exit gates across Rust schemas, PyO3 projections,
generated Python stubs, frozen runtime inputs, provider enforcement, and the
self-contained Crankshaft WFM conformance profile.

## Toolchain Root Cause

The reported `pyo3-stub-gen 0.23.0` / `pyo3 0.29.0`
`PyEncodingWarning` failure was reproduced before any repair.

- `pyo3 0.29.0` does define `PyEncodingWarning`, guarded by
  `cfg(Py_3_10)`.
- Direct Cargo commands auto-selected macOS system Python 3.9.6, below the
  package's declared Python 3.11 floor.
- Selecting the project CPython 3.13 interpreter allowed the locked
  `pyo3-stub-gen 0.23.0` and `pyo3 0.29.0` pair to compile and reach the
  actual schema/stub drift checks.
- CI now makes the supported interpreter selection explicit for the Rust
  Wave 3R job.

No `Cargo.toml` or `Cargo.lock` change was made because a dependency change
would not address the root cause.

## Delivered

- Added exact no-loss assertions for the embedded `LockedCapability`,
  independent descriptor/schema/error/safety digests, descriptor canonical
  bytes, native service variants, and immutable projections.
- Added `CapabilityDescriptor.canonical_json()` and exported the three
  existing correlated native loaders from the Python package.
- Added a Wave 4 fake-adapter gate that constructs solely from
  `ResolvedRuntimeInputs` after package, binding, lock, and target paths are
  removed. File reads, JSON/YAML reparsing, IR reloads, and catalog access are
  trapped.
- Added Wave 5 gates for required/optional completeness, monotonic grant
  narrowing across every authority dimension, persisted authority-revision
  digest tampering, admission/grant-only invocation binding, descriptor-aware
  outcome/status validation, protected suspensions, trace propagation,
  strict 304 behavior, and credential-header isolation.
- Added a canonical, self-contained
  `crankshaft-wfm-profile.json` fixture using only generic Kiteframe V1 alpha
  fields and values. The conformance test imports only Kiteframe, the Python
  standard library, and local fixtures.
- Regenerated and checked the Python native stub and the catalog schema.
- Added explicit Wave 3R no-loss/consumer/conformance execution to CI.

## Narrow Matrix Repairs

Unblocking the complete matrix exposed four stale test-only/golden issues.
They were repaired without changing runtime semantics:

- PyO3 0.29 method traits were imported in `service_projection`.
- Typed `ModelRole` and `RegistrySymbol` keys replaced invalid string lookups
  in a `validate.rs` unit test.
- The `ResolvedRuntimeInputs` integration test now exercises Python-visible
  attributes rather than private Rust methods.
- CLI explain and standalone lock goldens were updated to the already-repaired
  support fixture and catalog-freshness digests.

## TDD Evidence

- RED: the new native requirement test failed because
  `CapabilityDescriptor` had no `canonical_json`.
- RED: Python Wave 5 and conformance collection failed because the correlated
  loaders existed natively but were not exported by `kiteframe`.
- GREEN: both focused PyO3 suites passed, 22 tests total.
- RED: the first WFM fixture run rejected its noncanonical baggage correlation
  value. The value was corrected to 32 lowercase hexadecimal characters and
  every dependent admission, grant, and proposal digest was recomputed.
- GREEN: the focused Python no-loss, stub, Wave 4, Wave 5, and conformance
  gates passed, 40 tests total.

## Verification

Passed:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo run -p kiteframe-schema -- --check schemas/v1alpha1`
- `cargo run -p kiteframe-schema -- --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi`
- `uv run --project . pytest -q` - 170 passed
- `uv run --project . ruff check src tests`
- `uv run --project . pyright` - 0 errors, 0 warnings
- `git diff --check`

Local Rust/PyO3 commands selected the supported CPython 3.13 interpreter
explicitly; `PYTHONHOME` was also supplied for embedded-Python Rust tests in
the uv-managed local installation.
