# Wave 3 Task 3 Report: Generated Python Stubs and Golden Parity

## Status

Complete.

Task 3 now provides:

- frozen, non-constructible `CapabilityGrant`, `CapabilityGrantSet`,
  `InvocationOutcome`, and `InvocationStatus` PyO3 projections;
- Rust-deserializing provider-response entrypoints for grant sets, invocation
  outcomes, and invocation statuses;
- scalar read-only properties, immutable tuple projections, stable
  `Literal[...]` status vocabularies, and canonical JSON bytes;
- all four `ResolvedAgent` digest properties required by the Wave 2 golden
  fixture;
- `pyo3-stub-gen` metadata on the native classes, functions, methods, and
  diagnostic exception;
- schema-generator modes that write or check the generated `_native.pyi`;
- focused Python golden/drift tests and Rust-side conversion tests.

No Task 4 registry or Task 5 provider client/protocol implementation was
added.

## Documentation decision

Current Context7 documentation was consulted for both `pyo3-stub-gen` and
PyO3 before choosing the annotations and generation path.

The resulting design follows the current documented contracts:

- `#[gen_stub_pyclass]`, `#[gen_stub_pymethods]`, and
  `#[gen_stub_pyfunction]` register Rust export metadata;
- getter methods produce read-only properties;
- explicit type overrides describe runtime `bytes`, immutable tuples, and
  stable `Literal[...]` discriminants accurately;
- `#[pyclass(frozen, immutable_type)]` prevents instance mutation and type
  monkey-patching;
- no `#[new]`, `dict`, or pickle reconstruction method is exported.

`pyo3-stub-gen` is pinned through the workspace dependency at `0.23`; the
lockfile resolves `0.23.0`.

## Implementation

### Rust-owned service projections

`crates/kiteframe-py/src/service.rs` wraps each contract value in an
`Arc`-owned frozen PyO3 class:

- `CapabilityGrant`
  - `name`
  - `version`
  - tuple `resources`
- `CapabilityGrantSet`
  - admission, actor, agent, task, session, and policy scalar references
  - catalog and grant digests
  - issue and expiry Unix seconds
  - tuple `grants`
  - `canonical_json() -> bytes`
- `InvocationOutcome`
  - exact stable status:
    `succeeded | failed | denied | suspended | deferred | outcome_unknown`
  - invocation ID
  - `canonical_json() -> bytes`
- `InvocationStatus`
  - exact stable status:
    `pending | suspended | succeeded | failed | denied | outcome_unknown`
  - invocation ID
  - `canonical_json() -> bytes`

The three `load_*` functions deserialize directly into the existing
Rust-owned service contract types. Invalid provider response bytes become a
redacted `KiteframeDiagnosticError` carrying `KF-CAP-002`; invalid values
never become Python projection instances.

No service validation, policy logic, feature negotiation, or alternate Python
model was duplicated.

### Golden parity

The Python `ResolvedAgent` projection now exposes:

- `portable_digest`
- `lock_digest`
- `binding_digest`
- `resolved_digest`

`ResolvedAgent` did not previously expose a Rust `portable_digest()` accessor,
so `crates/kiteframe-contract/src/ir.rs` received one minimal read-only getter.
This does not alter construction, canonicalization, validation, or digest
calculation.

The tests compare all four values with
`tests/fixtures/resolved/support-agent.digests.json` and preserve the exact
bytes in `tests/fixtures/resolved/support-agent.json`.

### Generated stub

`kiteframe-py` collects annotated Rust metadata into the public
`kiteframe._native` module description. `kiteframe-schema` renders that module
to the exact destination requested by:

```text
--python-stubs <stub-file>
```

and compares freshly rendered bytes with the checked-in artifact for:

```text
--check-python-stubs <stub-file>
```

The generated stub contains:

- all Rust-owned value classes;
- the three provider-response loaders;
- existing resolution functions and projections;
- `KiteframeDiagnosticError.diagnostics_json`;
- read-only properties;
- immutable tuple and byte return types;
- exact `Literal[...]` outcome/status discriminants;
- no `__init__` or `__new__` for Rust-owned values.

## Changed files

- `Cargo.lock`
  - locks `pyo3-stub-gen` 0.23.0 and transitive dependencies.
