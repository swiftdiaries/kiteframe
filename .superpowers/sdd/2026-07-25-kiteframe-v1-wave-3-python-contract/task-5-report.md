# Wave 3 Task 5 report: expanded native provider boundary

## Expanded-task context

The original Task 5 HTTP-client assignment was stopped before implementation
because the Python native surface did not expose the Rust-owned request and
catalog values that the client was required to consume and return. The human
authorized a plan amendment that inserted this dependency-boundary task as the
new Task 5 and renumbered the HTTP client and exit-gate work to Tasks 6 and 7.

This report covers only the expanded native request/catalog task. No provider
protocol, HTTP transport, policy, adapter, registry integration, alternate IR,
or additional route was added.

## Implementation

The native extension now exposes four frozen, non-constructible Rust-owned
values:

- `CatalogRequest`
- `AdmissionRequest`
- `InvocationRequest`
- `CapabilityCatalog`

Each projection:

- owns its validated Rust value through `Arc`;
- is declared with PyO3 `frozen` and `immutable_type`;
- has no `#[new]`, `__init__`, `__new__`, dynamic `__dict__`, or pickle
  reconstruction path;
- returns only read-only scalars and immutable tuples;
- serializes only through Rust `canonical_json()`.

`CatalogRequest.default()` is the sole public factory. It creates an empty
catalog request with no known digest and a fresh W3C traceparent generated from
Python's cryptographically secure `secrets` module. The generated trace ID and
parent ID are not reused between default requests.

The extension also exposes canonical-only loaders:

- `load_catalog_request(...)`
- `load_admission_request(...)`
- `load_invocation_request(...)`
- `load_capability_catalog(...)`

The new loaders execute this exact boundary:

1. parse the body as JSON without exposing it to Python;
2. reproduce canonical JSON and reject any byte mismatch;
3. validate the generic JSON value against the embedded checked-in locked
   schema;
4. deserialize it into the Rust-owned contract type, preserving all Rust
   semantic constructors and digest checks;
5. return a frozen projection only after every phase succeeds.

Malformed, noncanonical, locked-schema-invalid, and contract-invalid payloads
become redacted `KiteframeDiagnosticError` values with stable public
`code == "KF-CAP-002"`. The exception text is sourced only from the safe
diagnostic message, and `diagnostics_json` remains canonical and redacted.

`CapabilityCatalog` uses the existing `CapabilityCatalog::try_new`/custom
deserialization path, so Python does not parse descriptors, compute the catalog
digest, select versions, or negotiate features. Admission and invocation
loaders similarly retain the existing Rust request invariants.

The pre-existing Task 3 grant/outcome/status response loaders retain their
established schema-first behavior. Canonical-byte rejection is scoped to the
four new request/catalog entrypoints required by this amended task.

## Schema and stub ownership

The amended task requires a corresponding locked schema for every new loader.
Because the repository did not previously generate a `CatalogRequest` schema,
`kiteframe-schema` now generates and checks:

```text
schemas/v1alpha1/catalog-request.schema.json
```

The generated `_native.pyi` now contains all four value classes, all four
loaders, read-only properties, the approved `CatalogRequest.default()` static
factory, and `KiteframeDiagnosticError.code`. Stub drift checks prove that no
public value constructor was introduced.

## Changed files

- `docs/superpowers/plans/2026-07-25-kiteframe-v1-wave-3-python-contract.md`
  - records the human-authorized Task 5 insertion and renumbering.
- `crates/kiteframe-py/src/service.rs`
  - adds the four projections, canonical schema-first loaders, secure default
    catalog factory, locked validators, and redacted error mapping.
- `crates/kiteframe-py/src/lib.rs`
  - registers and exports the native classes, functions, and Rust-side loader
    helpers.
- `crates/kiteframe-py/src/error.rs`
  - exposes the stable first diagnostic code and safe diagnostic message.
- `crates/kiteframe-py/tests/provider_request_projection.rs`
  - covers immutable projections, noncanonical input, locked-schema ordering,
    Rust request validation, and Rust catalog digest validation.
- `crates/kiteframe-schema/src/main.rs`
  - generates and checks the locked catalog-request schema.
- `schemas/v1alpha1/catalog-request.schema.json`
  - checked-in generated schema used by the embedded native validator.
- `python/kiteframe/src/kiteframe/__init__.py`
  - re-exports the four native values and loaders.
- `python/kiteframe/src/kiteframe/_native.pyi`
  - generated public native API.
- `python/kiteframe/tests/test_native_provider_requests.py`
  - covers factory-only construction, fresh trace contexts, exact canonical
    round trips, mutation/pickle rejection, immutable properties, and redacted
    invalid-output errors.
- `python/kiteframe/tests/test_stub_drift.py`
  - includes the new classes and permits only the approved static factory.
- `.superpowers/sdd/2026-07-25-kiteframe-v1-wave-3-python-contract/task-5-report.md`
  - this expanded-task report.

## TDD evidence

### Python RED precondition

The first sandboxed invocation could not read uv's external cache:

```text
error: failed to open file `/Users/adhita/.cache/uv/sdists-v7/.git`:
Operation not permitted (os error 1)
```

After granting cache access, the editable extension needed its normal rebuild:

```text
ModuleNotFoundError: No module named 'kiteframe._native'
```

The baseline extension rebuild completed successfully with
`rtk env -u CONDA_PREFIX uv run --project . maturin develop`.

### Python feature RED

Command, from `python/kiteframe`:

```text
rtk env -u CONDA_PREFIX uv run --project . \
  pytest tests/test_native_provider_requests.py -q
```

Exit: `2`

Exact feature failure:

