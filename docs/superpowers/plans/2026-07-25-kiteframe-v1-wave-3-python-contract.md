# Kiteframe V1 Wave 3 Python Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose immutable Rust-owned contracts to Python, provide an instance-scoped trusted component registry, and implement the standardized capability-provider client without allowing Python to reinterpret or forge portable IR.

**Architecture:** The `kiteframe-py` extension wraps `Arc`-owned Rust values in frozen PyO3 classes with no Python constructor. All package resolution and canonical IR deserialization enter through Rust validation. Pure Python code supplies deployment-owned object registration and an async HTTP client, but every provider response is converted into a Rust-owned immutable value before an adapter can consume it.

**Tech Stack:** Rust 1.97.1, Python 3.11+, maturin, PyO3, pyo3-stub-gen, uv, pytest, pytest-asyncio, httpx, W3C Trace Context.

## Global Constraints

- Python cannot construct a partially valid `ResolvedAgent`; it may only receive one from successful Rust resolution or canonical IR deserialization that passes the same Rust validation.
- Python projections are immutable and do not duplicate portable validation or feature-negotiation policy.
- Generated JSON Schemas and Python type stubs derive from Rust-owned types.
- Registry symbols resolve only against deployment-populated `ComponentRegistry` values; manifests and bindings never supply import paths or executable expressions.
- An absent symbol, duplicate symbol, or wrong-kind symbol fails with `KF-RUNTIME-001` before runtime construction.
- Registries are instance-scoped and frozen before compilation; no process-global mutable registry is permitted.
- Provider redirects are disabled, TLS is required outside an explicit in-memory test transport, and response bodies always use the stable Kiteframe contract.
- `traceparent` and `tracestate` propagate. `baggage` is allowlisted and never carries credentials, prompts, arguments, results, or authorization tuples.
- Provider output is validated against Rust-owned response types and locked JSON schemas before an adapter receives it.

---

## File Structure

```text
crates/kiteframe-contract/src/
└── service.rs                                 # Admission, invocation, status, trace, grant contracts
crates/kiteframe-py/
├── Cargo.toml
└── src/
    ├── lib.rs                                 # _native module and exported functions
    ├── error.rs                               # DiagnosticError conversion with redacted payload
    ├── ir.rs                                  # Frozen ResolvedAgent and child projections
    ├── service.rs                             # Frozen grant/outcome/status projections
    └── validate.rs                            # Canonical IR and provider-response entrypoints
python/kiteframe/
├── pyproject.toml
├── uv.lock
├── README.md
├── src/kiteframe/
│   ├── __init__.py                            # Public contract re-exports
│   ├── _native.pyi                            # Generated extension stubs
│   ├── py.typed
│   ├── diagnostics.py                         # Python exception projection only
│   ├── registry.py                            # Mutable builder and frozen instance registry
│   └── provider/
│       ├── __init__.py
│       ├── protocols.py                       # Adapter-facing Protocols
│       ├── trace.py                           # W3C header and baggage allowlist logic
│       └── http.py                            # Async standardized HTTP client
└── tests/
    ├── test_native_immutability.py
    ├── test_native_golden.py
    ├── test_registry.py
    └── provider/test_http_client.py
schemas/v1alpha1/
├── admission-request.schema.json
├── capability-grant-set.schema.json
├── invocation-request.schema.json
├── invocation-outcome.schema.json
└── invocation-status.schema.json
```

### Task 1: Add Rust-owned service, grant, invocation, and status contracts

**Files:**
- Create: `crates/kiteframe-contract/src/service.rs`
- Modify: `crates/kiteframe-contract/src/lib.rs`
- Modify: `crates/kiteframe-schema/src/main.rs`
- Create: `schemas/v1alpha1/admission-request.schema.json`
- Create: `schemas/v1alpha1/capability-grant-set.schema.json`
- Create: `schemas/v1alpha1/invocation-request.schema.json`
- Create: `schemas/v1alpha1/invocation-outcome.schema.json`
- Create: `schemas/v1alpha1/invocation-status.schema.json`
- Test: `crates/kiteframe-contract/tests/service_contract.rs`

**Interfaces:**
- Consumes: Wave 2 descriptors, locked capabilities, digests, diagnostics, and resolved requirements.
- Produces: `CatalogRequest`, `AdmissionRequest`, `CapabilityGrantSet`, `CapabilityGrant`, `InvocationRequest`, `InvocationOutcome`, `InvocationStatus`, `TraceContext`, `EvidenceReferences`, and `DelegationAncestry`.