- `Cargo.toml`
  - adds the shared `pyo3-stub-gen` dependency.
- `crates/kiteframe-contract/src/ir.rs`
  - adds the missing read-only `ResolvedAgent::portable_digest()` accessor.
- `crates/kiteframe-py/Cargo.toml`
  - consumes `pyo3-stub-gen`.
- `crates/kiteframe-py/src/error.rs`
  - registers the existing diagnostic exception and its byte property for
    stub generation.
- `crates/kiteframe-py/src/ir.rs`
  - annotates existing projections and adds the three missing digest getters.
- `crates/kiteframe-py/src/lib.rs`
  - registers service classes/functions and exposes generated stub text to
    the schema tool.
- `crates/kiteframe-py/src/service.rs`
  - implements the frozen service projections and validated response loaders.
- `crates/kiteframe-py/src/validate.rs`
  - annotates the existing native validation functions for stub generation.
- `crates/kiteframe-py/tests/service_projection.rs`
  - exercises Rust response conversion, variant preservation, and rejection.
- `crates/kiteframe-schema/Cargo.toml`
  - links the native Rust metadata owner.
- `crates/kiteframe-schema/src/main.rs`
  - adds stub generation and drift-check CLI modes.
- `python/kiteframe/src/kiteframe/_native.pyi`
  - generated native extension contract.
- `python/kiteframe/tests/test_native_golden.py`
  - proves golden parity, frozen service projections, and invalid-output
    rejection.
- `python/kiteframe/tests/test_stub_drift.py`
  - proves generator drift and the absence of value constructors/setters.
- `.superpowers/sdd/2026-07-25-kiteframe-v1-wave-3-python-contract/task-3-report.md`
  - this report.

## TDD evidence

### Python RED precondition

The first focused run correctly showed that the cleaned Task 2 worktree needed
its editable native extension rebuilt before a feature-level RED could be
observed.

```text
cwd=python/kiteframe
rtk env -u CONDA_PREFIX uv run --project . pytest \
  tests/test_native_golden.py tests/test_stub_drift.py -q
```

Exit: `2`

Exact relevant output:

```text
ModuleNotFoundError: No module named 'kiteframe._native'
1 error in 0.06s
```

Build precondition:

```text
cwd=python/kiteframe
rtk env -u CONDA_PREFIX uv run --project . maturin develop
```

Exit: `0`

Exact summary:

```text
🍹 Building a mixed python/rust project
🐍 Found CPython 3.13 at .../python/kiteframe/.venv/bin/python
🔗 Found pyo3 bindings
📡 Using build options features from pyproject.toml
📦 Built wheel for CPython 3.13
✏️ Setting installed package as editable
🛠 Installed kiteframe-0.1.0
```

### Python feature RED

```text
cwd=python/kiteframe
rtk env -u CONDA_PREFIX uv run --project . pytest \
  tests/test_native_golden.py tests/test_stub_drift.py -q
```

Exit: `2`

Exact relevant output:

```text
ImportError: cannot import name 'CapabilityGrant' from 'kiteframe._native'
1 error in 0.79s
```

This was the intended failure: the Task 3 service projection did not exist.

### Rust feature RED

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo test -p kiteframe-py --test service_projection
```

Exit: `101`

Exact relevant output:

```text
error[E0432]: unresolved imports `_native::PyCapabilityGrantSet`,
`_native::PyInvocationOutcome`, `_native::PyInvocationStatus`,
`_native::load_invocation_outcome_inner`,
`_native::load_invocation_status_inner`
error: could not compile `kiteframe-py` (test "service_projection")
```

### Rust focused GREEN

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo test -p kiteframe-py --test service_projection
```

Exit: `0`

Exact output:

```text
running 3 tests
test provider_loaders_reject_unknown_variants ... ok
test invocation_loaders_validate_and_preserve_stable_variants ... ok
test grant_set_projection_exposes_only_stable_scalar_and_tuple_values ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Stub generation GREEN

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo run -p kiteframe-schema -- \
  --python-stubs python/kiteframe/src/kiteframe/_native.pyi
```

Exit: `0`

Exact summary:

```text
Finished `dev` profile
Running `target/debug/kiteframe-schema --python-stubs
python/kiteframe/src/kiteframe/_native.pyi`
```

