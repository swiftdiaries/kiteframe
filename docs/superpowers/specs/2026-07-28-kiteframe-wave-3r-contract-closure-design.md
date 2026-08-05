# Kiteframe Wave 3R Contract Closure Design Addendum

- **Status:** Approved
- **Date:** 2026-07-28
- **Repository:** `swiftdiaries/kiteframe`
- **Extends:** `2026-07-25-kiteframe-declarative-agent-harness-design.md`
- **Contract maturity:** V1 alpha

## Purpose

This addendum closes contract gaps found by the adversarial review of the
merged Wave 3 implementation at `54a2175`. It is normative where it is more
specific than the founding design. All founding goals, trust boundaries,
security properties, and non-goals remain in force.

Wave 3R is a mandatory contract-closure delivery between Wave 3 and Waves 4
and 5. It does not add a workflow language, execution engine, conversation
platform, task synthesizer, presentation layer, or second runtime.

## Decisions

### Complete runtime compilation inputs

`resolve_package(...)` returns a frozen, non-constructible
`ResolvedRuntimeInputs` projection containing:

- the validated `ResolvedAgent`;
- the exact validated `RuntimeBinding`;
- the selected `RuntimeTarget`; and
- the validated target component descriptors through `target_components`.

`ResolvedRuntimeInputs` is a Python compilation boundary, not portable package
IR. `load_resolved_agent(...)` remains an inspection and canonical-IR loading
surface; it is not sufficient input for runtime compilation. Runtime adapters
MUST NOT reopen package YAML, capability locks, bindings, targets, or catalogs
to reconstruct discarded inputs.

### Exact locked capability semantics

`ResolvedCapabilityRequirement` retains its name and embeds the exact verified
`LockedCapability`. Rust and frozen Python projections expose:

- the locked descriptor;
- descriptor digest;
- input-schema digest;
- output-schema digest;
- stable-error-set digest; and
- safety-metadata digest.

Runtime and provider consumers MUST use these exact values. They MUST NOT
refetch a live descriptor, hardcode a schema, or create an alternate Python
contract by parsing canonical JSON.

### Structured and descriptor-aware provider results

Invocation outcomes and statuses expose typed result, stable error,
diagnostic, suspension, and state fields directly. Before any result reaches
an adapter, Rust validation proves:

- invocation correlation;
- output-schema validity;
- stable-error membership and exact semantics;
- execution-mode compatibility; and
- suspension/status compatibility with the locked descriptor.

Canonical JSON remains available for hashing, storage, and interchange. It is
not the adapter's object model.

### Admission and effective authority

An `AdmissionRequest` includes expected catalog identity and digest and has a
domain-separated canonical request digest.

`CapabilityGrantSet` contains:

- the admission request digest;
- exact actor, agent, task, and session correlation;
- exact catalog identity and digest;
- canonical `AuthorityRevisionSet`;
- `EffectiveCapabilityGrant` values;
- optional-capability denial diagnostics; and
- the canonical grant-set `grant_digest`.

Every required capability has exactly one effective grant. Every optional
capability has exactly one effective grant or one stable denial diagnostic.

`EffectiveCapabilityGrant` carries exact capability identity and resources,
plus only narrowed:

- execution modes;
- maximum effect;
- per-capability expiry;
- confirmation, approval, and consent evidence requirements;
- freshness requirements; and
- preconditions.

`AuthorityRevisionSet` is a canonical, digest-verified set of generic
source/revision entries. It is provider-produced admission state, not a bearer
credential and not client-supplied point-of-use authority. It contains no
provider authorization tuples or credentials.

`InvocationRequest` binds to `admission_id + grant_digest`. The provider loads
the persisted admission and admitted revision snapshot, obtains current
authority revisions, and performs current point-of-use authorization. Matching
an admitted digest never authorizes execution by itself.

### Proposal-bound suspension and traced status

