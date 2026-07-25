# Kiteframe V1 Wave 4 Deep Agents Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile immutable `ResolvedAgent` values into the pinned public Deep Agents graph while preserving default denial, dynamic capability visibility, point-of-use authorization, strict subagent narrowing, and durable suspension.

**Architecture:** A deployment installs one static, deny-only Deep Agents harness profile before it builds Kiteframe registries; adapter compilation never registers or mutates a process-global profile. The adapter resolves trusted components from a frozen registry, creates one typed provider-backed tool per admitted capability, and adds a guard middleware that filters each model request and blocks forged calls. Declared nested agents compile recursively from immutable session state.

**Tech Stack:** Python 3.11+, `deepagents==0.6.12`, LangChain/LangGraph public APIs pinned through Deep Agents, Kiteframe native contracts, pytest, pytest-asyncio, uv.

## Global Constraints

- The adapter targets `deepagents==0.6.12` at upstream commit `196a0870fcf8a7f29d1fb37886dd323b190f9c16`; the implementation lock records distribution hashes.
- The only construction call is public `deepagents.create_deep_agent(...)`, returning public `CompiledStateGraph`.
- Adapter source MUST NOT import `deepagents._*`, call private functions, or mutate a global harness profile during validation or compilation.
- Deployment bootstrap may register one static deny-only profile through the public `register_harness_profile` API; the adapter requires a matching trusted token and independently enforces the same deny rules.
- Filesystem, shell, direct HTTP, MCP, delegation, and the automatic general-purpose subagent are unavailable unless represented by declared, locked, bound, and admitted Kiteframe capabilities.
- Provider, policy, registry, middleware, or construction failure never restores a built-in or broader subagent.
- A required denied capability stops session construction; an optional denied capability is absent with a stable diagnostic.
- The provider reauthorizes every tool invocation; dynamic model visibility is not authorization.
- Child effective authority equals parent current authority ∩ parent delegation declaration ∩ child requirements ∩ child admission.
- Any suspendable capability requires a durable checkpointer. Resume revalidates evidence, grant expiry, policy freshness, resource preconditions, and point-of-use authorization.
- Runtime state, component registries, grants, middleware, and subagent graphs are immutable per session and never shared through mutable globals.

---

## File Structure

```text
python/kiteframe-deepagents/
├── pyproject.toml
├── uv.lock
├── README.md
├── src/kiteframe_deepagents/
│   ├── __init__.py                            # Public adapter exports
│   ├── compatibility.py                       # Pin, signature, and safe-profile token
│   ├── target.py                              # Exact supported-feature descriptor
│   ├── adapter.py                             # Validate and compile entrypoint
│   ├── components.py                          # Runtime-checkable trusted component contracts
│   ├── context.py                             # Immutable actor/task/session state
│   ├── tools.py                               # Provider-backed typed BaseTool
│   ├── middleware.py                          # Visibility and forged-call guard
│   ├── delegation.py                          # Authority intersection and recursive compile
│   └── suspension.py                          # Interrupt/resume evidence references
└── tests/
    ├── conftest.py                            # Fake model/provider/registry/checkpointer
    ├── test_compatibility.py
    ├── test_adapter_validation.py
    ├── test_tools.py
    ├── test_default_denial.py
    ├── test_delegation.py
    ├── test_concurrency.py
    └── test_suspension.py
runtime-targets/
└── deepagents-0.6.12.json                     # Exact features and public API fingerprint
```

### Task 1: Pin Deep Agents and prove a deployment-installed deny-only profile

**Files:**
- Create: `python/kiteframe-deepagents/pyproject.toml`
- Create: `python/kiteframe-deepagents/uv.lock`
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/__init__.py`
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/compatibility.py`
- Create: `python/kiteframe-deepagents/tests/test_compatibility.py`

**Interfaces:**
- Consumes: public `deepagents.create_deep_agent`, `HarnessProfile`, `GeneralPurposeSubagentProfile`, and deployment-time `register_harness_profile`.
- Produces: `DeepAgentsCompatibility`, `KiteframeHarnessProfileToken`, `deny_only_profile()`, and `verify_compatibility()`.