- [ ] **Step 1: Write failing service-contract tests**

```rust
#[test]
fn grant_set_is_time_bounded_and_not_a_bearer_credential() {
    let schema = serde_json::to_string(&schemars::schema_for!(CapabilityGrantSet)).unwrap();
    assert!(schema.contains("issuedAt"));
    assert!(schema.contains("expiresAt"));
    assert!(schema.contains("policyRevision"));
    assert!(!schema.contains("token"));
    assert!(!schema.contains("credential"));
}

#[test]
fn effectful_invocation_requires_an_idempotency_key() {
    let descriptor = effectful_descriptor();
    let request = invocation_request_without_key();
    let errors = request.validate_against(&descriptor).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn outcome_unknown_requires_status_first_retry() {
    let outcome = InvocationOutcome::OutcomeUnknown {
        invocation_id: InvocationId::new("inv-1").unwrap(),
        diagnostic: Diagnostic::outcome_unknown("status is required"),
    };
    assert_eq!(outcome.diagnostic().unwrap().retry, RetryClass::StatusFirst);
}
```

- [ ] **Step 2: Run the service tests**

Run: `rtk cargo test -p kiteframe-contract --test service_contract`

Expected: FAIL because the service contracts are undefined.

- [ ] **Step 3: Add exact immutable service types**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityGrantSet {
    admission_id: AdmissionId,
    actor: ActorRef,
    agent: AgentRef,
    task: TaskRef,
    session: SessionRef,
    policy_revision: PolicyRevision,
    catalog_digest: Sha256Digest,
    issued_at: Timestamp,
    expires_at: Timestamp,
    grants: Vec<CapabilityGrant>,
    grant_digest: Sha256Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "status", deny_unknown_fields)]
pub enum InvocationOutcome {
    Succeeded { invocation_id: InvocationId, result: serde_json::Value },
    Failed { invocation_id: InvocationId, error: StableCapabilityError },
    Denied { invocation_id: InvocationId, diagnostic: Diagnostic },
    Suspended { invocation_id: InvocationId, suspension: Suspension },
    Deferred { invocation_id: InvocationId },
    OutcomeUnknown { invocation_id: InvocationId, diagnostic: Diagnostic },
}
```

`CapabilityGrantSet` is constructed only through digest-validating `try_new`/deserialization entrypoints and exposes read-only getters. `InvocationStatus` uses exactly `pending`, `suspended`, `succeeded`, `failed`, `denied`, and `outcome_unknown`. `InvocationRequest` carries exact capability identity, selected resource, typed arguments, preconditions, optional idempotency key, evidence references, admission ID, and trace context.

- [ ] **Step 4: Validate grant and request invariants**

Reject `expires_at <= issued_at`, duplicate capability versions, selectors broader than the resolved requirement, raw evidence payloads in place of references, non-allowlisted baggage keys, and an idempotency key on a descriptor whose contract is `None`.

- [ ] **Step 5: Generate and verify service schemas**

Run: `rtk cargo run -p kiteframe-schema -- schemas/v1alpha1`

Expected: writes the five new service schemas.

Run: `rtk cargo test -p kiteframe-contract --test service_contract`

Expected: PASS.

- [ ] **Step 6: Commit the provider wire contracts**

```bash
rtk git add crates/kiteframe-contract crates/kiteframe-schema schemas/v1alpha1
rtk git commit -m "feat: define capability provider contracts"
```

### Task 2: Build the maturin package and non-constructible PyO3 IR projections

**Files:**
- Create: `crates/kiteframe-py/Cargo.toml`
- Create: `crates/kiteframe-py/src/lib.rs`
- Create: `crates/kiteframe-py/src/error.rs`
- Create: `crates/kiteframe-py/src/ir.rs`
- Create: `crates/kiteframe-py/src/validate.rs`
- Modify: `Cargo.toml`
- Create: `python/kiteframe/pyproject.toml`
- Create: `python/kiteframe/src/kiteframe/__init__.py`
- Create: `python/kiteframe/src/kiteframe/diagnostics.py`
- Create: `python/kiteframe/src/kiteframe/py.typed`
- Test: `python/kiteframe/tests/test_native_immutability.py`

**Interfaces:**
- Consumes: `ResolvedAgent`, resolver entrypoints, canonical JSON, and diagnostics.
- Produces: `_native.ResolvedAgent`, `_native.ResolvedCapabilityRequirement`, `_native.ResolvedSubagent`, `resolve_package(...)`, and `load_resolved_agent(...)`.

- [ ] **Step 1: Write failing immutability tests**

```python
import dataclasses
import pytest
from kiteframe import ResolvedAgent, load_resolved_agent


