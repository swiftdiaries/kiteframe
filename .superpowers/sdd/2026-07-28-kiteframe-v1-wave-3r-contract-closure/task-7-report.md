# Task 7 Report: Deployment-Owned Provider Authentication

## Status

Implemented the deployment-owned Python authentication seam for all four V1
provider operations. Credentials remain outside native Rust values, canonical
request bodies, trace baggage, diagnostics, schemas, and stubs.

## Delivered

- Added frozen, slotted `ProviderAuthRequest` metadata and the runtime-checkable
  async `ProviderAuthenticator` protocol.
- Added an explicit, case-normalized ASCII credential-header allowlist.
- Rejects normalized duplicate names, invalid names and values, unlisted
  headers, and credential control over request framing, trace, baggage, cookie,
  and proxy headers.
- Calls the authenticator immediately before every catalog, admission,
  invocation, and status request, then removes credential values from local
  request state after the request completes.
- Redacts authenticator and transport failures without retaining raw exception
  causes or contexts.
- Passes an optional deployment-built `ssl.SSLContext` to HTTPX as `verify`
  while retaining `trust_env=False`; runtime validation rejects booleans,
  strings, and arbitrary objects that could otherwise reach HTTPX as an
  unsafe `verify` value.
- Documents deployment ownership, credential boundaries, and client-certificate
  setup. Mock transports remain explicitly test-only.
- Did not add provider-side credential verification or any Task 8 gate.

## TDD and Verification

- RED: `pytest tests/provider/test_authentication.py -q` produced 30 expected
  failures because the authenticator and TLS constructor seams did not exist.
- GREEN: focused authentication tests passed after implementation.
- Regression: `pytest tests/provider/test_authentication.py
  tests/provider/test_http_client.py -q` passed (89 tests).
- Additional RED/GREEN: a non-ASCII credential value initially escaped as a raw
  `UnicodeEncodeError`; fixed-message validation now rejects it before HTTPX.
- Security review found that `tls_context=False` could disable HTTPX
  verification despite the annotated API. A failing regression reproduced the
  escape; runtime `ssl.SSLContext` enforcement now closes it.
- `pyright`: 0 errors, 0 warnings, 0 information.
- Ruff over the Task 7 changed Python files: passed.

## Known Cumulative Baseline Issues Kept Out of Scope

- Full `ruff check src tests` has pre-existing import-order failures in
  `src/kiteframe/__init__.py` and `src/kiteframe/provider/protocols.py` from the
  earlier catalog work.
- Full Python collection runs, but five pre-existing tests fail: three stale
  catalog fixtures do not contain the new freshness fields, and two native-stub
  checks hit the known `pyo3-stub-gen` tool mismatch/stale generated stub.
- A temporary `maturin develop` compatibility build was used only to verify the
  Python tests. Its generated native extension and caches are not committed.
