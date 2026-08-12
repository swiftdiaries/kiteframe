# OpenAI Codex Harness Patterns

Reference snapshot: `references/openai-codex` at
`2230d64464488d8847197722fdca09d90095c705`.

This note records design patterns reviewed from the public Codex repository.
It is a reference, not a dependency or a proposal to adopt Codex's product
surface.

## Applicable patterns

1. **Gate each connection with a one-time initialization handshake.** Codex's
   app server rejects normal requests before initialization and rejects a
   repeated initialize request. Kiteframe's ACP adapters should keep the same
   connection-local state so version negotiation and extension capability
   selection cannot change mid-session.

2. **Keep a version-neutral core and adapt only at transport boundaries.**
   Codex's protocol documentation explicitly treats NDJSON as a serialization
   for bidirectional streaming transports, not as the application model.
   Kiteframe's `ApplicationCommand` and `ApplicationResult` follow this shape;
   ACP v1/v2, stdio, WebSocket, and HTTP should remain adapters over them.

3. **Bound ingress, work, and egress separately.** Codex's app server uses
   bounded queues and returns a retryable overload error rather than allowing
   unbounded connection buffering. Kiteframe's remote transport work must add
   bounded admission and distinguish overload from authentication, malformed
   protocol, and application failures.

4. **Make cancellation completion observable.** Codex exposes interruption as
   a request followed by an explicit completed terminal event. Kiteframe should
   retain its durable cancellation event and only report cancellation complete
   after in-flight external effects have settled or been classified for
   recovery.

5. **Keep approval scope concrete.** Codex approval guidance treats untrusted
   content as incapable of expanding approval scope. Kiteframe preserves this
   rule by binding user decisions to the persisted tool name, descriptor digest,
   arguments, policy revision, and idempotency key.

## Reviewed sources

- `codex-rs/app-server/README.md`: connection lifecycle, bounded queues,
  backpressure, cancellation, and approval flows.
- `codex-rs/docs/protocol_v1.md`: version-neutral core events and NDJSON
  transport adaptation.
- `codex-rs/core/src/guardian/policy_template.md`: concrete approval scope and
  untrusted-content boundaries.
