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
reference, proposal digest, traceparent, and the public LangGraph execution
scope (`thread_id`, task ID, checkpoint namespace, and checkpoint ID). A
deployment-owned `EvidenceReferenceResolver` must turn the untrusted external
handle and exact suspension payload into a versioned opaque credential. The
durable checkpointer must implement `EvidenceResumeCredentialVerifier` with
restart-stable verification material and return exact
`EvidenceResumeCredentialClaims` containing only a protected reference,
key ID, nonce, expiry, and the complete suspension scope:

```python
from kiteframe_deepagents import (
    resolve_protected_evidence_reference,
    resume_command,
)

protected_reference = await resolve_protected_evidence_reference(
    external_evidence_handle,
    interrupt.value,
    deployment_evidence_resolver,
    durable_checkpointer,
)
command = resume_command(protected_reference, durable_checkpointer)
```

Plain approval text, passwords, JWTs, and base64-like secret values are rejected
before `Command` construction. The command retains the exact resolver-issued
brand rather than downcasting it to a public dictionary. Credentials must not
embed the external handle, raw evidence, signing keys, or other client
evidence. Adapter compilation injects the checkpointer's verifier into both the
protected serializer and a delegating saver. Every deserialize and write
re-verifies the opaque credential, its expiry, and its graph scope before
privately restoring the brand or calling the deployment saver. The suspension
bridge additionally requires exact native invocation, admission, proposal, and
LangGraph task/checkpoint equality, preventing cross-suspension replay.

The credential remains replayable only for the same unexpired suspension. This
is required to recover when a process exits after the resume write is durable
but before the interrupted node consumes it; provider idempotency and the full
native point-of-use revalidation remain authoritative. Deployments that rotate
keys must keep the credential's `key_id` verifiable until its expiry.
Resume then rebuilds a native `InvocationRequest` with the same invocation ID,
idempotency key, admission ID, canonical grant-set digest, arguments,
preconditions, and trace context. The provider validates the referenced
evidence, reloads the admitted authority snapshot, obtains fresh authority
revisions, checks expiry and preconditions, and authorizes the effect at point
of use. `AuthorityRevisionSet` remains immutable local guard state and is never
serialized into the provider request.