`EffectProposal` binds the exact invocation, admission, grant-set digest,
capability, selected resource, argument digest, precondition digest, optional
idempotency key, and effect classification through a domain-separated proposal
digest. An idempotency key is present exactly when required by the locked
descriptor.

`Suspension` contains only:

- checkpoint reference;
- evidence kind;
- protected evidence-request reference; and
- proposal digest.

It never contains approval, confirmation, or consent text. Resume MUST
revalidate the proposal/evidence binding, admission and grant, current
authority revisions, expiry, freshness, preconditions, and point-of-use
authorization.

`StatusRequest` contains the invocation ID and native `TraceContext`.
Catalog, admission, invocation, and status calls all propagate their request's
W3C `traceparent` and `tracestate`; no call invents ambient trace state.

### Catalog freshness and typed revalidation

`CapabilityCatalog` includes issued time and optional expiry, both covered by
its canonical digest. Expiry MUST follow issue time.

The Python provider boundary returns native `CatalogFetchResult` values:

- `modified`, carrying a validated catalog; or
- `not_modified`, carrying the exact known catalog digest.

HTTP `304 Not Modified` is accepted only when the request sent a known digest,
the body is empty, and the ETag exact-matches that digest. All other redirect
class responses remain forbidden.

### Deployment authentication boundary

`ProviderAuthenticator` is a deployment-owned Python client protocol. It
returns per-call credential headers through an explicit credential-header
allowlist. It applies to catalog, admission, invocation, and status. Client
certificate material is supplied only through a deployment-built TLS context.

Credentials and certificate configuration MUST NOT enter portable values,
canonical JSON, baggage, diagnostics, exceptions, logs, telemetry, audit
records, or status storage.

The provider server does not implement `ProviderAuthenticator`. Wave 5 defines
a separate server-owned `ProviderPrincipalVerifier` that independently verifies
human and workload credentials and returns opaque verified principal
references. Before application logic, the provider correlates verified tenant,
human, workload, and run identity with the portable actor, agent, task,
session, and admission context. Mismatch fails closed.

### Provider authority and platform profiles

The provider owns the authoritative capability catalog and locked-descriptor
registry. Admission exact-matches client expectations against that state and
persists the provider-validated `LockedCapability`. Provider execution does not
depend on runtime-specific `ResolvedRuntimeInputs`.

Crankshaft is a conformance-driving platform profile, not a Kiteframe
dependency. The profile proves dual-principal authentication, resource and
authority narrowing, stable field projection, revision freshness, durable
audit/outcome linkage, and status-first restart recovery. Crankshaft-specific
field ACLs and legacy presentation shapes remain provider-internal.

## User-journey ownership

The host owns conversation restoration, intent clarification, task synthesis,
interaction and progress streaming, approval UI, and presentation.

Kiteframe owns published package validation, runtime construction from frozen
inputs, declared delegation narrowing, capability admission and invocation,
proposal/evidence correlation, durable suspension/status, safe denial, and
enforcement/audit correlation.

Host fixtures MAY demonstrate the complete product journeys. They MUST NOT be
used as evidence that Kiteframe provides a conversation platform, workflow
DSL, automatic task synthesis, or portable progress-streaming protocol.

## Compatibility and sequencing

These are intentional V1-alpha breaking contract changes. Changed Rust types,
JSON Schemas, canonical fixtures, PyO3 projections, and generated stubs MUST
land together. Old, partial, or mismatched payloads fail closed; no
compatibility shim may silently restore the lossy Wave 3 surface.

The required sequence is:

```text
Wave 3 -> Wave 3R -> Wave 4
                  -> Wave 5
Wave 4 + Wave 5 -> Wave 6
```

Waves 4 and 5 MUST NOT begin until Wave 3R's cross-language consumer gates
pass. Wave 6 MUST trace every normative requirement in both the founding
design and this addendum to passing evidence.

## Approval record

The Wave 3R sequencing and conservative V1 ownership boundary were explicitly
approved on 2026-07-28. This addendum records that decision without expanding
the founding V1 product scope.
