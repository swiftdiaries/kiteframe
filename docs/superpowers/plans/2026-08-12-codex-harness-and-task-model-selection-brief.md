# Codex Harness and Task Model Selection Brief

Status: deferred follow-up.

Use the pinned `references/openai-codex` submodule as a design reference for
the harness. Do not make it a runtime dependency or treat its feature set as
Kiteframe's product boundary.

## Harness patterns to carry forward

1. Require a one-time transport initialization before normal requests, and
   reject a repeated initialization.
2. Keep the workflow application core independent of ACP, stdio, WebSocket,
   and HTTP adapters.
3. Bound connection ingress, execution admission, and egress independently;
   return a retryable overload result rather than accumulating work without a
   limit.
4. Make cancellation observable only after in-flight effects have reached a
   terminal or recoverable classification.
5. Bind approval decisions to a concrete persisted operation so untrusted
   model content cannot broaden their scope.

## Kiteframe extension: model per task/subagent

Each statically declared task is a logical subagent when invoked through
`call` or `parallel`. Add an author-declared model profile to that task node,
so a workflow can select the appropriate model independently for planning,
retrieval, decisioning, or execution tasks.

The selection must be compiled into the immutable workflow definition and
recorded on the model attempt. Resolve profile names through deployment-owned
configuration rather than accepting arbitrary provider model identifiers from
workflow source. The resolved provider/model, profile revision, and relevant
limits must be included in attempt events for replay and audit.

## Acceptance criteria for the later task

- DSL and compiled IR express an optional task model profile.
- Calls and parallel children use the model selected for their own task node.
- A missing profile uses the deployment default deterministically.
- Unknown, disabled, or unauthorized profiles fail before a provider call.
- Profile resolution is immutable for an already-started run and auditable in
  model-attempt history.
- Policy and budgets can constrain model profile use per session, workflow, or
  task.
- Tests prove mixed-model sequential and parallel graphs, default fallback,
  invalid-profile rejection, and replay/recovery behavior.

## Explicit non-goals

- Dynamic model-authored task-graph spawning.
- Delegating Kiteframe workflow execution to the Codex runtime.
- Allowing a prompt or tool result to select or override a task's model.

See [OpenAI Codex Harness Patterns](../references/openai-codex-harness-patterns.md)
for the reviewed source patterns.