```text
ImportError: cannot import name 'AdmissionRequest' from 'kiteframe'
1 error in 0.79s
```

This failed because none of the amended Task 5 native request/catalog exports
existed.

### Rust feature RED

Command:

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo test -p kiteframe-py --test provider_request_projection
```

Exit: `101`

Exact relevant output:

```text
unresolved imports `_native::PyAdmissionRequest`,
`_native::PyCapabilityCatalog`, `_native::PyCatalogRequest`,
`_native::PyInvocationRequest`, `_native::load_capability_catalog_inner`,
`_native::load_catalog_request_inner`,
`_native::load_invocation_request_inner`

no variant or associated item named `NonCanonical`
found for enum `ProviderResponseError`
```

### Secure default-factory RED

The first implementation used one deterministic valid traceparent for every
default request. The focused test proved the reuse:

```text
assert request.traceparent != next_request.traceparent
AssertionError:
'00-00000000000000000000000000000001-0000000000000001-00'
==
'00-00000000000000000000000000000001-0000000000000001-00'
1 failed in 0.03s
```

The minimal fix generates fresh trace and parent IDs with `secrets.token_hex`.

### Focused GREEN

Python, from `python/kiteframe`:

```text
rtk env -u CONDA_PREFIX uv run --project . \
  pytest tests/test_native_provider_requests.py -q
```

Exit: `0`

```text
........                                                                 [100%]
8 passed in 0.87s
```

Rust:

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo test -p kiteframe-py --test provider_request_projection
```

Exit: `0`

```text
running 5 tests
test locked_schema_rejection_precedes_typed_contract_validation ... ok
test provider_request_boundary_rejects_noncanonical_bytes ... ok
test schema_valid_request_still_uses_rust_contract_validation ... ok
test schema_valid_catalog_still_uses_rust_digest_validation ... ok
test request_and_catalog_projections_expose_only_stable_values ... ok

test result: ok. 5 passed; 0 failed
```

## Final verification

### Complete nested Python suite

Command, from `python/kiteframe`:

```text
rtk env -u CONDA_PREFIX uv run --project . pytest -q
```

Exit: `0`

```text
...............................                                          [100%]
31 passed in 7.94s
```

### Relevant Rust crates

Command:

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
  provider_request_projection: 5 passed
  service_projection: 6 passed
kiteframe-schema:
  binary tests: 0, compiled successfully
Doc-tests:
  kiteframe_contract: ok
  _native: ok
Total non-doc Rust tests: 63 passed; 0 failed
```

One earlier attempt ran multiple Cargo verification commands concurrently.
All 63 non-doc tests passed, but the `_native` doctest process raced with
parallel build artifacts and reported `can't find crate for pyo3`. The final
Rust verification above was rerun serially from the same source and passed,
including both doc-test suites.

### Drift, lint, formatting, and diff hygiene

All of the following completed with exit `0`:

```text
rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo clippy -p kiteframe-contract -p kiteframe-py \
  -p kiteframe-schema --all-targets -- -D warnings

rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo run -p kiteframe-schema -- --check schemas/v1alpha1

rtk env -u CONDA_PREFIX PYO3_PYTHON=/opt/homebrew/bin/python3 \
  cargo run -p kiteframe-schema -- \
  --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi

rtk cargo fmt --all -- --check
rtk git diff --check
```

## Self-review

- Ownership: every Python-visible value is an `Arc`-owned projection of a Rust
  contract value.
- Construction: only `CatalogRequest.default()` is exposed; none of the four
  classes has a public constructor.
- Trace safety: the approved default factory creates fresh nonzero W3C trace
  and parent IDs rather than reusing a global correlation identifier.
- Mutation: projection classes are frozen, have no `__dict__`, reject writes to
  real existing properties, return tuple collections, and cannot be pickled.
- Canonicality: all four new loaders reject leading whitespace, alternate key
  order, and any other byte representation that differs from Rust canonical
  JSON.
- Validation ordering: locked schemas run on generic JSON before typed Rust
  deserialization; named Rust tests distinguish `LockedSchema` from
  `Contract`.
- Catalog integrity: a schema-valid catalog with a forged digest is rejected by
  the existing Rust catalog validator.
- Request integrity: schema-valid requests still pass through validated trace,
  evidence, ID, selector, admission, and capability constructors.
- Diagnostics: invalid provider fields do not appear in exception text or
  `diagnostics_json`; callers receive stable `KF-CAP-002`.
- Stub drift: generated classes are final, read-only, nonconstructible, and
  expose only the approved static factory.
- Schema drift: the catalog-request schema is generated from the Rust type and
  embedded at compile time.
- Scope: no HTTP client, provider protocol, runtime adapter, policy engine,
  alternate model, registry integration, or route was added.

## Concerns

1. `schemas/v1alpha1/catalog-request.schema.json` was an implicit dependency of
   the amended requirement but was omitted from its enumerated file list. It is
   included because `load_catalog_request(...)` cannot validate against a
   corresponding checked-in locked schema otherwise.
2. The host's ambient `CONDA_PREFIX=/opt/anaconda3` selects an incompatible
   Python for PyO3, while uv uses the nested CPython 3.13 environment. As in
   earlier Wave 3 tasks, reliable local commands unset `CONDA_PREFIX`; direct
   Cargo/PyO3 commands also set
   `PYO3_PYTHON=/opt/homebrew/bin/python3`.
3. Canonical-byte enforcement is intentionally limited to the four new
   request/catalog loaders. Changing the pre-existing Task 3 response loaders
   would be a separate behavior change and initially broke their established
   schema-phase tests, so it was not smuggled into this task.