def test_resolved_agent_has_no_public_constructor() -> None:
    with pytest.raises(TypeError):
        ResolvedAgent()  # type: ignore[call-arg]


def test_resolved_agent_fields_cannot_be_reassigned(golden_ir: bytes) -> None:
    resolved = load_resolved_agent(golden_ir)
    with pytest.raises(AttributeError):
        resolved.resolved_digest = "0" * 64  # type: ignore[misc]


def test_noncanonical_ir_is_rejected(golden_ir: bytes) -> None:
    spaced = b" " + golden_ir
    with pytest.raises(Exception, match="canonical"):
        load_resolved_agent(spaced)
```

- [ ] **Step 2: Run Python tests before the extension exists**

Run from `python/kiteframe`: `rtk uv run --project . maturin develop`

Expected: FAIL because the Python project and extension crate do not exist.

- [ ] **Step 3: Add frozen PyO3 classes with no `#[new]` method**

```rust
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ResolvedAgent"
)]
pub struct PyResolvedAgent {
    inner: Arc<ResolvedAgent>,
}

#[pymethods]
impl PyResolvedAgent {
    #[getter]
    fn package_name(&self) -> &str {
        self.inner.package_identity().name().as_str()
    }

    #[getter]
    fn resolved_digest(&self) -> String {
        self.inner.resolved_digest().to_string()
    }

    fn canonical_json(&self) -> PyResult<Vec<u8>> {
        canonical_json(self.inner.as_ref()).map_err(PyErr::from)
    }
}
```

Return tuples for collections and frozen child PyO3 objects for structured values. Never return a mutable dictionary that can be written back into Rust.

- [ ] **Step 4: Add validated native entrypoints and redacted exceptions**

```rust
#[pyfunction]
fn load_resolved_agent(bytes: &[u8]) -> PyResult<PyResolvedAgent> {
    let resolved: ResolvedAgent = serde_json::from_slice(bytes)
        .map_err(ir_parse_error)?;
    resolved.validate().map_err(PyErr::from)?;
    let canonical = canonical_json(&resolved).map_err(PyErr::from)?;
    if canonical != bytes {
        return Err(PyValueError::new_err("ResolvedAgent JSON is not canonical"));
    }
    Ok(PyResolvedAgent { inner: Arc::new(resolved) })
}
```

Map `Vec<Diagnostic>` to a `KiteframeDiagnosticError` whose public `.diagnostics_json` contains only the already-redacted structured diagnostics.

- [ ] **Step 5: Build and run immutability tests**

Run from `python/kiteframe`: `rtk uv run --project . maturin develop`

Expected: builds `kiteframe._native` against Python 3.11+.

Run: `rtk uv run --project python/kiteframe pytest tests/test_native_immutability.py -q`

Expected: PASS.

- [ ] **Step 6: Commit the immutable native boundary**

```bash
rtk git add Cargo.toml crates/kiteframe-py python/kiteframe
rtk git commit -m "feat: expose immutable resolved agents to python"
```

### Task 3: Generate Python stubs and prove cross-language golden parity

**Files:**
- Create: `crates/kiteframe-py/src/service.rs`
- Modify: `crates/kiteframe-py/src/lib.rs`
- Modify: `crates/kiteframe-py/src/ir.rs`
- Create: `python/kiteframe/src/kiteframe/_native.pyi`
- Create: `python/kiteframe/tests/test_native_golden.py`
- Create: `python/kiteframe/tests/test_stub_drift.py`
- Modify: `crates/kiteframe-schema/src/main.rs`

**Interfaces:**
- Consumes: Rust service contracts and Wave 2 golden fixtures.
- Produces: frozen Python `CapabilityGrantSet`, `InvocationOutcome`, and `InvocationStatus`; generated `_native.pyi`.

- [ ] **Step 1: Write failing golden and stub-drift tests**