- [ ] **Step 1: Write failing version, signature, and deny-profile tests**

```python
import inspect
import deepagents
from deepagents import create_deep_agent

EXPECTED_PARAMETERS = (
    "model", "tools", "system_prompt", "middleware", "subagents", "skills",
    "memory", "permissions", "backend", "interrupt_on", "response_format",
    "state_schema", "context_schema", "checkpointer", "store", "debug",
    "name", "cache",
)


def test_pinned_distribution_and_public_signature() -> None:
    assert deepagents.__version__ == "0.6.12"
    assert tuple(inspect.signature(create_deep_agent).parameters) == EXPECTED_PARAMETERS


def test_deny_only_profile_disables_ambient_facilities() -> None:
    profile = deny_only_profile()
    assert profile.excluded_tools == frozenset(
        {"ls", "read_file", "write_file", "edit_file", "glob", "grep", "execute"}
    )
    assert profile.general_purpose_subagent.enabled is False
```

- [ ] **Step 2: Lock the exact dependency and run the tests**

Run: `rtk uv lock --project python/kiteframe-deepagents`

Expected: lock contains `deepagents==0.6.12`, Python `>=3.11,<4`, and distribution hashes.

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_compatibility.py -q`

Expected: FAIL because compatibility helpers do not exist.

- [ ] **Step 3: Define the deny-only profile and attestation token**

```python
AMBIENT_TOOL_NAMES = frozenset(
    {"ls", "read_file", "write_file", "edit_file", "glob", "grep", "execute"}
)


@dataclass(frozen=True, slots=True)
class KiteframeHarnessProfileToken:
    model_key: str
    deepagents_version: str
    excluded_tools: frozenset[str]
    general_purpose_subagent_disabled: bool


def deny_only_profile() -> HarnessProfile:
    return HarnessProfile(
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent=GeneralPurposeSubagentProfile(enabled=False),
    )
```

The deployment performs `register_harness_profile(model_key, deny_only_profile())` before creating the frozen Kiteframe registry, constructs `KiteframeHarnessProfileToken` from the same model key and constants, and registers that token under `ComponentKind.HARNESS_PROFILE`. `DeepAgentsAdapter.compile` only resolves and checks the token; it never calls registration.

- [ ] **Step 4: Add the compiled-graph compatibility fixture**

Build a fake model under the registered model key, call the public constructor with no subagents, and assert the first model request contains none of the ambient tool names and no `task` tool. Add a second fixture with one explicit subagent and assert only `task` is added.

- [ ] **Step 5: Run compatibility tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_compatibility.py -q`

Expected: PASS.

If the pin, signature, deny-only profile, or graph tool list differs, stop Wave 4 and mark `kiteframe.runtime.deepagents.public-create@1` unsupported; do not patch private Deep Agents internals.

- [ ] **Step 6: Commit the compatibility gate**

```bash
rtk git add python/kiteframe-deepagents
rtk git commit -m "test: pin deep agents public compatibility"
```

### Task 2: Publish target features and validate every trusted component before construction

**Files:**
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/target.py`
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/components.py`
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/context.py`
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/adapter.py`
- Create: `runtime-targets/deepagents-0.6.12.json`
- Create: `python/kiteframe-deepagents/tests/test_adapter_validation.py`

**Interfaces:**
- Consumes: `ResolvedAgent`, `RuntimeBinding`, `FrozenComponentRegistry`, native `CompilationReport`.
- Produces: `DeepAgentsAdapter.target()`, `supported_features()`, `validate(...)`, `KiteframeSessionContext`, and runtime-checkable `DurableCheckpointer`.

- [ ] **Step 1: Write failing component and feature validation tests**

