# Kiteframe Deep Agents adapter

This package compiles one immutable Kiteframe `ResolvedRuntimeInputs` snapshot
to the public `deepagents==0.6.12` constructor. It keeps package resolution,
runtime component registration, authorization, and provider credentials outside
the model-visible graph.

## Deployment bootstrap

Profile registration is a one-time deployment action. Build the mutable
`ComponentRegistry`, register the deployment-owned models and components, and
then call `bootstrap_deepagents_deployment(...)` with the exact public
`provider:model` key that compilation will pass to Deep Agents:

```python
from kiteframe import ComponentKind, ComponentRegistry
from kiteframe_deepagents import bootstrap_deepagents_deployment
from kiteframe_deepagents.adapter import DeepAgentsAdapter

registry = ComponentRegistry()
registry.register(ComponentKind.MODEL, "models.primary", "provider:model")
# Register the capability provider, audit sink, durable checkpointer, and any
# declared middleware/backend/store components here.
bootstrap_deepagents_deployment(
    registry,
    model_key="provider:model",
    profile_symbol="profiles.deepagents",
)
trusted_registry = registry.freeze()

graph = DeepAgentsAdapter().compile(
    resolved_runtime_inputs,
    trusted_registry,
    session_context,
)
```

The bootstrap uses the public `register_harness_profile` API to install the
static deny-only profile and registers a matching trusted
`KiteframeHarnessProfileToken` before the registry is frozen. Validation and
compilation only resolve and compare that token; they never register a profile
or mutate process-global profile state.

Suspendable capabilities additionally require a deployment-attested durable
LangGraph checkpointer. It must implement
`persist_invocation_correlation(...)` and
`load_invocation_correlation(...)` from the adapter checkpoint protocols, in
addition to the public LangGraph saver operations. Effectful capabilities also
require `persist_idempotency_key(...)`; restart-compatible legacy integrations
may continue to expose `load_idempotency_key(...)`. The adapter writes the
invocation correlation before the first provider call, including for
read-only suspendable capabilities.

On suspension, `SuspensionEnvelope` exposes only the native invocation and
admission IDs, checkpoint reference, evidence kind, protected evidence-request
reference, proposal digest, and traceparent. A deployment-owned
`EvidenceReferenceResolver` must turn the untrusted external handle into a
branded reference before a public LangGraph command can be built:

```python
from kiteframe_deepagents import (
    resolve_protected_evidence_reference,
    resume_command,
)

protected_reference = await resolve_protected_evidence_reference(
    external_evidence_handle,
    deployment_evidence_resolver,
)
command = resume_command(protected_reference)
```

Plain approval text, passwords, JWTs, and base64-like secret values are rejected
before `Command` construction. Resume rebuilds a native `InvocationRequest`
with the same invocation ID, idempotency key, admission ID, canonical grant-set
digest, arguments, preconditions, and trace context. The provider then
validates the referenced evidence, reloads the admitted authority snapshot,
obtains fresh authority revisions, checks expiry and preconditions, and
authorizes the effect at point of use. `AuthorityRevisionSet` remains immutable
local guard state and is never serialized into the provider request.