Drift check:

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo run -p kiteframe-schema -- \
  --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi
```

Exit: `0`

Exact summary:

```text
Finished `dev` profile
Running `target/debug/kiteframe-schema --check-python-stubs
python/kiteframe/src/kiteframe/_native.pyi`
```

### Python focused GREEN

Fresh final command:

```text
cwd=python/kiteframe
rtk env -u CONDA_PREFIX uv run --project . pytest \
  tests/test_native_golden.py tests/test_stub_drift.py -q
```

Exit: `0`

Exact output:

```text
........                                                                 [100%]
8 passed in 7.00s
```

## Final verification

### Complete nested Python suite

```text
cwd=python/kiteframe
rtk env -u CONDA_PREFIX uv run --project . pytest -q
```

Exit: `0`

Exact output:

```text
.................                                                        [100%]
17 passed in 0.47s
```

### Changed Rust crates

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo test -p kiteframe-contract -p kiteframe-py -p kiteframe-schema
```

Exit: `0`

Exact suite summary:

```text
kiteframe-contract:
  capability_contract: 11 passed
  diagnostic_contract: 6 passed
  digest_contract: 2 passed
  schema_contract: 13 passed
  service_contract: 20 passed
kiteframe-py:
  service_projection: 3 passed
kiteframe-schema:
  0 tests, binary compiled successfully
Doc-tests:
  kiteframe_contract: ok
  _native: ok
Total non-doc Rust tests: 55 passed; 0 failed
```

The brief-required `cargo test -p kiteframe-py` also passed independently:

```text
running 3 tests
test provider_loaders_reject_unknown_variants ... ok
test invocation_loaders_validate_and_preserve_stable_variants ... ok
test grant_set_projection_exposes_only_stable_scalar_and_tuple_values ... ok
test result: ok. 3 passed; 0 failed
Doc-tests _native: ok
```

### Formatting, linting, drift, and diff hygiene

```text
rtk cargo fmt --all -- --check
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo clippy -p kiteframe-contract -p kiteframe-py \
  -p kiteframe-schema --all-targets -- -D warnings
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo run -p kiteframe-schema -- \
  --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi
rtk git diff --check
```

All exited `0`.

Generated extension binaries and Python cache directories were removed before
staging.

## Self-review

- Ownership: every response loader deserializes into the Rust contract type
  before creating a Python object.
- Validation: no Python validation or feature-negotiation rule was added.
- Mutation: every Rust-owned Python value is frozen and uses
  `immutable_type`; collection properties are tuples.
- Construction: no value class exports `#[new]`, `__init__`, or `__new__`.
- Dynamic attributes: no class enables `dict`; tests verify there is no
  `__dict__`.
- Pickle: no pickle reconstruction method exists; tests verify projection
  pickling fails.
- Serialization: canonical bytes come only from
  `kiteframe_core::canonical_json`.
- Variants: Rust exhaustive matches map every current contract variant, and
  generated stubs publish the exact `Literal[...]` vocabularies.
- Diagnostics: invalid provider bodies produce a stable redacted
  `KF-CAP-002`, with no serde input details exposed.
- Stub ownership: `_native.pyi` is rendered from Rust inventory, including the
  existing diagnostic exception, rather than maintained by hand.
- Drift: both the schema command and Python test compare complete generated
  bytes.
- Scope: no registry, protocol, HTTP client, adapter, or mutable alternate IR
  was added.

## Concerns

1. The host ambient `CONDA_PREFIX=/opt/anaconda3` points PyO3 at Python 3.9,
   while the nested uv project uses CPython 3.13. As in Task 2, reliable local
   build/test commands must unset `CONDA_PREFIX`; direct Cargo PyO3 commands
   also set `PYO3_PYTHON=/opt/homebrew/bin/python3`.
2. `ResolvedAgent` lacked the read-only `portable_digest()` accessor required
   by the approved Task 3 golden test. The implementation adds only that
   getter to the contract crate; it does not change the frozen Task 2
   resolution or digest algorithms.
3. Task 3 intentionally exports the new service values from
   `kiteframe._native` only. It does not modify the pure-Python public
   re-export surface or implement the registry/client work assigned to Tasks
   4 and 5.