```python
def test_missing_model_symbol_fails_before_constructor(
    adapter: DeepAgentsAdapter,
    resolved_agent: ResolvedAgent,
    registry_without_model: FrozenComponentRegistry,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(resolved_agent, deepagents_binding(), registry_without_model)
    assert error.value.code == "KF-RUNTIME-001"


def test_suspendable_capability_requires_durable_checkpointer(
    adapter: DeepAgentsAdapter,
    suspendable_agent: ResolvedAgent,
    registry_with_ephemeral_checkpointer: FrozenComponentRegistry,
) -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        adapter.validate(
            suspendable_agent,
            deepagents_binding(),
            registry_with_ephemeral_checkpointer,
        )
    assert error.value.code == "KF-RUNTIME-001"
    assert "durable checkpointer" in str(error.value)
```

- [ ] **Step 2: Run validation tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_adapter_validation.py -q`

Expected: FAIL because the adapter and component contracts do not exist.

- [ ] **Step 3: Define exact target features**

```python
SUPPORTED_FEATURES = frozenset(
    {
        "kiteframe.runtime.deepagents.public-create@1",
        "kiteframe.capability.point-of-use-auth@1",
        "kiteframe.capability.dynamic-visibility@1",
        "kiteframe.capability.deferred@1",
        "kiteframe.capability.suspendable@1",
        "kiteframe.delegation.narrowing@1",
    }
)
```

Generate `runtime-targets/deepagents-0.6.12.json` from this constant and include the expected public parameter list plus the upstream commit.

- [ ] **Step 4: Resolve and validate components without constructing the graph**

Validate model role objects, middleware sequence, package backend, optional store, durable checkpointer, capability provider, audit sink, and harness profile token by exact `ComponentKind`. The profile token must match the resolved model key, Deep Agents version, ambient exclusion set, and general-subagent-disabled flag.

```python
@runtime_checkable
class DurableCheckpointer(Protocol):
    kiteframe_durable: Literal[True]

    async def aget_tuple(self, config: RunnableConfig) -> CheckpointTuple | None: ...
```

- [ ] **Step 5: Run validation tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_adapter_validation.py -q`

Expected: PASS and verify `create_deep_agent` is never called on a validation failure.

- [ ] **Step 6: Commit adapter validation and target metadata**

```bash
rtk git add python/kiteframe-deepagents runtime-targets/deepagents-0.6.12.json
rtk git commit -m "feat: validate deep agents runtime bindings"
```

### Task 3: Create locked-schema capability tools with status-first retry behavior

**Files:**
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/tools.py`
- Create: `python/kiteframe-deepagents/tests/test_tools.py`

**Interfaces:**
- Consumes: `ResolvedCapabilityRequirement`, locked `CapabilityDescriptor`, `CapabilityGrant`, `CapabilityInvoker`, `KiteframeSessionContext`.
- Produces: `CapabilityTool(BaseTool)` and `build_capability_tools(...) -> tuple[CapabilityTool, ...]`.

- [ ] **Step 1: Write failing schema, authorization, and retry tests**

```python
def test_tool_name_description_and_schema_come_from_lock(
    read_tool: CapabilityTool,
    locked_read_descriptor: CapabilityDescriptor,
) -> None:
    assert read_tool.name == "cases.read"
    assert read_tool.description == locked_read_descriptor.summary
    assert read_tool.args_schema == locked_read_descriptor.input_schema


