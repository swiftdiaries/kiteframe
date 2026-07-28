# Wave 3 Python Contract Final Fix Report

Date: 2026-07-28

Status: implementation complete; all required local gates pass

Base reviewed: `32fa2b432f92708d6dce86d392dda8899b70062f`

Wave 2 base: `85661ee5454d788ffe7ba93898f8697a00e06038`

Commit intent: one cohesive commit named `fix: harden Wave 3 provider contract boundary`

## Scope reviewed

The remediation started only after reading the complete Wave 3 implementation
plan, its progress ledger, all seven task briefs and reports, and the whole-branch
final review. The working tree was verified as the linked
`codex/kiteframe-v1-wave-3` worktree before edits.

The final review's two Critical findings, five Important findings, and the
requested minor regressions were treated as one security-boundary fix:

1. correlate every admission, invocation, and status response in Rust;
2. make provider-body redaction exception-total, including invalid 2xx bodies;
3. compare canonical bytes only after locked-schema validation and typed Rust
   construction/normalization;
4. prevent serde from bypassing constructor invariants;
5. move the canonical W3C trace-context subset into the Rust contract and its
   generated schemas;
6. reject non-identity response encodings before reading response bytes;
7. expose the provider response contracts and loaders through the supported
   top-level Python package;
8. make generated stub output deterministic with exactly one terminal newline;
9. cover the distinct wrong-kind resolver-redaction path; and
10. run the Python package gate on all declared Python versions 3.11, 3.12, and
    3.13 in CI.

## Contract decisions

### Rust remains authoritative

`CapabilityGrantSet::validate_against` now rejects actor, agent, task, or
session mismatches; unrequested capability grants; and resource selectors that
are broader than the matching request. `InvocationOutcome::validate_against`
and `InvocationStatus::validate_invocation_id` reject another invocation's
response. The PyO3 correlated loaders call these Rust APIs before constructing
Python projections.

The admission contract has no caller-supplied admission ID, so correlation uses
the identity fields and grant bounds that are actually present in
`AdmissionRequest`. A provider may omit a requested grant; downstream required
grant/session construction remains responsible for required-capability
completeness. This layer prevents authority widening and cross-request binding.

The approved V1 boundaries are unchanged:

- no `AuditSink` was added; it remains outside V1;
- `status(invocation_id)` does not propagate an ambient, cached, or invented
  trace context.

### Locked schema, typed construction, then canonical comparison

Canonical-only native loaders now:

1. parse JSON;
2. validate the embedded locked schema;
3. deserialize through the Rust contract constructors;
4. serialize the resulting typed value canonically; and
5. compare those bytes with the provider bytes.

This rejects lexically canonical payloads whose collection order or duplicate
normalization would change during typed construction, including unsorted
admission requests and capability catalogs.

### Constructor invariants cannot be bypassed

`DelegationAncestry`, `RequestedCapability`, `StableCapabilityError`, and
`Suspension` now deserialize through their validated constructors. Generated
schemas additionally express unique collection members and non-empty scalar
fields where JSON Schema can represent the invariant.

`TraceContext::try_new` now owns the same intentionally narrow canonical W3C
subset as the HTTP projection:

- version-00, lowercase traceparent with nonzero trace and parent IDs;
- ASCII tracestate of at most 512 bytes and 32 members;
- unique, grammar-valid keys and bounded values;
- no optional outer member whitespace, duplicate keys, extra `=`, control
  characters, or header breaks.

The generated request schemas carry the traceparent pattern and bounded
printable tracestate constraints. Typed Rust construction retains the stronger
semantic checks that JSON Schema cannot fully express.

### Provider bodies remain inside the redaction boundary

Provider diagnostic fields are type-checked before enum membership. Diagnostic
parsing, sanitization, sorting, and error construction are covered by a total
`Exception` fallback and a `finally` body clear. Success-body loading is
centralized so public provider methods never retain a raw response body in an
exception traceback frame, and the loader frame clears its body reference in
`finally`.

Requests explicitly advertise `Accept-Encoding: identity`. Any present
non-identity `Content-Encoding` is rejected before `aiter_bytes()` can decode
or allocate a compressed response.

## Files changed

Rust contract, native boundary, and generators:

- `crates/kiteframe-contract/src/service.rs`
- `crates/kiteframe-contract/tests/service_contract.rs`
- `crates/kiteframe-py/src/lib.rs`
- `crates/kiteframe-py/src/service.rs`
- `crates/kiteframe-py/tests/provider_request_projection.rs`
- `crates/kiteframe-schema/src/main.rs`