```python
from pathlib import Path
from kiteframe import load_resolved_agent


def test_python_round_trip_preserves_exact_golden_bytes() -> None:
    expected = Path("tests/fixtures/resolved/support-agent.json").read_bytes()
    resolved = load_resolved_agent(expected)
    assert resolved.canonical_json() == expected


def test_digest_tuple_matches_rust_fixture() -> None:
    expected = load_digest_fixture("support-agent.digests.json")
    resolved = load_support_agent()
    assert resolved.portable_digest == expected["portableDigest"]
    assert resolved.lock_digest == expected["lockDigest"]
    assert resolved.binding_digest == expected["bindingDigest"]
    assert resolved.resolved_digest == expected["resolvedDigest"]
```

- [ ] **Step 2: Run golden tests**

Run: `rtk uv run --project python/kiteframe pytest tests/test_native_golden.py tests/test_stub_drift.py -q`

Expected: FAIL because service projections and generated stubs are incomplete.

- [ ] **Step 3: Add frozen service projections**

Expose scalar getters, tuple grants, stable outcome variants, and `canonical_json()` for all provider response types. Do not expose Python setters, `__dict__`, pickle reconstruction, or constructors.

```rust
#[pyclass(frozen, immutable_type, module = "kiteframe._native")]
pub struct CapabilityGrantSet {
    inner: Arc<ContractCapabilityGrantSet>,
}
```

- [ ] **Step 4: Generate stubs from annotated PyO3 exports**

Run: `rtk cargo run -p kiteframe-schema -- --python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: the stub includes read-only properties and omits `__init__` for Rust-owned values.

Run: `rtk cargo run -p kiteframe-schema -- --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: PASS with no drift.

- [ ] **Step 5: Run cross-language tests**

Run: `rtk uv run --project python/kiteframe pytest tests/test_native_golden.py tests/test_stub_drift.py -q`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-py`

Expected: PASS for Rust-side PyO3 conversion tests.

- [ ] **Step 6: Commit stubs and service projections**

```bash
rtk git add crates/kiteframe-py crates/kiteframe-schema python/kiteframe/src/kiteframe/_native.pyi python/kiteframe/tests
rtk git commit -m "test: freeze python contract projections"
```

### Task 4: Add an instance-scoped component registry that freezes before compile

**Files:**
- Create: `python/kiteframe/src/kiteframe/registry.py`
- Modify: `python/kiteframe/src/kiteframe/__init__.py`
- Test: `python/kiteframe/tests/test_registry.py`

**Interfaces:**
- Consumes: Rust `ComponentKind`, `RegistrySymbol`, and `KF-RUNTIME-001`.
- Produces: `ComponentRegistry.register(kind, symbol, value)`, `ComponentRegistry.freeze()`, and `FrozenComponentRegistry.resolve(kind, symbol)`.

- [ ] **Step 1: Write failing registry tests**

```python
import pytest
from kiteframe.registry import ComponentKind, ComponentRegistry


def test_duplicate_registration_is_rejected_without_overwrite() -> None:
    registry = ComponentRegistry()
    first = object()
    registry.register(ComponentKind.MODEL, "models.primary", first)
    with pytest.raises(ValueError, match="already registered"):
        registry.register(ComponentKind.MODEL, "models.primary", object())
    assert registry.freeze().resolve(ComponentKind.MODEL, "models.primary") is first


def test_wrong_kind_and_absent_symbol_use_component_unresolved() -> None:
    registry = ComponentRegistry()
    registry.register(ComponentKind.BACKEND, "backends.workspace", object())
    frozen = registry.freeze()
    with pytest.raises(Exception) as wrong:
        frozen.resolve(ComponentKind.MODEL, "backends.workspace")
    assert wrong.value.code == "KF-RUNTIME-001"


def test_frozen_registry_cannot_be_mutated() -> None:
    registry = ComponentRegistry()
    frozen = registry.freeze()
    with pytest.raises(RuntimeError, match="frozen"):
        registry.register(ComponentKind.MODEL, "models.late", object())
```

- [ ] **Step 2: Run registry tests**

Run: `rtk uv run --project python/kiteframe pytest tests/test_registry.py -q`

Expected: FAIL because `registry.py` does not exist.

- [ ] **Step 3: Implement builder-to-frozen transition**

```python
@dataclass(frozen=True, slots=True)
class RegistryKey:
    kind: ComponentKind
    symbol: str


