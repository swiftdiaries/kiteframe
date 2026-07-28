# Kiteframe Python

`kiteframe` is the narrow Python trust boundary around Kiteframe's
Rust-validated contracts. Python receives immutable native projections; it
does not own a second representation of the agent IR or capability protocol.

## Trust boundary

The package provides:

- frozen, factory-only projections of resolved agents, provider requests,
  catalogs, grants, invocation outcomes, and invocation status;
- canonical JSON round trips owned by Rust;
- a deployment-owned component registry that becomes immutable before it is
  used; and
- a strict async client for the four V1 capability-provider routes.

The package deliberately contains no runtime adapter, policy engine, mutable
alternate IR, generic provider route, or V1 `AuditSink`. Provider responses
become visible to Python only after the checked-in JSON Schema and Rust
contract both accept them. Public native diagnostics contain stable,
redacted fields rather than caller or provider input.

## Freeze deployment components

Register deployment-owned objects during startup, then freeze the registry
before resolution or runtime construction:

```python
from kiteframe import ComponentKind, ComponentRegistry

registry = ComponentRegistry()
registry.register(ComponentKind.MODEL, "models.primary", deployment_model)
registry.register(
    ComponentKind.CAPABILITY_PROVIDER,
    "providers.primary",
    deployment_provider,
)

components = registry.freeze()
model = components.resolve(ComponentKind.MODEL, "models.primary")
```

Duplicate, absent, wrong-kind, malformed, and late registrations fail closed.
Registry values remain deployment objects; they are not serialized into the
portable agent contract.

## Call a capability provider

Provider methods accept native request values, not dictionaries or
adapter-local models:

```python
from kiteframe import (
    CatalogRequest,
    load_admission_request,
    load_invocation_request,
)
from kiteframe.provider import ProviderHttpClient

catalog_request = CatalogRequest.default()
admission_request = load_admission_request(canonical_admission_json)
invocation_request = load_invocation_request(canonical_invocation_json)

async with ProviderHttpClient("https://provider.example") as client:
    catalog = await client.catalog(catalog_request)
    grants = await client.admit(admission_request)
    outcome = await client.invoke(invocation_request)
    status = await client.status(outcome.invocation_id)
```

The client exposes only catalog, admission, invocation, and status. It
requires TLS for network transports, does not follow redirects, bounds
response bodies, filters baggage, and parses every successful response
through its native locked-schema loader. The V1 status lookup accepts only an
invocation ID and therefore does not infer or reuse ambient caller trace
state.

## Resolve an agent package

Resolution returns the canonical Rust value directly:

```python
from pathlib import Path

from kiteframe import resolve_package

package = Path("agent-packages/support-agent")
resolved = resolve_package(
    package,
    package / "bindings/deepagents.yaml",
    Path("deployment/components.json"),
)

canonical_bytes = resolved.canonical_json()
assert resolved.resolved_digest
```

`ResolvedAgent` and its nested collections cannot be constructed, reassigned,
pickled, or given Python-side fields.

## Development gates

Build the extension and run Python checks from this directory:

```console
uv run --project . maturin develop
uv run --project . pytest -q
uv run --project . ruff check src tests
uv run --project . pyright
```

From the repository root, also run the Rust workspace and generated-artifact
checks documented in the Wave 3 plan.