Python package, provider boundary, and tests:

- `python/kiteframe/src/kiteframe/__init__.py`
- `python/kiteframe/src/kiteframe/_native.pyi`
- `python/kiteframe/src/kiteframe/provider/http.py`
- `python/kiteframe/tests/provider/test_http_client.py`
- `python/kiteframe/tests/test_diagnostic_redaction.py`
- `python/kiteframe/tests/test_stub_drift.py`

Generated locked schemas:

- `schemas/v1alpha1/admission-request.schema.json`
- `schemas/v1alpha1/catalog-request.schema.json`
- `schemas/v1alpha1/invocation-outcome.schema.json`
- `schemas/v1alpha1/invocation-request.schema.json`
- `schemas/v1alpha1/invocation-status.schema.json`

CI:

- `.github/workflows/ci.yml`

## TDD evidence

### Rust invariants, canonicalization, and trace context

Initial RED:

- `rtk cargo test -p kiteframe-contract --test service_contract`: 5 failed,
  21 passed;
- `rtk cargo test -p kiteframe-py --test provider_request_projection`: 3 failed,
  5 passed.

The failures proved that constructor-invalid values, weaker trace context, and
lexically canonical but semantically noncanonical request/catalog collections
were still accepted.

GREEN after the contract and loader implementation:

- contract suite: 26 passed;
- native projection suite: 8 passed.

### Rust response correlation

Initial RED failed to compile with five missing correlation APIs. After the
Rust validation methods and correlated PyO3 loaders were implemented:

- contract suite: 30 passed;
- native projection suite: 10 passed.

The tests cover all four admission identity fields, unrequested and broader
grants, invocation outcome mismatch, and invocation status mismatch.

### Python HTTP, redaction, exports, stub, and CI

Initial focused RED:

- 15 failed, 39 passed.

The failures covered admission/invocation/status correlation, compressed
responses, unhashable diagnostic fields, deeply nested invalid diagnostics,
invalid-2xx traceback retention, public response exports/loaders, and generated
stub newline drift.

Focused GREEN:

- `rtk env -u CONDA_PREFIX uv run --project . pytest
  tests/provider/test_http_client.py tests/test_diagnostic_redaction.py
  tests/test_stub_drift.py -q`: 54 passed;
- `rtk env -u CONDA_PREFIX uv run --project . ruff check src tests`: passed;
- `rtk env -u CONDA_PREFIX uv run --project . pyright`: 0 errors, 0 warnings.

## Fresh full verification

The final gate was run serially against the completed implementation:

| Command | Result |
| --- | --- |
| `rtk cargo fmt --all --check` | passed |
| `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed |
| `rtk cargo test --workspace --all-features` | 191 passed across 29 suites |
| `rtk cargo run -p kiteframe-schema -- --check schemas/v1alpha1` | passed |
| `rtk cargo run -p kiteframe-schema -- --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi` | passed |
| `rtk env -u CONDA_PREFIX uv run --project . maturin develop` from `python/kiteframe` | editable CPython 3.13 extension built and installed |
| `rtk env -u CONDA_PREFIX uv run --project . pytest -q` from `python/kiteframe` | 95 passed |
| `rtk env -u CONDA_PREFIX uv run --project . ruff check src tests` from `python/kiteframe` | passed |
| `rtk env -u CONDA_PREFIX uv run --project . pyright` from `python/kiteframe` | 0 errors, 0 warnings |
| `rtk ruby -e 'require "yaml"; YAML.parse_file(".github/workflows/ci.yml")'` | valid YAML |
| `rtk git diff --check` | passed |
| `rtk git diff --check 85661ee5454d788ffe7ba93898f8697a00e06038` | passed |

The editable extension and all pytest-created `__pycache__` directories were
removed after the final Python gate. They are not part of the commit.

## Independent review

An independent read-only final-diff review checked the completed working tree
against the complete plan and original whole-branch findings. It found no
Critical, Important, or actionable Minor issues and returned **ready to
commit**. The reviewer independently confirmed the Rust-owned correlation,
redaction boundary, typed canonicalization, invariant-preserving
deserialization, trace-context parity, compression rejection, public exports,
stub generation, CI matrix, and both preserved V1 exclusions.

## Deferred nonblocking observations

The earlier review's two calibrated minor observations remain intentionally
outside this fix:

- package resolution retains the interpreter during startup filesystem work;
- some Rust loader/error visibility exists for integration-test access.

Neither changes the provider security boundary or the Wave 3 exit criteria.