class ComponentRegistry:
    def __init__(self) -> None:
        self._entries: dict[RegistryKey, object] = {}
        self._symbols: dict[str, ComponentKind] = {}
        self._frozen = False

    def register(self, kind: ComponentKind, symbol: str, value: object) -> None:
        if self._frozen:
            raise RuntimeError("component registry is frozen")
        key = RegistryKey(kind, validate_registry_symbol(symbol))
        if symbol in self._symbols:
            raise ValueError(f"registry symbol {symbol!r} is already registered")
        self._entries[key] = value
        self._symbols[symbol] = kind

    def freeze(self) -> FrozenComponentRegistry:
        self._frozen = True
        return FrozenComponentRegistry(MappingProxyType(dict(self._entries)),
                                       MappingProxyType(dict(self._symbols)))
```

`FrozenComponentRegistry.resolve` checks the symbol map first so a wrong kind can be distinguished from absence without revealing registered object details.

- [ ] **Step 4: Run registry tests**

Run: `rtk uv run --project python/kiteframe pytest tests/test_registry.py -q`

Expected: PASS.

- [ ] **Step 5: Prove registry isolation under concurrency**

Add a test that freezes two registries with the same symbol bound to distinct objects, resolves them in 100 concurrent tasks, and asserts no cross-registry value appears.

Run: `rtk uv run --project python/kiteframe pytest tests/test_registry.py -q`

Expected: PASS.

- [ ] **Step 6: Commit the trusted registry**

```bash
rtk git add python/kiteframe/src/kiteframe python/kiteframe/tests/test_registry.py
rtk git commit -m "feat: add frozen component registry"
```

### Task 5: Add native provider request and catalog boundary projections

**Files:**
- Modify: `crates/kiteframe-py/src/service.rs`
- Modify: `crates/kiteframe-py/src/lib.rs`
- Modify: `crates/kiteframe-py/src/error.rs`
- Modify: `crates/kiteframe-schema/src/main.rs`
- Modify: `python/kiteframe/src/kiteframe/__init__.py`
- Modify: `python/kiteframe/src/kiteframe/_native.pyi`
- Create: `crates/kiteframe-py/tests/provider_request_projection.rs`
- Create: `python/kiteframe/tests/test_native_provider_requests.py`

**Interfaces:**
- Consumes: Rust-owned `CatalogRequest`, `AdmissionRequest`, `InvocationRequest`, `CapabilityCatalog`, canonical JSON, locked schemas, diagnostics, and the existing schema-first native response boundary.
- Produces: frozen native `CatalogRequest`, `AdmissionRequest`, `InvocationRequest`, and `CapabilityCatalog`; `CatalogRequest.default()`; and canonical-only `load_catalog_request(...)`, `load_admission_request(...)`, `load_invocation_request(...)`, and `load_capability_catalog(...)` entrypoints.

- [ ] **Step 1: Write failing native request and catalog boundary tests**

```python
import pytest
from kiteframe import (
    CatalogRequest,
    load_admission_request,
    load_capability_catalog,
    load_invocation_request,
)


def test_catalog_request_is_factory_only_and_canonical() -> None:
    with pytest.raises(TypeError):
        CatalogRequest()  # type: ignore[call-arg]
    assert CatalogRequest.default().canonical_json()


def test_provider_requests_are_frozen_and_reject_noncanonical_bytes(valid_invocation: bytes) -> None:
    request = load_invocation_request(valid_invocation)
    with pytest.raises(AttributeError):
        request.invocation_id = "forged"  # type: ignore[misc]
    with pytest.raises(Exception, match="canonical"):
        load_invocation_request(b" " + valid_invocation)


def test_catalog_loader_rejects_schema_invalid_output(schema_invalid_catalog: bytes) -> None:
    with pytest.raises(Exception) as error:
        load_capability_catalog(schema_invalid_catalog)
    assert error.value.code == "KF-CAP-002"
```

- [ ] **Step 2: Run the focused boundary tests**

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_provider_requests.py -q`

Expected: FAIL because native request/catalog projections and loaders do not exist.

- [ ] **Step 3: Add frozen request/catalog projections and canonical loaders**