@pytest.mark.asyncio
async def test_tool_invokes_provider_with_session_and_trace_context(
    read_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    await read_tool.ainvoke({"case_id": "case-1", "_resource": "tenant:t1/case:case-1"})
    request = fake_invoker.requests[-1]
    assert request.admission_id == "adm-1"
    assert request.trace_context.traceparent == VALID_TRACEPARENT


@pytest.mark.asyncio
async def test_unknown_outcome_queries_status_before_same_key_retry(
    comment_tool: CapabilityTool,
    fake_invoker: FakeInvoker,
) -> None:
    fake_invoker.outcomes = [outcome_unknown("inv-1"), succeeded("inv-1", {"ok": True})]
    result = await comment_tool.ainvoke(
        {"case_id": "case-1", "body": "hello", "_resource": "tenant:t1/case:case-1"}
    )
    assert result == {"ok": True}
    assert fake_invoker.calls == ["invoke:key-1", "status:inv-1"]
```

- [ ] **Step 2: Run tool tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_tools.py -q`

Expected: FAIL because `CapabilityTool` does not exist.

- [ ] **Step 3: Implement the async provider-backed tool**

```python
class CapabilityTool(BaseTool):
    name: str
    description: str
    args_schema: dict[str, Any]
    descriptor: CapabilityDescriptor
    grant: CapabilityGrant
    invoker: CapabilityInvoker
    session: KiteframeSessionContext

    async def _arun(self, **arguments: Any) -> Any:
        resource = self._select_resource(arguments.pop("_resource", None))
        request = build_native_invocation_request(
            descriptor=self.descriptor,
            grant=self.grant,
            session=self.session,
            resource=resource,
            arguments=arguments,
        )
        outcome = await self.invoker.invoke(request)
        return await self._resolve_outcome(request, outcome)

    def _run(self, **arguments: Any) -> Any:
        raise RuntimeError("Kiteframe capability tools require async invocation")
```

Use a caller-generated UUIDv7 idempotency key scoped by actor, exact capability version, normalized resource, and semantic operation. Persist the key in session checkpoint state before provider invocation.

- [ ] **Step 4: Validate provider results against the locked output schema**

Native Rust validation runs first. The tool maps `Denied`, `Failed`, and `OutcomeUnknown` to stable safe tool errors, returns `Deferred` as an invocation reference, and sends `Suspended` to the suspension bridge. Provider-native exceptions never become tool output.

- [ ] **Step 5: Run tool tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_tools.py -q`

Expected: PASS for schema projection, resource selection, admission context, trace context, idempotency, invalid results, stable errors, deferred status, and outcome-unknown status-first behavior.

- [ ] **Step 6: Commit capability tools**

```bash
rtk git add python/kiteframe-deepagents/src/kiteframe_deepagents/tools.py python/kiteframe-deepagents/tests/test_tools.py
rtk git commit -m "feat: map grants to deep agents tools"
```

### Task 4: Filter model-visible tools dynamically and block forged ambient calls

**Files:**
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/middleware.py`
- Create: `python/kiteframe-deepagents/tests/test_default_denial.py`

**Interfaces:**
- Consumes: current `CapabilityGrantSet`, session time, suspension state, admitted capability tools, declared child tool.
- Produces: `KiteframeGuardMiddleware`.

- [ ] **Step 1: Write failing visibility and forged-call tests**

```python
@pytest.mark.asyncio
async def test_expired_or_revoked_grants_disappear_from_each_model_request(
    middleware: KiteframeGuardMiddleware,
) -> None:
    assert tool_names(await middleware.visible_tools(now=ISSUED_AT)) == {"cases.read"}
    middleware.replace_grants(expired_grant_set())
    assert tool_names(await middleware.visible_tools(now=EXPIRED_AT)) == set()


@pytest.mark.parametrize(
    "name",
    ["ls", "read_file", "write_file", "edit_file", "glob", "grep", "execute", "http", "mcp", "task"],
)
@pytest.mark.asyncio
async def test_forged_ambient_call_is_denied(name: str, middleware: KiteframeGuardMiddleware) -> None:
    request = forged_tool_call(name)
    with pytest.raises(KiteframeDiagnosticError) as error:
        await middleware.awrap_tool_call(request, should_not_run)
    assert error.value.code == "KF-AUTH-003"
```

- [ ] **Step 2: Run default-denial tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_default_denial.py -q`

Expected: FAIL because the guard middleware does not exist.

- [ ] **Step 3: Implement per-model-call visibility**

```python
class KiteframeGuardMiddleware(AgentMiddleware):
    async def awrap_model_call(self, request: ModelRequest, handler: ModelCallHandler):
        visible = self._visible_tools(
            grants=self._session.grants,
            now=self._clock.now(),
            suspended=self._session.suspension is not None,
        )
        return await handler(request.override(tools=visible))

    async def awrap_tool_call(self, request: ToolCallRequest, handler: ToolCallHandler):
        if request.tool_call["name"] not in self._currently_invocable_names():
            raise invocation_denied(f"tool {request.tool_call['name']!r} is not visible")
        return await handler(request)
```

The tool list is recomputed for every model request from immutable current grants, expiry, task/session identity, child declarations, and suspension state. The deny-only ambient set is always removed before the request reaches the model.

- [ ] **Step 4: Add provider and middleware failure tests**

Make grant refresh, registry lookup, and middleware exceptions fail the model call with stable diagnostics. Assert the request never falls back to the original Deep Agents tool list.

- [ ] **Step 5: Run default-denial tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_default_denial.py -q`

Expected: PASS.

- [ ] **Step 6: Commit defense-in-depth middleware**

```bash
rtk git add python/kiteframe-deepagents/src/kiteframe_deepagents/middleware.py python/kiteframe-deepagents/tests/test_default_denial.py
rtk git commit -m "feat: enforce dynamic tool visibility"
```

### Task 5: Compile the root graph through the pinned public constructor

**Files:**
- Modify: `python/kiteframe-deepagents/src/kiteframe_deepagents/adapter.py`
- Modify: `python/kiteframe-deepagents/src/kiteframe_deepagents/components.py`
- Create: `python/kiteframe-deepagents/tests/test_compile.py`

**Interfaces:**
- Consumes: validated binding components, root prompt/skill assets, capability tools, guard middleware, explicit child specs.
- Produces: `DeepAgentsAdapter.compile(...) -> CompiledStateGraph`.

- [ ] **Step 1: Write failing public-construction tests**

```python
def test_compile_returns_public_compiled_state_graph(
    adapter: DeepAgentsAdapter,
    resolved_agent: ResolvedAgent,
    frozen_registry: FrozenComponentRegistry,
) -> None:
    graph = adapter.compile(
        resolved_agent,
        deepagents_binding(),
        frozen_registry,
        session_context(),
    )
    assert isinstance(graph, CompiledStateGraph)


def test_constructor_receives_only_resolved_and_registered_values(
    create_spy: Mock,
    adapter: DeepAgentsAdapter,
) -> None:
    adapter.compile(resolved_agent(), deepagents_binding(), registry(), session_context())
    kwargs = create_spy.call_args.kwargs
    assert kwargs["system_prompt"] == "You are the support agent."
    assert kwargs["model"] is registry().resolve(ComponentKind.MODEL, "models.primary")
    assert kwargs["skills"] == ["/__kiteframe__/skills/case-summary"]
    assert kwargs["name"] == "support-agent"
```

- [ ] **Step 2: Run compile tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_compile.py -q`

Expected: FAIL because `compile` does not construct a graph.

- [ ] **Step 3: Assemble exact public arguments**

```python
graph = create_deep_agent(
    model=components.primary_model,
    tools=capability_tools,
    system_prompt=resolved.system_prompt,
    middleware=(*components.middleware, guard),
    subagents=compiled_children or None,
    skills=components.package_backend.skill_sources(resolved.skills) or None,
    memory=None,
    permissions=None,
    backend=components.package_backend,
    interrupt_on=None,
    checkpointer=components.checkpointer,
    store=components.store,
    name=resolved.package_name,
)
```

The package backend exposes only validated prompt/skill bytes under the virtual `/__kiteframe__/` prefix. `permissions=None` does not grant ambient tools because the deployment profile and guard independently exclude them.

- [ ] **Step 4: Map construction failures safely**

Catch public constructor exceptions and emit `KF-RUNTIME-002` with exception class and safe component symbol only. Do not include prompt text, model credentials, tool arguments, provider response bodies, or object representations.

- [ ] **Step 5: Run compile tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_compile.py -q`

Expected: PASS and verify the public constructor is called exactly once after all validation succeeds.

- [ ] **Step 6: Commit public graph construction**

```bash
rtk git add python/kiteframe-deepagents/src/kiteframe_deepagents python/kiteframe-deepagents/tests/test_compile.py
rtk git commit -m "feat: compile resolved agents to deep agents"
```

### Task 6: Recursively compile declared subagents with monotonic authority

**Files:**
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/delegation.py`
- Modify: `python/kiteframe-deepagents/src/kiteframe_deepagents/adapter.py`
- Create: `python/kiteframe-deepagents/tests/test_delegation.py`
- Create: `python/kiteframe-deepagents/tests/test_concurrency.py`

**Interfaces:**
- Consumes: parent current grants, parent delegation requirement, child requirements, child admission, delegation ancestry.
- Produces: `intersect_child_envelope(...)`, recursively compiled `CompiledSubAgent` values, and isolated child session contexts.

- [ ] **Step 1: Write failing narrowing and isolation tests**

```python
def test_child_cannot_receive_capability_missing_from_parent() -> None:
    with pytest.raises(KiteframeDiagnosticError) as error:
        intersect_child_envelope(
            parent=grants("cases.read"),
            delegation=delegation("cases.read", "cases.comment"),
            child_requirements=requirements("cases.comment"),
            child_admission=grants("cases.comment"),
        )
    assert error.value.code == "KF-AUTH-001"


def test_child_selector_and_expiry_are_narrower() -> None:
    child = intersect_child_envelope(
        parent=grant("cases.read", "tenant:t1/case:*", expires=HOUR_2),
        delegation=delegation("cases.read", "tenant:t1/case:case-7"),
        child_requirements=requirement("cases.read", "tenant:t1/case:*"),
        child_admission=grant("cases.read", "tenant:t1/case:case-7", expires=HOUR_1),
    )
    assert child.grants[0].resources == ("tenant:t1/case:case-7",)
    assert child.expires_at == HOUR_1
```

- [ ] **Step 2: Run delegation tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_delegation.py -q`

Expected: FAIL because delegation intersection is undefined.

- [ ] **Step 3: Implement field-by-field intersection**

Intersect exact version, resource selector, effect class, execution mode, expiry, freshness, and required evidence. Explicit deny or absence at any term removes the grant. Include immutable ancestry entries `(parent_agent, child_agent, delegated_capabilities)` in child admission and invocation requests.

- [ ] **Step 4: Recursively compile only declared child packages**

Use each `ResolvedSubagent` from Rust IR, admit the child against the narrowed request, create a new frozen child session context, and pass a public `CompiledSubAgent` to the parent. Reject duplicate identity and cycles even if Rust validation already rejected them.

- [ ] **Step 5: Run delegation and concurrency tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_delegation.py tests/test_concurrency.py -q`

Expected: PASS for version/resource/effect/expiry/evidence narrowing and 100 concurrent sessions with no model, registry, grant, middleware, idempotency-key, or child-graph leakage.

- [ ] **Step 6: Commit recursive delegation**

```bash
rtk git add python/kiteframe-deepagents/src/kiteframe_deepagents/delegation.py python/kiteframe-deepagents/src/kiteframe_deepagents/adapter.py python/kiteframe-deepagents/tests
rtk git commit -m "feat: narrow deep agents subagent authority"
```

### Task 7: Suspend and resume through durable checkpoints with full revalidation

**Files:**
- Create: `python/kiteframe-deepagents/src/kiteframe_deepagents/suspension.py`
- Modify: `python/kiteframe-deepagents/src/kiteframe_deepagents/tools.py`
- Create: `python/kiteframe-deepagents/tests/test_suspension.py`
- Modify: `python/kiteframe-deepagents/README.md`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: native `Suspension`, evidence references, durable checkpointer, provider invoker, session context.
- Produces: `SuspensionEnvelope`, public LangGraph interrupt/resume mapping, and final Wave 4 compatibility suite.

- [ ] **Step 1: Write failing suspension and restart tests**

```python
@pytest.mark.asyncio
async def test_suspension_checkpoint_contains_references_not_evidence_text(
    suspendable_graph: CompiledStateGraph,
    durable_checkpointer: FakeDurableCheckpointer,
) -> None:
    result = await invoke_until_interrupt(suspendable_graph, comment_request())
    checkpoint = durable_checkpointer.latest()
    serialized = json.dumps(checkpoint)
    assert result["type"] == "kiteframe.capability.suspension"
    assert "evidence-ref-1" in serialized
    assert "I approve this comment" not in serialized


@pytest.mark.asyncio
async def test_process_restart_resume_reauthorizes_before_effect(
    restarted_graph: CompiledStateGraph,
    fake_invoker: FakeInvoker,
) -> None:
    await resume_with_evidence(restarted_graph, "evidence-ref-1")
    assert fake_invoker.calls == [
        "validate_evidence",
        "check_grant_expiry",
        "check_policy_revision",
        "check_preconditions",
        "invoke_point_of_use",
    ]
```

- [ ] **Step 2: Run suspension tests**

Run: `rtk uv run --project python/kiteframe-deepagents pytest tests/test_suspension.py -q`

Expected: FAIL because the suspension bridge does not exist.

- [ ] **Step 3: Implement protected interrupt payloads**

```python
@dataclass(frozen=True, slots=True)
class SuspensionEnvelope:
    invocation_id: str
    admission_id: str
    evidence_kind: Literal["confirmation", "approval", "consent"]
    evidence_request_ref: str
    traceparent: str


def suspend(outcome: InvocationOutcome) -> NoReturn:
    envelope = SuspensionEnvelope.from_native(outcome)
    interrupt(dataclasses.asdict(envelope))
```

Checkpoint payloads contain only opaque references and correlation IDs. On resume, construct a new native `InvocationRequest` with the evidence reference and the same idempotency key.

- [ ] **Step 4: Add the public-surface and no-global-mutation scan**

Parse adapter Python source with `ast` and fail when an import starts with `deepagents._`. Monkeypatch public `register_harness_profile` to raise during every adapter validation/compile test and prove the adapter never calls it.

- [ ] **Step 5: Run the complete Wave 4 suite**

Run: `rtk uv run --project python/kiteframe-deepagents pytest -q`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe-deepagents ruff check src tests`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe-deepagents pyright`

Expected: PASS.

Run: `rtk uv run --project python/kiteframe-deepagents python -m kiteframe_deepagents.target --check runtime-targets/deepagents-0.6.12.json`

Expected: PASS for target metadata drift.

- [ ] **Step 6: Document deployment bootstrap and commit the Wave 4 gate**

Document the one-time public profile registration, trusted token registration, registry freeze, adapter compilation, and the guarantee that compile never mutates process-global profile state.

```bash
rtk git add python/kiteframe-deepagents runtime-targets/deepagents-0.6.12.json .github/workflows/ci.yml
rtk git commit -m "test: gate secure deep agents compilation"
```

## Wave 4 Exit Criteria

- The exact pinned distribution, public signature, profile behavior, and target feature descriptor pass a compatibility fixture.
- Adapter validation resolves every model/component/provider/audit/profile symbol before calling the constructor.
- Model-visible tools are recomputed for every request; expired, revoked, suspended, undeclared, and ambient tools are absent.
- Forged calls to hidden or built-in tools fail with `KF-AUTH-003`; no failure path restores the Deep Agents default tool suite.
- Only declared child packages compile, and every child authority dimension is equal or narrower than the parent.
- Concurrent sessions do not leak registries, grants, middleware, subagents, or idempotency state.
- Suspendable capabilities require a durable checkpointer; checkpoint payloads contain opaque evidence references; restart/resume performs full revalidation.
- Adapter source uses no private Deep Agents imports and does not call global profile registration during validation or compilation.