Expose only read-only scalar/tuple properties plus `canonical_json() -> bytes`. `CatalogRequest.default()` is the sole public factory for its empty request; no native request/catalog class receives a public `#[new]` method. Every `load_*` entrypoint must deserialize only canonical bytes, validate the corresponding checked-in locked schema before typed Rust deserialization, and map parse, schema, and contract failures to redacted `KiteframeDiagnosticError` with stable code `KF-CAP-002`.

`CapabilityCatalog` validation uses the existing Rust catalog validator; Python must not parse descriptors, negotiate features, compute catalog digests, or select versions. `AdmissionRequest` and `InvocationRequest` preserve the Rust request invariants already enforced by their typed deserializers.

- [ ] **Step 4: Generate and check native stubs**

Run: `rtk cargo run -p kiteframe-schema -- --python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Run: `rtk cargo run -p kiteframe-schema -- --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: all request/catalog values and loaders appear as immutable/read-only native APIs; no public value constructors appear.

- [ ] **Step 5: Run native request/catalog verification**

Run from `python/kiteframe`: `rtk uv run --project . pytest tests/test_native_provider_requests.py -q`

Expected: PASS.

Run: `rtk cargo test -p kiteframe-py --test provider_request_projection`

Expected: PASS for canonical-byte rejection and locked-schema-before-contract ordering.

- [ ] **Step 6: Commit the native provider dependency boundary**

```bash
rtk git add docs/superpowers/plans/2026-07-25-kiteframe-v1-wave-3-python-contract.md crates/kiteframe-py crates/kiteframe-schema python/kiteframe/src/kiteframe python/kiteframe/tests
rtk git commit -m "feat: expose native provider request contracts"
```

### Task 6: Define adapter-facing provider protocols and a strict async HTTP client

**Files:**
- Create: `python/kiteframe/src/kiteframe/provider/__init__.py`
- Create: `python/kiteframe/src/kiteframe/provider/protocols.py`
- Create: `python/kiteframe/src/kiteframe/provider/trace.py`
- Create: `python/kiteframe/src/kiteframe/provider/http.py`
- Create: `python/kiteframe/tests/provider/test_http_client.py`
- Modify: `python/kiteframe/pyproject.toml`

**Interfaces:**
- Consumes: frozen native request/response values and diagnostics.
- Produces: `CatalogProvider`, `AdmissionProvider`, `CapabilityInvoker`, `AuditSink` Protocols and `ProviderHttpClient`.

- [ ] **Step 1: Write failing client security and parsing tests**

```python
@pytest.mark.asyncio
async def test_client_does_not_follow_redirects() -> None:
    transport = httpx.MockTransport(
        lambda request: httpx.Response(307, headers={"location": "https://evil.invalid"})
    )
    client = ProviderHttpClient("https://provider.test", transport=transport)
    with pytest.raises(ProviderTransportError, match="redirect"):
        await client.catalog(CatalogRequest.default())


@pytest.mark.asyncio
async def test_invalid_result_never_reaches_caller() -> None:
    transport = fixture_transport("invalid-invocation-outcome.json")
    client = ProviderHttpClient("https://provider.test", transport=transport)
    with pytest.raises(KiteframeDiagnosticError) as error:
        await client.invoke(valid_invocation_request())
    assert error.value.code == "KF-CAP-002"


def test_baggage_drops_sensitive_and_unlisted_keys() -> None:
    headers = trace_headers(
        traceparent=VALID_TRACEPARENT,
        tracestate="vendor=value",
        baggage={"tenant.id": "t1", "prompt": "secret", "authorization": "tuple"},
        baggage_allowlist=frozenset({"tenant.id"}),
    )
    assert headers["baggage"] == "tenant.id=t1"
```

- [ ] **Step 2: Run provider client tests**

Run: `rtk uv run --project python/kiteframe pytest tests/provider/test_http_client.py -q`

Expected: FAIL because provider protocols and client do not exist.

- [ ] **Step 3: Add exact structural protocols**

```python
class AdmissionProvider(Protocol):
    async def admit(self, request: AdmissionRequest) -> CapabilityGrantSet: ...


class CapabilityInvoker(Protocol):
    async def invoke(self, request: InvocationRequest) -> InvocationOutcome: ...

    async def status(self, invocation_id: str) -> InvocationStatus: ...
```

Protocols accept and return native immutable classes; they do not accept unvalidated dictionaries.

- [ ] **Step 4: Implement strict HTTP behavior**

```python
class ProviderHttpClient:
    def __init__(
        self,
        base_url: str,
        *,
        transport: httpx.AsyncBaseTransport | None = None,
        baggage_allowlist: frozenset[str] = frozenset(),
    ) -> None:
        require_https(base_url, allow_mock=transport is not None)
        self._client = httpx.AsyncClient(
            base_url=base_url,
            follow_redirects=False,
            verify=True,
            transport=transport,
            timeout=httpx.Timeout(10.0),
        )
        self._baggage_allowlist = baggage_allowlist
```

Implement only the four V1 routes. Reject every 3xx response, cap response bodies at the provider-response limit, parse structured diagnostic bodies for non-2xx statuses, and send canonical JSON bytes from native request objects.

- [ ] **Step 5: Run client tests**

Run: `rtk uv run --project python/kiteframe pytest tests/provider/test_http_client.py -q`

Expected: PASS for catalog ETag, admission, invocation, status, redirects, TLS requirement, body limits, W3C propagation, baggage filtering, structured diagnostics, and invalid response bodies.

- [ ] **Step 6: Commit the provider client boundary**

```bash
rtk git add python/kiteframe/src/kiteframe/provider python/kiteframe/tests/provider python/kiteframe/pyproject.toml python/kiteframe/uv.lock
rtk git commit -m "feat: add capability provider client"
```

### Task 7: Gate cross-language schemas, redaction, and package quality

**Files:**
- Create: `python/kiteframe/tests/test_diagnostic_redaction.py`
- Create: `python/kiteframe/tests/test_service_round_trip.py`
- Modify: `.github/workflows/ci.yml`
- Create: `python/kiteframe/README.md`

**Interfaces:**
- Consumes: all Wave 3 APIs.
- Produces: CI proof that Python cannot mutate, forge, or reinterpret Rust contracts; no new public interface.

- [ ] **Step 1: Write the redaction and mutation corpus**

```python
@pytest.mark.parametrize(
    "secret",
    ["sk-live-secret", "raw prompt text", "user@example.test", "tuple:user:admin"],
)
def test_native_diagnostics_never_expose_sensitive_input(secret: str) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        load_resolved_agent(invalid_ir_containing(secret))
    public = error.value.diagnostics_json
    assert secret not in public
    assert "KF-PKG-001" in public


def test_python_cannot_mutate_grant_then_reserialize(valid_grant_json: bytes) -> None:
    grant = load_capability_grant_set(valid_grant_json)
    with pytest.raises(AttributeError):
        grant.grants += ()  # type: ignore[misc]
```

- [ ] **Step 2: Run the new tests**

Run: `rtk uv run --project python/kiteframe pytest tests/test_diagnostic_redaction.py tests/test_service_round_trip.py -q`

Expected: PASS after every native projection uses frozen classes and redacted errors.

- [ ] **Step 3: Run all Rust and Python contract checks**

Run: `rtk cargo fmt --all --check`

Expected: PASS.

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `rtk cargo test --workspace --all-features`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe pytest -q`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe ruff check src tests`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe pyright`

Expected: PASS.

- [ ] **Step 4: Verify generated artifacts are clean**

Run: `rtk cargo run -p kiteframe-schema -- --check schemas/v1alpha1`

Expected: PASS.

Run: `rtk cargo run -p kiteframe-schema -- --check-python-stubs python/kiteframe/src/kiteframe/_native.pyi`

Expected: PASS.

- [ ] **Step 5: Document the Python trust boundary**

Document that `kiteframe` contains no runtime adapter, no policy engine, and no mutable alternate IR model. Show registry construction followed by `freeze()`, and show provider calls accepting native request values.

- [ ] **Step 6: Commit the Wave 3 verification gate**

```bash
rtk git add .github/workflows/ci.yml python/kiteframe
rtk git commit -m "test: gate cross-language contract integrity"
```

## Wave 3 Exit Criteria

- Every Python-visible IR, grant, outcome, and status object is frozen, non-constructible, and backed by a validated Rust value.
- Canonical IR and all four digest values round-trip byte-for-byte through Python.
- Generated stubs and JSON Schemas fail CI on drift.
- A frozen component registry rejects duplicate, absent, and wrong-kind symbols and does not leak values across instances.
- The HTTP client implements only the four standardized routes, never follows redirects, requires TLS, filters baggage, and parses all responses through Rust.
- Provider-native invalid outputs and sensitive diagnostic inputs never reach adapter code or public error text.
