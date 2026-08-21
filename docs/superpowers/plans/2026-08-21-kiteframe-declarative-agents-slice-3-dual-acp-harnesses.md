# Kiteframe Declarative Agents Slice 3 Dual ACP Harnesses Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Slice 2 `HarnessCommandExecutor` seam with a stable built-in/out-of-process harness boundary that supports both Codex ACP and DeepSeek Harness ACP as V1 external coding-harness profiles through one provider-neutral ACP core and one normative conformance suite.

**Architecture:** The Rust kernel continues to own the durable command journal, task/goal revisions, evidence validation, and every authoritative state transition. A versioned `HarnessPlugin` contract normalizes capability description, invocation start, cancellation, progress, terminal evidence, and uncertainty; the direct Responses loop becomes its built-in implementation, while one ACP stdio implementation drives fresh sessions for either pinned external profile. Codex-only features are negotiated optional capabilities, DeepSeek Harness uses its automation-only fresh-session surface, and neither profile can widen the invocation grant or turn harness completion into durable-goal completion.

**Tech Stack:** Rust 1.97.1, edition 2024, Tokio 1, `async-trait` 0.1, `agent-client-protocol` 2.0.0 schema compatibility, raw JSON-RPC 2.0 over stdio, Serde/`serde_json`, SQLx 0.8 with PostgreSQL, SHA-256 canonical digests, Codex ACP source `https://github.com/zed-industries/codex-acp.git` at `296069e841634cd4bb9bc4515602d836e49231ec`, and DeepSeek Harness source `https://github.com/deepseek-ai/deepseek-harness.git` at `141eb6fef83422698aef7a981029e843e8161534`.

**Spec:** `docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md`

## Global Constraints

- Start from reviewed `main` commit `ca09f32`; Slice 2 entered `main` at `a015f8b` and its handoff in `docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-2.md` is the frozen input contract.
- Preserve `AgentRuntime::{validate_agent, start, handle}`, `RuntimeStateSnapshot`, `RuntimeTransition`, `KernelCommand`, `InvokeHarness`, `RuntimeEvent::HarnessResult`, and the Slice 2 `PostgresStore` command methods without renaming or weakening them.
- Only the Rust kernel may validate and commit task, goal, command, snapshot, evidence, approval, cancellation, or recovery authority; plugins return invocation evidence and never issue `CompleteGoal` or `BlockGoal`.
- A claimed `InvokeHarness` must be persisted before any model or subprocess work begins. The durable `accepted` transition is committed immediately before writing `session/prompt`; from that point onward dispatch may have occurred, so process loss is `outcome_unknown` rather than retryable. Replay must reuse the existing command identity, payload digest, invocation identity, and terminal record.
- The normative portable ACP core is `initialize`, `session/new`, ordered text `session/prompt`, `session/request_permission` with one-shot allow/reject, `session/cancel`, committed assistant text, and terminal `end_turn` or `cancelled`.
- Both Codex ACP and DeepSeek Harness ACP must pass the same deterministic command, failure, idempotency, permission, cancellation, process-death, and recovery conformance suite before either is advertised as V1-supported.
- DeepSeek Harness V1 uses fresh connection-owned sessions, one in-flight prompt per session, an absolute primary workspace, empty `additionalDirectories` and `mcpServers`, committed output, and no required load/list/resume/close, editor, terminal, filesystem, MCP, mode, configuration-picker, reasoning, plan, title, or tool-presentation ACP capability.
- Codex ACP may negotiate richer optional capabilities, but `.kite`, `InvokeHarness`, completion validation, recovery, and V1 release acceptance must not depend on them; requesting an absent optional capability fails closed with `harness_capability_unsupported`.
- Pin source provenance exactly: Codex ACP repository `https://github.com/zed-industries/codex-acp.git` at full commit `296069e841634cd4bb9bc4515602d836e49231ec`, and DeepSeek Harness repository `https://github.com/deepseek-ai/deepseek-harness.git` at full commit `141eb6fef83422698aef7a981029e843e8161534`. Live commands are supplied by deployment environment together with the exact source revision attestation; ACP handshake implementation names/versions remain observed metadata and are never substituted for source provenance.
- Cordis auto-composability is outside V1: do not add `cordis_define`, `cordis_run`, `cordis_stop`, dynamic package lifecycle, marketplace installation, hot-swap recovery, package provenance, or runtime-generated authority.
- Tools internal to a fixed DeepSeek Harness deployment composition inherit the bounded invocation grant and cannot add undeclared connector mutations, permissions, workspace roots, secrets, or network destinations.
- Preserve Rust `unsafe_code = "forbid"`, default features, feature-gated `acp-v2`, existing ACP server/client behavior, existing public clients, and all Slice 1-2 tests.
- Database tests remain `DATABASE_URL`-gated; live Codex and DeepSeek suites remain separately key/command-gated and supplement rather than replace deterministic fake-agent tests.
- Preserve unrelated untracked `fuzz/` and `site/`; stage and commit only paths named in each task.

---

## File Structure and Responsibilities

| Path | Change | Single responsibility |
|---|---|---|
| `Cargo.toml` | Modify | Register the test-support fake ACP binary without changing default production targets. |
| `config/harness-profiles/codex-acp-v1.json` | Create | Exact Codex ACP V1 repository/commit provenance, required core, and allowed optional capabilities. |
| `config/harness-profiles/deepseek-harness-acp-v1.json` | Create | Exact DeepSeek Harness repository/commit provenance, fresh-session restrictions, and empty MCP/additional-directory requirements. |
| `migrations/0014_harness_plugins.sql` | Create | Durable plugin registration, per-command invocation/attempt journal, and sequenced invocation-event audit with immutable identities and constrained states. |
| `src/harness/mod.rs` | Create | Public harness module exports only. |
| `src/harness/types.rs` | Create | Versioned provider-neutral profile, capability, request, event, result, failure, and recovery types. |
| `src/harness/profile.rs` | Create | Parse, validate, and digest checked-in harness profiles; enforce exact source provenance and core/optional capability rules. |
| `src/harness/plugin.rs` | Create | `HarnessPlugin` and `HarnessApprovalBroker` traits plus versioned envelopes. |
| `src/harness/registry.rs` | Create | Select one registered plugin by semantic role/profile, enforce capability compatibility, and track cancellable active invocations. |
| `src/harness/builtin.rs` | Create | Move the direct Responses-compatible loop behind `HarnessPlugin` without changing its request or completion semantics. |
| `src/harness/acp/mod.rs` | Create | ACP adapter module exports. |
| `src/harness/acp/client.rs` | Create | JSON-RPC stdio process lifecycle, request correlation, notifications, cancellation, timeouts, and process-death classification. |
| `src/harness/acp/adapter.rs` | Create | Map `HarnessInvocationRequest` to the portable ACP session and map committed ACP output/permissions into normalized events/results. |
| `src/harness/acp/permissions.rs` | Create | One-shot allow/reject option selection and fail-closed approval brokerage. |
| `src/engine/harness.rs` | Modify | Keep durable command dispatch and kernel completion/failure calls; replace direct executor calls with registry/plugin invocation. |
| `src/engine/mod.rs` | Modify | Re-export the new dispatcher surface and stop exporting the removed executor trait. |
| `src/store/harness.rs` | Create | Typed SQLx operations for registration, attempt start, acceptance, normalized event/approval audit, terminal settlement, and recovery scans. |
| `src/store/mod.rs` | Modify | Mount and export harness persistence types. |
| `src/store/models.rs` | Modify | Add durable registration/invocation state views used by the kernel-side dispatcher. |
| `src/lib.rs` | Modify | Export stable harness contracts and remove `HarnessCommandExecutor` from the public surface. |
| `src/main.rs` | Modify | Load validated profiles, construct built-in/ACP plugins and registry, run invocation recovery, and start the dispatcher. |
| `src/bin/kiteframe-fake-acp.rs` | Create | Deterministic scripted ACP subprocess used only with `test-support`. |
| `tests/fixtures/acp/core-success.json` | Create | Initialize/new/prompt/committed-output/end-turn golden exchange. |
| `tests/fixtures/acp/permission-once.json` | Create | One-shot permission request and decision golden exchange. |
| `tests/fixtures/acp/cancel.json` | Create | In-flight prompt cancellation golden exchange. |
| `tests/fixtures/acp/incompatible.json` | Create | Protocol/capability mismatch golden exchange. |
| `tests/fixtures/acp/process-death.json` | Create | Accepted-prompt then EOF fixture for uncertain-outcome recovery. |
| `tests/support/harness_conformance.rs` | Create | One normative suite constructor/assertion set reused unchanged by built-in, fake ACP, Codex, and DeepSeek profiles. |
| `tests/harness_profiles.rs` | Create | Exact pin, schema, digest, role routing, and fail-closed capability tests. |
| `tests/harness_plugin_protocol.rs` | Create | Versioned envelope, registry, fake plugin, cancellation, and plugin isolation tests. |
| `tests/harness_store.rs` | Create | PostgreSQL registration/invocation idempotency, immutability, settlement, and recovery tests. |
| `tests/harness_builtin_conformance.rs` | Create | Run the normative suite against the extracted direct model loop. |
| `tests/harness_acp_conformance.rs` | Create | Run the same suite against scripted stdio ACP and exercise wire failures. |
| `tests/harness_acp_recovery.rs` | Create | Kernel restart/process-death tests proving no duplicate prompt or authoritative transition. |
| `tests/harness_acp_live.rs` | Create | Separately gated Codex and DeepSeek live smoke/conformance entry points. |
| `tests/agent_direct_bridge.rs` | Modify | Preserve Slice 2 kernel-authority assertions while constructing the new registry. |
| `tests/repository_shape.rs` | Modify | Prove exact dual-profile support and the absence of Cordis/dynamic-package V1 surface. |
| `README.md` | Modify | Report built-in, Codex ACP, and DeepSeek Harness ACP support only after release gates pass. |
| `docs/guides/building-on-kiteframe.md` | Modify | Explain the portable ACP core, dual profiles, authority boundary, and V1 exclusions. |
| `docs/guides/testing-kiteframe.md` | Modify | Document deterministic, database-gated, Codex-live, and DeepSeek-live commands and skip evidence. |
| `docs/reference/agent-runtime-and-goal-lifecycle.md` | Modify | Document normalized harness evidence/failure/uncertainty without changing goal authority. |

## Frozen Interfaces and Naming Ledger

The following names are introduced once and used unchanged throughout all tasks:

```rust
pub const HARNESS_PROTOCOL_VERSION: u16 = 1;

pub struct HarnessProfileId(String);
pub enum HarnessTransport { Builtin, AcpStdio }
pub enum HarnessCapability {
    FreshSession,
    TextPrompt,
    PermissionOnce,
    Cancel,
    CommittedOutput,
    SessionLoad,
    EmbeddedContext,
    ImagePrompt,
    McpStdio,
    McpHttp,
    ToolPresentation,
}
pub struct HarnessProfile {
    pub id: HarnessProfileId,
    pub transport: HarnessTransport,
    pub source_repository: String,
    pub source_revision: String,
    pub program: String,
    pub args: Vec<String>,
    pub required: BTreeSet<HarnessCapability>,
    pub optional: BTreeSet<HarnessCapability>,
    pub fresh_sessions_only: bool,
}
pub struct HarnessCapabilities {
    pub protocol_version: u16,
    pub implementation_name: String,
    pub implementation_version: String,
    pub supported: BTreeSet<HarnessCapability>,
}
pub struct HarnessApprovalRequest {
    pub invocation_id: HarnessInvocationId,
    pub request_id: String,
    pub title: String,
}
pub struct HarnessInvocationOutput {
    pub outcome: String,
    pub fields: serde_json::Value,
    pub history: Vec<RuntimeHistoryItem>,
}
pub struct AcpCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub source_revision: String,
}
pub struct AcpLimits {
    pub max_frame_bytes: usize,
    pub shutdown_grace: Duration,
    pub max_pre_accept_attempts: u32,
}
pub struct PermissionOption { pub id: String, pub kind: String }
pub type PermissionOptionId = String;
pub struct HarnessRouteTable {
    pub reasoning: HarnessProfileId,
    pub coding: HarnessProfileId,
}
pub struct HarnessInvocationId(uuid::Uuid);
pub struct HarnessInvocationRequest {
    pub invocation_id: HarnessInvocationId,
    pub command_id: AgentCommandId,
    pub command_revision: u64,
    pub profile_id: HarnessProfileId,
    pub workspace: PathBuf,
    pub payload: InvokeHarness,
    pub payload_digest: String,
}
pub struct HarnessPendingEvent {
    pub event: HarnessInvocationEvent,
    pub persisted: oneshot::Sender<Result<(), HarnessFailure>>,
}
pub enum HarnessInvocationEvent {
    Started,
    Progress { message: String },
    ApprovalRequested { request_id: String, title: String },
    ApprovalResolved { request_id: String, decision: HarnessApprovalDecision },
    AssistantMessage { text: String },
    Cancelled,
}
pub enum HarnessTerminal {
    Completed(HarnessInvocationOutput),
    Cancelled,
    Retryable(HarnessFailure),
    Failed(HarnessFailure),
    OutcomeUnknown(HarnessFailure),
}
pub struct HarnessFailure { pub code: &'static str, pub detail: String }

#[async_trait::async_trait]
pub trait HarnessPlugin: Send + Sync {
    fn profile(&self) -> &HarnessProfile;
    async fn describe_capabilities(&self) -> Result<HarnessCapabilities, HarnessFailure>;
    async fn start_invocation(
        &self,
        request: HarnessInvocationRequest,
        events: mpsc::Sender<HarnessPendingEvent>,
        approvals: Arc<dyn HarnessApprovalBroker>,
        cancellation: CancellationToken,
    ) -> HarnessTerminal;
}

#[async_trait::async_trait]
pub trait HarnessApprovalBroker: Send + Sync {
    async fn decide(&self, request: HarnessApprovalRequest) -> HarnessApprovalDecision;
}

#[async_trait::async_trait]
pub trait HarnessEventSink: Send + Sync {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure>;
}

pub enum HarnessApprovalDecision { AllowOnce, RejectOnce }
pub struct HarnessRegistry;
impl HarnessRegistry {
    pub async fn register(&mut self, plugin: Arc<dyn HarnessPlugin>) -> Result<(), HarnessFailure>;
    pub async fn invoke(
        &self,
        request: HarnessInvocationRequest,
        approvals: Arc<dyn HarnessApprovalBroker>,
        events: Arc<dyn HarnessEventSink>,
    ) -> HarnessTerminal;
    pub async fn cancel(&self, invocation_id: &HarnessInvocationId) -> bool;
}
```

The durable invocation state names are exactly `prepared`, `started`, `accepted`, `completed`, `cancelled`, `failed`, and `outcome_unknown`. The only Slice 3 stable failure codes are:

```text
harness_profile_unavailable
harness_protocol_incompatible
harness_capability_unsupported
harness_permission_rejected
harness_cancelled
harness_failed
harness_outcome_unknown
```

### Task 1: Freeze the Portable Core and Exact Dual Profiles

**Files:**
- Create: `src/harness/mod.rs`
- Create: `src/harness/types.rs`
- Create: `src/harness/profile.rs`
- Create: `config/harness-profiles/codex-acp-v1.json`
- Create: `config/harness-profiles/deepseek-harness-acp-v1.json`
- Modify: `src/lib.rs`
- Test: `tests/harness_profiles.rs`

**Interfaces:**
- Consumes: Slice 2 `AgentCommandId`, `InvokeHarness`, `HarnessRole`, `canonical_sha256`, and immutable `.kite` provider neutrality.
- Produces: every profile/capability/request/output/failure/routing type in the Frozen Interfaces ledger; `HarnessProfile::from_json(bytes: &[u8]) -> Result<Self, HarnessFailure>`; `HarnessProfile::validate_observed(&self, observed: &HarnessCapabilities) -> Result<(), HarnessFailure>`; `HarnessProfile::require(capability: HarnessCapability) -> Result<(), HarnessFailure>`; `HarnessProfile::digest(&self) -> Result<String, HarnessFailure>`; `HarnessRouteTable::{new, select}`.

- [ ] **Step 1: Write failing exact-profile and portability tests**

```rust
#[test]
fn v1_profiles_pin_exact_packages_and_share_only_the_portable_core() {
    let codex = profile("config/harness-profiles/codex-acp-v1.json");
    let deepseek = profile("config/harness-profiles/deepseek-harness-acp-v1.json");
    let core = BTreeSet::from([
        HarnessCapability::FreshSession,
        HarnessCapability::TextPrompt,
        HarnessCapability::PermissionOnce,
        HarnessCapability::Cancel,
        HarnessCapability::CommittedOutput,
    ]);
    assert_eq!(codex.source_revision, "296069e841634cd4bb9bc4515602d836e49231ec");
    assert_eq!(deepseek.source_revision, "141eb6fef83422698aef7a981029e843e8161534");
    assert_eq!(codex.required, core);
    assert_eq!(deepseek.required, core);
    assert!(deepseek.fresh_sessions_only);
    assert!(!deepseek.optional.contains(&HarnessCapability::SessionLoad));
}

#[test]
fn absent_optional_capability_fails_closed_when_requested() {
    let deepseek = profile("config/harness-profiles/deepseek-harness-acp-v1.json");
    let error = deepseek.require(HarnessCapability::SessionLoad).unwrap_err();
    assert_eq!(error.code, "harness_capability_unsupported");
}

#[test]
fn deployment_route_table_selects_profiles_without_changing_kite_source() {
    let routes = HarnessRouteTable::new(profile_id("builtin-v1"), profile_id("codex-acp-v1"));
    assert_eq!(routes.select(Some(HarnessRole::Coding), ModelRole::Reasoning).as_str(), "codex-acp-v1");
    assert_eq!(routes.select(None, ModelRole::Reasoning).as_str(), "builtin-v1");
}
```

- [ ] **Step 2: Run the focused test and confirm the missing-module failure**

Run: `cargo test --test harness_profiles -- --nocapture`

Expected: FAIL to compile with `unresolved import kiteframe::harness` and missing profile files.

- [ ] **Step 3: Define closed types and profile validation**

Implement profile/envelope structs with `serde(rename_all = "camelCase", deny_unknown_fields)`, enum values with `serde(rename_all = "snake_case")`, private string/UUID fields with validated constructors, and this fail-closed check:

```rust
pub fn validate_observed(&self, observed: &HarnessCapabilities) -> Result<(), HarnessFailure> {
    if observed.protocol_version != HARNESS_PROTOCOL_VERSION {
        return Err(HarnessFailure::new(
            "harness_protocol_incompatible",
            format!("expected {}, observed {}", HARNESS_PROTOCOL_VERSION, observed.protocol_version),
        ));
    }
    if !self.required.is_subset(&observed.supported) {
        return Err(HarnessFailure::new(
            "harness_capability_unsupported",
            "required portable ACP capability is absent",
        ));
    }
    Ok(())
}
```

`implementation_name` and `implementation_version` are captured as observed audit metadata only. Source provenance is established by the deployment's exact repository/revision attestation, not inferred from self-reported handshake strings.

- [ ] **Step 4: Add the exact checked-in profiles**

`config/harness-profiles/codex-acp-v1.json`:

```json
{
  "id": "codex-acp-v1",
  "transport": "acp_stdio",
  "sourceRepository": "https://github.com/zed-industries/codex-acp.git",
  "sourceRevision": "296069e841634cd4bb9bc4515602d836e49231ec",
  "program": "codex-acp",
  "args": [],
  "required": ["fresh_session", "text_prompt", "permission_once", "cancel", "committed_output"],
  "optional": ["session_load", "embedded_context", "image_prompt", "mcp_stdio", "mcp_http", "tool_presentation"],
  "freshSessionsOnly": false
}
```

`config/harness-profiles/deepseek-harness-acp-v1.json`:

```json
{
  "id": "deepseek-harness-acp-v1",
  "transport": "acp_stdio",
  "sourceRepository": "https://github.com/deepseek-ai/deepseek-harness.git",
  "sourceRevision": "141eb6fef83422698aef7a981029e843e8161534",
  "program": "pnpm",
  "args": ["--dir", "vendor/deepseek-harness", "run", "demo:acp"],
  "required": ["fresh_session", "text_prompt", "permission_once", "cancel", "committed_output"],
  "optional": [],
  "freshSessionsOnly": true
}
```

The checked-in commands are operator launch recipes. Live tests use `KITEFRAME_CODEX_ACP_COMMAND_JSON` or `KITEFRAME_DSH_ACP_COMMAND_JSON` to name deployment binaries/checkouts and require the companion source-revision value to equal the profile's full commit before spawn. Handshake names/versions are recorded separately as observed protocol metadata.

- [ ] **Step 5: Run profile tests and public-surface checks**

Run: `cargo test --test harness_profiles -- --nocapture`

Expected: PASS; malformed unknown fields, duplicate capabilities, mismatched protocol versions, and non-full or unexpected source revisions all fail with a stable code; handshake implementation name/version are retained as non-authoritative audit metadata.

Run: `cargo check --no-default-features --lib`

Expected: PASS without enabling `acp-v2` or `test-support`.

- [ ] **Step 6: Commit the profile contract**

```bash
git add src/harness/mod.rs src/harness/types.rs src/harness/profile.rs src/lib.rs config/harness-profiles/codex-acp-v1.json config/harness-profiles/deepseek-harness-acp-v1.json tests/harness_profiles.rs
git commit -m "feat: define portable dual ACP harness profiles"
```

### Task 2: Persist Plugin Registrations and Invocation Attempts

**Files:**
- Create: `migrations/0014_harness_plugins.sql`
- Create: `src/store/harness.rs`
- Modify: `src/store/mod.rs`
- Modify: `src/store/models.rs`
- Test: `tests/harness_store.rs`

**Interfaces:**
- Consumes: Task 1 `HarnessProfileId`, `HarnessInvocationId`, profile digest, command identity/revision/payload digest, and Slice 2 `AgentCommandId` command journal.
- Produces: `HarnessPluginRegistration`, `HarnessInvocationView`, `HarnessInvocationState`, `PrepareHarnessInvocation`; `PostgresStore::{register_harness_plugin, prepare_harness_invocation, mark_harness_invocation_started, record_harness_retryable_failure, mark_harness_prompt_accepted, append_harness_invocation_event, harness_invocation_events, settle_harness_invocation, recover_harness_invocations}`.

- [ ] **Step 1: Write failing migration and store idempotency tests**

```rust
#[tokio::test]
async fn prepare_is_idempotent_for_one_command_revision_and_rejects_drift() {
    let Some(fixture) = HarnessStoreFixture::new().await else { return };
    let prepared = fixture.prepare("sha256:payload-a").await.unwrap();
    let replay = fixture.prepare("sha256:payload-a").await.unwrap();
    assert_eq!(replay.invocation_id, prepared.invocation_id);
    assert_eq!(replay.state, HarnessInvocationState::Prepared);
    let error = fixture.prepare("sha256:payload-b").await.unwrap_err();
    assert_eq!(error, StoreError::IdempotencyConflict);
}

#[tokio::test]
async fn accepted_process_death_is_recovered_as_outcome_unknown() {
    let Some(fixture) = HarnessStoreFixture::new().await else { return };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    fixture.store.mark_harness_invocation_started(&invocation.invocation_id, 0).await.unwrap();
    fixture.store.mark_harness_prompt_accepted(&invocation.invocation_id, 0).await.unwrap();
    let recovered = fixture.store.recover_harness_invocations(16).await.unwrap();
    assert_eq!(recovered[0].state, HarnessInvocationState::OutcomeUnknown);
}

#[tokio::test]
async fn event_sequence_replay_is_idempotent_and_digest_drift_conflicts() {
    let Some(fixture) = HarnessStoreFixture::new().await else { return };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let event = fixture.event(&invocation.invocation_id, 0, HarnessInvocationEvent::Started);
    fixture.store.append_harness_invocation_event(&event).await.unwrap();
    fixture.store.append_harness_invocation_event(&event).await.unwrap();
    let drift = fixture.event(&invocation.invocation_id, 0, HarnessInvocationEvent::Progress {
        message: "different bytes".to_owned(),
    });
    assert_eq!(
        fixture.store.append_harness_invocation_event(&drift).await.unwrap_err(),
        StoreError::IdempotencyConflict,
    );
}

#[tokio::test]
async fn approval_request_and_one_shot_decision_are_durable_events() {
    let Some(fixture) = HarnessStoreFixture::new().await else { return };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    fixture.append_approval_pair(&invocation.invocation_id, "permission-7", HarnessApprovalDecision::RejectOnce).await.unwrap();
    let events = fixture.store.harness_invocation_events(&invocation.invocation_id).await.unwrap();
    assert!(matches!(events[0].event, HarnessInvocationEvent::ApprovalRequested { .. }));
    assert!(matches!(events[1].event, HarnessInvocationEvent::ApprovalResolved { decision: HarnessApprovalDecision::RejectOnce, .. }));
}
```

- [ ] **Step 2: Run the focused test and confirm missing persistence APIs**

Run: `cargo test --test harness_store -- --nocapture`

Expected: FAIL to compile because `HarnessInvocationState`, `PrepareHarnessInvocation`, and the store methods do not exist.

- [ ] **Step 3: Add constrained durable tables and immutability triggers**

Create `harness_plugin_registrations` keyed by `profile_id`, with `protocol_version = 1`, `profile_digest`, exact `source_repository`, exact full `source_revision`, `transport`, `enabled`, and timestamps. Create `harness_invocations` with a unique `(command_id, command_revision)`, foreign key to `agent_commands(id)`, immutable `invocation_id`, `profile_id`, `profile_digest`, `payload_digest`, `state`, `attempt`, `prompt_accepted`, `terminal`, and timestamps. Create `harness_invocation_events` with foreign key `invocation_id`, `sequence BIGINT CHECK (sequence >= 0)`, `event_digest`, closed `event JSONB`, timestamp, and `UNIQUE (invocation_id, sequence)`.

Use this state constraint:

```sql
CHECK (state IN (
  'prepared', 'started', 'accepted', 'completed',
  'cancelled', 'failed', 'outcome_unknown'
))
```

Add a trigger that rejects changes to `invocation_id`, `command_id`, `command_revision`, `profile_id`, `profile_digest`, and `payload_digest`. Permit only these edges:

```text
prepared -> started | cancelled | failed
started -> started only to increment an attempt after a pre-accept retryable failure
started -> accepted | cancelled | failed
accepted -> completed | cancelled | failed | outcome_unknown
terminal -> same terminal state only when request digest matches
```

`append_harness_invocation_event` inserts canonical JSON and digest, treats byte-identical `(invocation_id, sequence)` replay as success, rejects digest drift with `StoreError::IdempotencyConflict`, and rejects a sequence gap. Approval request/decision pairs are ordinary closed normalized events; this is audit only and does not claim the durable user work-item approval system owned by Slice 5.

- [ ] **Step 4: Implement typed compare-and-set store methods**

```rust
pub struct PrepareHarnessInvocation<'a> {
    pub invocation_id: &'a HarnessInvocationId,
    pub command_id: &'a AgentCommandId,
    pub command_revision: u64,
    pub profile_id: &'a HarnessProfileId,
    pub profile_digest: &'a str,
    pub payload_digest: &'a str,
}

pub async fn prepare_harness_invocation(
    &self,
    request: PrepareHarnessInvocation<'_>,
) -> Result<HarnessInvocationView, StoreError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query(
        "INSERT INTO harness_invocations
         (invocation_id, command_id, command_revision, profile_id,
          profile_digest, payload_digest, state, attempt, prompt_accepted)
         VALUES ($1, $2, $3, $4, $5, $6, 'prepared', 0, FALSE)
         ON CONFLICT (command_id, command_revision) DO NOTHING",
    )
    .bind(request.invocation_id.as_uuid())
    .bind(request.command_id.as_uuid())
    .bind(i64::try_from(request.command_revision).map_err(|_| {
        StoreError::InvalidData("harness command revision is malformed".to_owned())
    })?)
    .bind(request.profile_id.as_str())
    .bind(request.profile_digest)
    .bind(request.payload_digest)
    .execute(&mut *tx)
    .await?;
    let row = sqlx::query(
        "SELECT invocation_id, command_id, command_revision, profile_id,
                profile_digest, payload_digest, state, attempt,
                prompt_accepted, terminal
         FROM harness_invocations
         WHERE command_id = $1 AND command_revision = $2
         FOR UPDATE",
    )
    .bind(request.command_id.as_uuid())
    .bind(i64::try_from(request.command_revision).map_err(|_| {
        StoreError::InvalidData("harness command revision is malformed".to_owned())
    })?)
    .fetch_one(&mut *tx)
    .await?;
    let view = harness_invocation_from_row(&row)?;
    validate_prepare_replay(&view, &request)?;
    tx.commit().await?;
    Ok(view)
}
```

Implement event append in one transaction: lock the invocation row; query `COALESCE(MAX(sequence), -1)`; require the new sequence to equal `max + 1` unless that exact sequence already exists; insert canonical bytes/digest; on conflict select the existing digest and return success only when equal.

```rust
sqlx::query(
    "INSERT INTO harness_invocation_events
     (invocation_id, sequence, event_digest, event)
     VALUES ($1, $2, $3, $4)
     ON CONFLICT (invocation_id, sequence) DO NOTHING",
)
.bind(event.invocation_id.as_uuid())
.bind(i64::try_from(event.sequence).map_err(|_| {
    StoreError::InvalidData("harness event sequence is malformed".to_owned())
})?)
.bind(&event.event_digest)
.bind(&event.event)
.execute(&mut *tx)
.await?;
```

After the insert, select `event_digest FOR UPDATE`; equality is idempotent replay and inequality is `StoreError::IdempotencyConflict`. This exact query path also persists `ApprovalRequested` and `ApprovalResolved` events.

`attempt` is the zero-based ACP process/invocation ordinal, while `max_pre_accept_attempts` is the total attempt budget. Dispatch is permitted only while `attempt < max_pre_accept_attempts`. After a retryable result, `record_harness_retryable_failure` may advance to the next ordinal and retain `started` only while `prompt_accepted = false` and `attempt + 1 < max_pre_accept_attempts`; otherwise it durably marks the same row exhausted before command-authority settlement. `mark_harness_prompt_accepted` is the persist-before-dispatch barrier: commit it immediately before the adapter writes `session/prompt`, not after the write. `recover_harness_invocations` must atomically mark every nonterminal `accepted` row `outcome_unknown`, return `prepared` rows as retryable, return `started` rows as retryable only when `prompt_accepted = false` and `attempt < max_pre_accept_attempts`, close over-budget started rows without another plugin launch, and never convert an accepted attempt back to prepared. A crash after the accepted commit but before the prompt bytes reach the child is conservatively uncertain; this is preferable to duplicating work that may mutate the workspace.

- [ ] **Step 5: Run database and migration tests**

Run: `cargo test --test harness_store -- --nocapture`

Expected with `DATABASE_URL`: PASS for registration replay, immutable identity, edge validation, terminal replay, pre-accept retry, and post-accept uncertainty. Expected without `DATABASE_URL`: PASS with the suite's explicit database-gated skip messages.

- [ ] **Step 6: Commit the durable journal**

```bash
git add migrations/0014_harness_plugins.sql src/store/harness.rs src/store/mod.rs src/store/models.rs tests/harness_store.rs
git commit -m "feat: persist harness plugin invocations"
```

### Task 3: Add the Versioned Plugin Boundary and Registry

**Files:**
- Modify: `Cargo.toml`
- Create: `src/harness/plugin.rs`
- Create: `src/harness/registry.rs`
- Modify: `src/store/harness.rs`
- Modify: `src/harness/mod.rs`
- Modify: `src/lib.rs`
- Test: `tests/harness_plugin_protocol.rs`

**Interfaces:**
- Consumes: Task 1 closed types/capability validation and Task 2 durable invocation identity.
- Produces: Frozen `HarnessPlugin`, `HarnessApprovalBroker`, `HarnessEventSink`, and `HarnessRegistry`; `HarnessRequestEnvelope { protocol_version, profile_id, invocation }`; `HarnessEventEnvelope { protocol_version, invocation_id, sequence, event_digest, event }`; `DenyAllHarnessApprovals`; `PostgresHarnessEventSink::new(store: Arc<PostgresStore>)`.

- [ ] **Step 1: Write failing envelope, registration, and cancellation tests**

```rust
#[tokio::test]
async fn registry_rejects_capability_or_protocol_drift_before_invocation() {
    let plugin = Arc::new(FakePlugin::with_protocol_version(2));
    let mut registry = HarnessRegistry::new();
    let error = registry.register(plugin).await.unwrap_err();
    assert_eq!(error.code, "harness_protocol_incompatible");
    assert_eq!(registry.active_count(), 0);
}

#[tokio::test]
async fn cancel_targets_only_the_exact_active_invocation() {
    let plugin = Arc::new(FakePlugin::wait_for_cancel());
    let registry = registry_with(plugin);
    let sink = Arc::new(RecordingHarnessEventSink::default());
    let task = tokio::spawn(registry.clone().invoke(
        request(), Arc::new(DenyAllHarnessApprovals), sink,
    ));
    assert!(registry.cancel(&request().invocation_id).await);
    assert_eq!(task.await.unwrap(), HarnessTerminal::Cancelled);
    assert!(!registry.cancel(&HarnessInvocationId::new(uuid::Uuid::now_v7())).await);
}

#[tokio::test]
async fn registry_drains_and_sequences_events_before_returning_terminal() {
    let plugin = Arc::new(FakePlugin::events_then_success(vec![
        HarnessInvocationEvent::Started,
        HarnessInvocationEvent::Progress { message: "working".to_owned() },
        HarnessInvocationEvent::AssistantMessage { text: "committed".to_owned() },
    ]));
    let sink = Arc::new(RecordingHarnessEventSink::default());
    let terminal = registry_with(plugin).invoke(request(), Arc::new(DenyAllHarnessApprovals), sink.clone()).await;
    assert!(matches!(terminal, HarnessTerminal::Completed(_)));
    assert_eq!(sink.sequences(), vec![0, 1, 2]);
}
```

- [ ] **Step 2: Run the focused test and confirm missing traits**

Run: `cargo test --test harness_plugin_protocol -- --nocapture`

Expected: FAIL to compile because `HarnessPlugin`, `HarnessRegistry`, and versioned envelopes are absent.

- [ ] **Step 3: Implement the stable traits and strict envelopes**

Enable the already-used `tokio-util` crate's cancellation primitive explicitly:

```toml
tokio-util = { version = "0.7", features = ["compat", "rt"] }
```

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessRequestEnvelope {
    pub protocol_version: u16,
    pub profile_id: HarnessProfileId,
    pub invocation: HarnessInvocationRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessEventEnvelope {
    pub protocol_version: u16,
    pub invocation_id: HarnessInvocationId,
    pub sequence: u64,
    pub event_digest: String,
    pub event: HarnessInvocationEvent,
}
```

`HarnessEventEnvelope::new` canonicalizes `{ protocolVersion, invocationId, sequence, event }`, computes `event_digest` with the existing `canonical_sha256`, and then constructs the closed envelope. Deserialization rejects unknown fields, non-version-1 envelopes, or a digest that does not recompute. Sequence zero is `Started`; subsequent values increase by exactly one; a duplicate byte-identical event is idempotent, while a reused sequence with different bytes is `harness_protocol_incompatible`.

- [ ] **Step 4: Implement registration, invocation, and exact cancellation**

```rust
pub async fn invoke(
    &self,
    request: HarnessInvocationRequest,
    approvals: Arc<dyn HarnessApprovalBroker>,
    sink: Arc<dyn HarnessEventSink>,
) -> HarnessTerminal {
    let Some(plugin) = self.plugins.get(&request.profile_id).cloned() else {
        return HarnessTerminal::Failed(HarnessFailure::new(
            "harness_profile_unavailable",
            "selected harness profile is not registered",
        ));
    };
    let token = CancellationToken::new();
    self.active.write().await.insert(request.invocation_id.clone(), token.clone());
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let invocation = plugin.start_invocation(request.clone(), events_tx, approvals, token);
    tokio::pin!(invocation);
    let mut sequence = 0;
    let terminal = loop {
        tokio::select! {
            terminal = &mut invocation => break terminal,
            Some(pending) = events_rx.recv() => {
                let envelope = HarnessEventEnvelope::new(
                    request.invocation_id.clone(), sequence, pending.event,
                );
                match sink.append(envelope).await {
                    Ok(()) => {
                        let _ = pending.persisted.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = pending.persisted.send(Err(error.clone()));
                        break HarnessTerminal::OutcomeUnknown(error);
                    }
                }
                sequence += 1;
            }
        }
    };
    while let Ok(pending) = events_rx.try_recv() {
        if let Err(error) = sink.append(HarnessEventEnvelope::new(
            request.invocation_id.clone(), sequence, pending.event,
        )).await {
            let _ = pending.persisted.send(Err(error.clone()));
            self.active.write().await.remove(&request.invocation_id);
            return HarnessTerminal::OutcomeUnknown(error);
        }
        let _ = pending.persisted.send(Ok(()));
        sequence += 1;
    }
    self.active.write().await.remove(&request.invocation_id);
    terminal
}
```

Registration calls `describe_capabilities`, validates the profile before insertion, rejects duplicate profile IDs with different digests, and permits byte-identical replay. `PostgresHarnessEventSink::append` delegates to Task 2's digest/sequence-checked store method. Plugins emit through an `emit_and_wait(events, event)` helper that creates the persistence acknowledgment channel and does not return until the registry sink confirms durable append. Registry invocation does not return until every queued normalized event is durably drained; a sink failure after work might have started is `OutcomeUnknown`, never success.

- [ ] **Step 5: Run protocol tests and serialization checks**

Run: `cargo test --test harness_plugin_protocol -- --nocapture`

Expected: PASS for duplicate registration, incompatible registration, sequence replay/drift, cross-invocation event rejection, exact cancellation, and default reject approvals.

Run: `cargo test --all-features --test agent_protocol -- --nocapture`

Expected: PASS unchanged; the harness plugin protocol is internal and does not alter the frozen ACP v1/v2 Kiteframe server extensions.

- [ ] **Step 6: Commit the plugin boundary**

```bash
git add Cargo.toml Cargo.lock src/harness/plugin.rs src/harness/registry.rs src/harness/mod.rs src/store/harness.rs src/lib.rs tests/harness_plugin_protocol.rs
git commit -m "feat: add versioned harness plugin registry"
```

### Task 4: Extract the Built-in Harness and Replace the Slice 2 Executor

**Files:**
- Create: `src/harness/builtin.rs`
- Create: `tests/support/harness_conformance.rs`
- Create: `tests/harness_builtin_conformance.rs`
- Modify: `src/engine/harness.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Modify: `tests/agent_direct_bridge.rs`

**Interfaces:**
- Consumes: Slice 2 `DirectLoopHarnessExecutor` behavior, Task 2 journal, Task 3 registry/plugin contract, and the unchanged `CompleteAgentCommand`/`FailAgentCommand` kernel methods.
- Produces: `BuiltinHarnessPlugin::new(profile: HarnessProfile, model: Arc<dyn ModelClient>)`; `AgentKernel::with_harness_registry(registry: Arc<HarnessRegistry>, approvals: Arc<dyn HarnessApprovalBroker>) -> AgentCommandDispatcher`; `HarnessConformanceDriver`; `assert_normative_harness(driver: impl HarnessConformanceDriver)`. Moves `HarnessExecutionResult` and `HarnessExecutionError` from `src/engine/harness.rs` to `src/harness/builtin.rs`, re-exports them through `src/harness/mod.rs`, and retains transitional re-exports from `src/engine/mod.rs`/`src/lib.rs` so existing callers do not form an import cycle.

- [ ] **Step 1: Write the reusable normative suite and built-in adapter test**

```rust
pub enum HarnessConformanceCase {
    Complete,
    Replay,
    RejectPermission,
    CancelInFlight,
    InvalidCompletion,
}

pub struct HarnessConformanceObservation {
    pub terminal: HarnessTerminal,
    pub terminal_count: u64,
    pub goal_phase: GoalPhase,
}

#[async_trait::async_trait]
pub trait HarnessConformanceDriver {
    async fn run(
        &mut self,
        case: HarnessConformanceCase,
    ) -> HarnessConformanceObservation;
}

fn terminal_code(terminal: &HarnessTerminal) -> Option<&str> {
    match terminal {
        HarnessTerminal::Cancelled => Some("harness_cancelled"),
        HarnessTerminal::Retryable(failure)
        | HarnessTerminal::Failed(failure)
        | HarnessTerminal::OutcomeUnknown(failure) => {
            Some(failure.code)
        }
        HarnessTerminal::Completed(_) => None,
    }
}

pub async fn assert_normative_harness(mut harness: impl HarnessConformanceDriver) {
    let completed = harness.run(HarnessConformanceCase::Complete).await;
    assert!(matches!(completed.terminal, HarnessTerminal::Completed(_)));
    assert_eq!(completed.goal_phase, GoalPhase::Active);

    let replay = harness.run(HarnessConformanceCase::Replay).await;
    assert_eq!(replay.terminal_count, 1);

    let rejected = harness.run(HarnessConformanceCase::RejectPermission).await;
    assert_eq!(terminal_code(&rejected.terminal), Some("harness_permission_rejected"));

    let cancelled = harness.run(HarnessConformanceCase::CancelInFlight).await;
    assert_eq!(terminal_code(&cancelled.terminal), Some("harness_cancelled"));

    let invalid = harness.run(HarnessConformanceCase::InvalidCompletion).await;
    assert_eq!(terminal_code(&invalid.terminal), Some("harness_failed"));
    assert_eq!(invalid.goal_phase, GoalPhase::Active);
}

#[tokio::test]
async fn builtin_passes_the_normative_v1_suite() {
    assert_normative_harness(BuiltinConformanceDriver::new()).await;
}
```

- [ ] **Step 2: Run the built-in conformance test and confirm the missing adapter**

Run: `cargo test --test harness_builtin_conformance -- --nocapture`

Expected: FAIL to compile because `BuiltinHarnessPlugin` and `with_harness_registry` are absent.

- [ ] **Step 3: Move direct-loop behavior without semantic changes**

Move `HarnessExecutionResult`, `HarnessExecutionError`, `DURABLE_GOAL_DIRECTIVE`, request construction, limit enforcement, completion-tool construction, response-item/tool-call limits, completion parsing, schema validation, and history normalization from `src/engine/harness.rs` to `src/harness/builtin.rs`. `src/engine/harness.rs` imports only `HarnessPlugin`-level types after the move; `src/harness/builtin.rs` does not import `AgentCommandDispatcher`, preventing a module cycle. Its plugin terminal mapping is exact:

```rust
match self.execute_direct(&request.payload).await {
    Ok(result) => HarnessTerminal::Completed(HarnessInvocationOutput {
        outcome: result.outcome,
        fields: result.fields,
        history: result.history,
    }),
    Err(HarnessExecutionError::Unavailable(detail)) => HarnessTerminal::Failed(
        HarnessFailure::new("harness_capability_unsupported", detail),
    ),
    Err(error) => HarnessTerminal::Failed(
        HarnessFailure::new("harness_failed", error.to_string()),
    ),
}
```

- [ ] **Step 4: Make the dispatcher journal and invoke through the registry**

Replace `BUILTIN_CLAIMANT` with `HARNESS_DISPATCHER_CLAIMANT = "harness-dispatcher-v1"`. After command claim, select the profile through `HarnessRouteTable`, generate `HarnessInvocationId::new(uuid::Uuid::now_v7())`, and call Task 2's atomic prepare. The first insert stores that UUIDv7; a unique `(command_id, command_revision)` conflict locks and returns the existing immutable row after digest/profile comparison, discarding the newly generated candidate. Only then call `registry.invoke` with `PostgresHarnessEventSink`. Map terminals as follows:

```text
Completed       -> RuntimeEvent::HarnessResult -> AgentKernel::complete_command
Cancelled       -> FailAgentCommand("harness_cancelled") -> AgentKernel::fail_command
Retryable       -> record pre-accept attempt; keep command claimed for bounded redispatch
Failed(code)    -> FailAgentCommand(code) -> AgentKernel::fail_command
OutcomeUnknown  -> journal outcome_unknown + FailAgentCommand("harness_outcome_unknown")
```

The event id and idempotency key use `harness-result:<invocation-uuid>` and `harness-terminal:<invocation-uuid>` so replay cannot mint a second evidence record.

- [ ] **Step 5: Run conformance and Slice 2 regression tests**

Run: `cargo test --test harness_builtin_conformance --test agent_direct_bridge --test agent_goal_authority --test agent_transition_store -- --nocapture`

Expected: PASS; built-in success still mints exactly one `harness.invocation_result`, invalid result still fails the bounded task, and no harness result completes or blocks a durable goal.

Run: `cargo test --test sequential_runtime --test structured_runtime --test recovery`

Expected: PASS with unchanged direct Responses behavior.

- [ ] **Step 6: Commit the built-in extraction**

```bash
git add src/harness/builtin.rs src/engine/harness.rs src/engine/mod.rs src/lib.rs src/main.rs tests/support/harness_conformance.rs tests/harness_builtin_conformance.rs tests/agent_direct_bridge.rs
git commit -m "refactor: run built-in model loop as harness plugin"
```

## Cross-task review gate A: Tasks 1-4

This gate is blocking. Use a fresh reviewer with the spec and Slice 2 handoff open. Reject the cumulative diff unless all statements below have concrete evidence:

- the old `HarnessCommandExecutor` trait is gone and its direct behavior is preserved behind `BuiltinHarnessPlugin`;
- profile source provenance and capability sets are closed, exact, digested, and deployment-owned rather than `.kite`-owned;
- every outbound invocation has a durable, immutable command-bound journal row before plugin work;
- plugin registration and events use protocol version 1, exact identities, monotonic sequence, and fail-closed negotiation;
- accepted work cannot be silently retried after process loss;
- built-in completion still creates only invocation evidence and cannot mutate goal authority;
- no ACP subprocess, Codex-specific behavior, DeepSeek-specific behavior, Cordis lifecycle, or Slice 4-8 subsystem was smuggled into Tasks 1-4.

Run:

```bash
cargo test --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance --test agent_direct_bridge --test agent_goal_authority --test agent_transition_store
cargo test --test harness_store --test postgres_lock_order
cargo check --no-default-features --lib --bin kiteframe-harness
git diff --check HEAD~4..HEAD
```

Expected: all non-database tests PASS; database tests PASS or emit only their explicit `DATABASE_URL` skips; check and diff-check PASS. Record review findings and fixes in the implementation task, make each fix in its owning task commit or a path-scoped `fix:` commit, rerun the gate, and continue only after zero unresolved findings.

### Task 5: Implement the Provider-neutral ACP Stdio Core

**Files:**
- Modify: `Cargo.toml`
- Create: `src/harness/acp/mod.rs`
- Create: `src/harness/acp/client.rs`
- Create: `src/harness/acp/adapter.rs`
- Create: `src/harness/acp/permissions.rs`
- Create: `src/bin/kiteframe-fake-acp.rs`
- Create: `tests/fixtures/acp/core-success.json`
- Create: `tests/fixtures/acp/permission-once.json`
- Create: `tests/fixtures/acp/cancel.json`
- Create: `tests/fixtures/acp/incompatible.json`
- Create: `tests/fixtures/acp/process-death.json`
- Create: `tests/harness_acp_conformance.rs`

**Interfaces:**
- Consumes: Tasks 1-4 `HarnessPlugin`, exact profiles, approval broker, journal semantics, normative suite, the ACP method/content/stop-reason schema compatible with `agent-client-protocol = 2.0.0`, and Tokio process/cancellation primitives. The adapter deliberately implements bounded raw JSON-RPC framing; it does not invent or depend on an `agent-client-protocol` subprocess-client API.
- Produces: `AcpHarnessPlugin::new(profile: HarnessProfile, command: AcpCommand, limits: AcpLimits)`; `AcpStdioClient::{spawn, initialize, new_session, prompt, cancel, shutdown}`; `select_permission_once(options: &[PermissionOption], decision: HarnessApprovalDecision) -> Result<PermissionOptionId, HarnessFailure>`.

- [ ] **Step 1: Write failing fake-process conformance tests**

```rust
#[tokio::test]
async fn scripted_acp_passes_the_same_normative_suite() {
    assert_normative_harness(AcpConformanceDriver::fixture("core-success.json")).await;
}

#[tokio::test]
async fn permission_answer_is_one_shot_and_correlated() {
    let driver = AcpConformanceDriver::fixture("permission-once.json");
    let terminal = driver.invoke_with(HarnessApprovalDecision::AllowOnce).await;
    assert!(matches!(terminal, HarnessTerminal::Completed(_)));
    assert_eq!(driver.selected_permission_option(), "allow_once");
    assert_eq!(driver.permission_response_count(), 1);
}

#[tokio::test]
async fn eof_after_prompt_acceptance_is_outcome_unknown() {
    let terminal = AcpConformanceDriver::fixture("process-death.json").invoke().await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
}
```

- [ ] **Step 2: Run the focused test and confirm the missing ACP client**

Run: `cargo test --features test-support --test harness_acp_conformance -- --nocapture`

Expected: FAIL to compile because `AcpHarnessPlugin`, `AcpStdioClient`, and the fake ACP binary target do not exist.

- [ ] **Step 3: Register and implement the scripted fake ACP process**

Add this target without changing `default-run`:

```toml
[[bin]]
name = "kiteframe-fake-acp"
path = "src/bin/kiteframe-fake-acp.rs"
required-features = ["test-support"]
```

The fake binary reads `KITEFRAME_FAKE_ACP_SCRIPT`, reserves stdout exclusively for newline-delimited JSON-RPC frames, emits diagnostics on stderr, checks each expected method and selected fields, and exits nonzero on drift. Each fixture contains a closed ordered array of entries such as `{ "expect": { "method": "initialize" }, "respond": { "protocolVersion": 1 } }` and `{ "notify": { "method": "session/update", "params": { "sessionId": "session-1" } } }`; `process-death.json` exits immediately after reading the prompt.

- [ ] **Step 4: Implement JSON-RPC correlation and process lifecycle**

`AcpStdioClient` owns one child, stdin writer task, stdout reader task, bounded pending-request map, stderr drain, and one connection cancellation token. `spawn` sets the absolute workspace as `current_dir`, uses `kill_on_drop(true)`, removes ambient secrets except explicitly allowlisted profile environment, and rejects stdout that is not valid JSON-RPC.

```rust
pub async fn request<P, R>(&self, method: &'static str, params: P) -> Result<R, AcpError>
where
    P: Serialize,
    R: DeserializeOwned,
{
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    self.pending.lock().await.insert(id, tx);
    self.write_json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await?;
    rx.await.map_err(|_| self.classify_process_loss())?
}
```

Define closed local wire DTOs for `InitializeRequest/Response`, `NewSessionRequest/Response`, `PromptRequest/Response`, `CancelNotification`, `SessionUpdate`, and `RequestPermission`. Unit tests serialize each DTO and compare it with the `agent-client-protocol = 2.0.0` golden method names/field casing already exercised by `tests/acp_conformance.rs`.

EOF before the durable accepted barrier returns `HarnessTerminal::Retryable(HarnessFailure::new("harness_failed", ...))`. The dispatcher records the attempt and redispatches only while `max_pre_accept_attempts` permits; exhaustion becomes terminal `harness_failed`. Immediately before writing `session/prompt`, the adapter commits `mark_harness_prompt_accepted`; any EOF or crash after that commit—including loss before the write completes—is `harness_outcome_unknown` and is never transparently replayed. The barrier deliberately over-approximates uncertainty to close the database-to-child-process crash window.

- [ ] **Step 5: Implement the exact portable session flow**

The adapter performs only:

```text
initialize(protocolVersion = supported stable version)
validate observed identity + required portable core
session/new(cwd = absolute, additionalDirectories = [], mcpServers = [])
commit durable accepted barrier
session/prompt(prompt = one ordered text block)
handle session/update committed assistant text
handle session/request_permission with exactly one AllowOnce/RejectOnce response
map stopReason end_turn/cancelled
shutdown/EOF cleanup
```

ACP core methods are protocol baseline operations, not optional bits expected in `initialize.capabilities`. `AcpHarnessPlugin::describe_capabilities` reports `FreshSession`, `TextPrompt`, `PermissionOnce`, `Cancel`, and `CommittedOutput` from the adapter/profile contract after version negotiation and deterministic conformance; it derives only richer optional features from advertised capability fields. This prevents DeepSeek's intentionally empty editor/terminal/filesystem/MCP capability object from being misclassified as lacking the portable core.

Serialize the entire `InvokeHarness` goal/input/result contract into the text prompt with canonical JSON and the existing durable-goal directive. Parse the final committed assistant text as one strict JSON object `{ "outcome": string, "fields": object }`, then call the same completion-schema validator used by the built-in plugin. Reject image/audio/embedded context, non-empty MCP servers/additional directories, session load/resume, and unnegotiated rich capabilities before launching work.

- [ ] **Step 6: Implement permission and cancellation behavior**

```rust
pub fn select_permission_once(
    options: &[PermissionOption],
    decision: HarnessApprovalDecision,
) -> Result<PermissionOptionId, HarnessFailure> {
    let wanted = match decision {
        HarnessApprovalDecision::AllowOnce => "allow_once",
        HarnessApprovalDecision::RejectOnce => "reject_once",
    };
    options.iter().find(|option| option.kind == wanted)
        .map(|option| option.id.clone())
        .ok_or_else(|| HarnessFailure::new(
            "harness_capability_unsupported",
            "ACP agent did not offer the required one-shot permission option",
        ))
}
```

For each correlated `session/request_permission`, the adapter sends `HarnessInvocationEvent::ApprovalRequested`, awaits `approvals.decide` exactly once, sends `HarnessInvocationEvent::ApprovalResolved` with that one-shot decision, waits for both events to be accepted by the registry sink ordering, and then returns exactly one JSON-RPC response selecting the matching option ID. It never selects `allow_always`; a missing one-shot option fails closed. This durable event pair is an invocation audit, not the user-visible leased approval work-item infrastructure deferred to Slice 5. Production startup uses `DenyAllHarnessApprovals` until that later infrastructure supplies another broker; live conformance uses an explicit fixture broker scoped to its temporary workspace.

When the registry token is cancelled, send `session/cancel` exactly once, wait only through the invocation's remaining active-execution limit, map `cancelled` to `HarnessTerminal::Cancelled`, and kill the connection if it does not quiesce. A late committed message after cancellation is audit-only and cannot complete the command.

- [ ] **Step 7: Run all fake ACP wire cases**

Run: `cargo test --features test-support --test harness_acp_conformance -- --nocapture`

Expected: PASS for success, strict result parsing, allow-once, reject-once, cancellation, protocol mismatch, missing required capability, malformed frame, duplicate response, timeout, EOF before acceptance, and EOF after acceptance.

Run: `cargo test --all-features --test acp_conformance --test transports -- --nocapture`

Expected: PASS; Kiteframe's existing inbound ACP v1/v2 server remains independent from the new outbound harness ACP client.

- [ ] **Step 8: Commit the generic ACP core**

```bash
git add Cargo.toml src/harness/acp/mod.rs src/harness/acp/client.rs src/harness/acp/adapter.rs src/harness/acp/permissions.rs src/bin/kiteframe-fake-acp.rs tests/fixtures/acp/core-success.json tests/fixtures/acp/permission-once.json tests/fixtures/acp/cancel.json tests/fixtures/acp/incompatible.json tests/fixtures/acp/process-death.json tests/harness_acp_conformance.rs
git commit -m "feat: add provider-neutral ACP harness adapter"
```

### Task 6: Certify Codex and DeepSeek Against One Profile Matrix

**Files:**
- Create: `tests/harness_acp_live.rs`
- Modify: `tests/harness_profiles.rs`
- Modify: `src/main.rs`
- Modify: `src/config.rs`

**Interfaces:**
- Consumes: Task 1 exact profiles, Task 5 ACP adapter, and Task 4 normative conformance driver.
- Produces: `ExternalHarnessCommands { codex: Option<AcpCommand>, deepseek: Option<AcpCommand> }`; `HarnessConfig::external_harness_commands()`; `HarnessConfig::harness_routes()` parsed from `KITEFRAME_REASONING_HARNESS_PROFILE` and `KITEFRAME_CODING_HARNESS_PROFILE` with `builtin-v1` defaults; live command gates `KITEFRAME_CODEX_ACP_COMMAND_JSON` and `KITEFRAME_DSH_ACP_COMMAND_JSON`; source-attestation gates `KITEFRAME_CODEX_ACP_SOURCE_REVISION` and `KITEFRAME_DSH_ACP_SOURCE_REVISION`.

- [ ] **Step 1: Write failing shared-matrix and environment-validation tests**

```rust
#[test]
fn command_overrides_are_json_arrays_without_shell_interpolation() {
    let config = HarnessConfig::from_test_values([
        ("KITEFRAME_CODEX_ACP_COMMAND_JSON", r#"["/opt/codex-acp/bin/codex-acp"]"#),
        ("KITEFRAME_CODEX_ACP_SOURCE_REVISION", "296069e841634cd4bb9bc4515602d836e49231ec"),
        ("KITEFRAME_DSH_ACP_COMMAND_JSON", r#"["pnpm","--dir","/opt/deepseek-harness","run","demo:acp"]"#),
        ("KITEFRAME_DSH_ACP_SOURCE_REVISION", "141eb6fef83422698aef7a981029e843e8161534"),
    ]).unwrap();
    assert_eq!(config.external_harness_commands().codex.unwrap().program(), "/opt/codex-acp/bin/codex-acp");
    assert_eq!(config.external_harness_commands().deepseek.unwrap().program(), "pnpm");
}

#[tokio::test]
async fn live_codex_and_deepseek_use_the_same_normative_cases() {
    for target in LiveTarget::enabled() {
        assert_normative_harness(target.driver()).await;
    }
}
```

- [ ] **Step 2: Run focused tests and confirm missing external command config**

Run: `cargo test --test harness_profiles --test harness_acp_live -- --nocapture`

Expected: FAIL to compile because `ExternalHarnessCommands`, environment parsing, and live driver are absent.

- [ ] **Step 3: Parse command arrays without invoking a shell**

Parse each command environment variable as a non-empty JSON string array. The first element is the executable and the rest are literal arguments passed to `tokio::process::Command`; reject empty strings, NUL, relative workspace overrides, shell metacharacter interpretation, and secret values in arguments. A command without its companion exact 40-character source revision is invalid configuration. Absence of both means that live target is skipped, not silently replaced by another profile.

- [ ] **Step 4: Register both external profiles at startup**

`src/main.rs` loads both checked-in JSON profiles, verifies their digests, repository URLs, and full source revisions, constructs `AcpHarnessPlugin` only when its explicit command plus matching source attestation are configured, and registers both alongside the built-in plugin. Semantic `HarnessRole::Coding` selection uses deployment policy/profile ID; `.kite` never names Codex, DeepSeek, source revisions, credentials, or command paths.

If source attestation or the configured process protocol version/capabilities do not match its profile, startup fails before dispatch with `harness_protocol_incompatible`. If a selected profile is not configured, its command fails with `harness_profile_unavailable`; it never falls back to built-in or the other provider.

- [ ] **Step 5: Implement separately gated live suites with the same assertions**

The Codex case is enabled only when `KITEFRAME_CODEX_ACP_COMMAND_JSON`, matching `KITEFRAME_CODEX_ACP_SOURCE_REVISION`, and existing Codex authentication are present. The DeepSeek case is enabled only when `KITEFRAME_DSH_ACP_COMMAND_JSON`, matching `KITEFRAME_DSH_ACP_SOURCE_REVISION`, and that deployment's existing provider authentication/configuration are present. Each suite uses a temporary absolute workspace, one fresh session, a text-only deterministic task, one harmless one-shot permission case, cancellation, and strict JSON completion.

```rust
#[tokio::test]
async fn codex_acp_live_core() {
    let Some(driver) = LiveTarget::Codex.driver_if_enabled().await else {
        eprintln!("SKIP codex live: KITEFRAME_CODEX_ACP_COMMAND_JSON is absent");
        return;
    };
    assert_live_portable_core(driver).await;
}

#[tokio::test]
async fn deepseek_harness_acp_live_core() {
    let Some(driver) = LiveTarget::DeepSeek.driver_if_enabled().await else {
        eprintln!("SKIP DeepSeek live: KITEFRAME_DSH_ACP_COMMAND_JSON is absent");
        return;
    };
    assert_live_portable_core(driver).await;
}
```

- [ ] **Step 6: Run deterministic and live profile commands**

Run: `cargo test --features test-support --test harness_profiles --test harness_acp_conformance --test harness_acp_live -- --nocapture`

Expected without live variables: deterministic tests PASS and both live tests print their distinct SKIP line. Expected with one variable: that provider's portable-core suite PASS and the other prints SKIP. Expected with both: both run the same assertions and PASS independently.

- [ ] **Step 7: Commit dual-profile startup and certification**

```bash
git add src/config.rs src/main.rs tests/harness_profiles.rs tests/harness_acp_live.rs
git commit -m "feat: certify Codex and DeepSeek ACP profiles"
```

### Task 7: Close Cancellation, Idempotency, Process-death, and Restart Recovery

**Files:**
- Create: `tests/harness_acp_recovery.rs`
- Modify: `src/engine/harness.rs`
- Modify: `src/store/harness.rs`
- Modify: `src/main.rs`
- Modify: `tests/agent_direct_bridge.rs`
- Modify: `tests/e2e_agent_restart.rs`

**Interfaces:**
- Consumes: Task 2 invocation journal, Task 3 exact cancellation, Task 5 process-loss classification, Slice 2 command recovery, and kernel compare-and-set completion/failure.
- Produces: `HarnessRecoveryReport { retryable_pre_accept: u64, outcome_unknown: u64, terminal: u64 }`; `AgentCommandDispatcher::recover_harness_invocations(limit: i64) -> Result<HarnessRecoveryReport, StoreError>`.

- [ ] **Step 1: Write failing crash-boundary and replay tests**

```rust
#[tokio::test]
async fn restart_before_prompt_acceptance_reuses_invocation_and_sends_once() {
    let Some(fixture) = RecoveryFixture::new("die-before-prompt").await else { return };
    let first = fixture.prepare_and_crash().await;
    let recovered = fixture.restart().await.unwrap();
    assert_eq!(recovered.invocation_id, first.invocation_id);
    assert_eq!(fixture.prompt_count().await, 1);
}

#[tokio::test]
async fn restart_after_prompt_acceptance_never_reassigns_or_completes_goal() {
    let Some(fixture) = RecoveryFixture::new("process-death.json").await else { return };
    fixture.accept_and_crash().await;
    let report = fixture.restart().await.unwrap();
    assert_eq!(report.outcome_unknown, 1);
    assert_eq!(fixture.prompt_count().await, 1);
    assert_eq!(fixture.evidence_count().await, 0);
    assert_eq!(fixture.goal_phase().await, GoalPhase::Active);
}

#[tokio::test]
async fn cancellation_wins_over_late_success() {
    let Some(fixture) = RecoveryFixture::new("late-success-after-cancel").await else { return };
    fixture.cancel_task().await;
    fixture.deliver_late_success().await;
    assert_eq!(fixture.command_state().await, AgentCommandState::Cancelled);
    assert_eq!(fixture.evidence_count().await, 0);
}
```

- [ ] **Step 2: Run recovery tests and confirm missing recovery report**

Run: `cargo test --features test-support --test harness_acp_recovery -- --nocapture`

Expected: FAIL to compile because `HarnessRecoveryReport` and dispatcher recovery do not exist.

- [ ] **Step 3: Recover durable attempts before dispatch starts**

At startup, call `recover_harness_invocations` after Slice 2 `agent_kernel().recover` and before spawning dispatcher loops. For `prepared` and unaccepted `started`, keep the same invocation ID and permit bounded replay through the same selected profile; `record_harness_retryable_failure` owns attempt increments, so recovery never increments twice. Exhausted pre-accept attempts settle `harness_failed`. For `accepted`—including a crash after the barrier but before the prompt write completes—atomically mark `outcome_unknown`, fail the bounded command with `harness_outcome_unknown`, and leave the durable goal active. Terminal rows require no work.

```rust
let harness_recovery = dispatcher
    .recover_harness_invocations(engine.config.scheduler_batch_size)
    .await?;
tracing::info!(
    retryable_pre_accept = harness_recovery.retryable_pre_accept,
    outcome_unknown = harness_recovery.outcome_unknown,
    terminal = harness_recovery.terminal,
    "harness invocation recovery complete"
);
```

- [ ] **Step 4: Make cancellation and late results converge idempotently**

The dispatcher watches task/command terminalization while a plugin is active. Task cancellation calls `registry.cancel(invocation_id)` and persists `cancelled` with one terminal digest. If completion races cancellation, the existing kernel compare-and-set winner remains authoritative; the losing terminal is recorded as late audit metadata and cannot append evidence, alter goal phase, or change the command terminal state.

- [ ] **Step 5: Prove all crash boundaries with PostgreSQL and fake ACP**

Cover crashes: before journal insert, after prepare, after process spawn, after initialize, after `session/new`, before the accepted barrier, after the accepted barrier but before prompt write, after prompt write, after committed output but before kernel completion, and after kernel completion but before subprocess shutdown. Assert that only pre-barrier rows retry, every post-barrier nonterminal row becomes uncertain without another prompt, no evidence duplicates, invocation/result idempotency keys remain stable, and the goal stays active unless the agent runtime later consumes the one valid harness result.

- [ ] **Step 6: Run restart and authority regression suites**

Run: `cargo test --features test-support --test harness_acp_recovery --test e2e_agent_restart --test agent_direct_bridge --test agent_goal_authority -- --nocapture`

Expected with `DATABASE_URL`: PASS across every crash injection. Expected without `DATABASE_URL`: deterministic non-database cases PASS and database cases print explicit skips.

Run: `cargo test --all-features --test agent_transition_store exact_completion_replays_when_winner_commits_after_empty_probe -- --exact --nocapture`

Expected: PASS; the existing completion-race proof remains intact.

- [ ] **Step 7: Commit recovery closure**

```bash
git add src/engine/harness.rs src/store/harness.rs src/main.rs tests/harness_acp_recovery.rs tests/agent_direct_bridge.rs tests/e2e_agent_restart.rs
git commit -m "feat: recover ACP harness invocations safely"
```

### Task 8: Publish Slice 3 Truth and the Dual-harness Release Gate

**Files:**
- Modify: `README.md`
- Modify: `docs/guides/building-on-kiteframe.md`
- Modify: `docs/guides/testing-kiteframe.md`
- Modify: `docs/reference/agent-runtime-and-goal-lifecycle.md`
- Modify: `tests/repository_shape.rs`

**Interfaces:**
- Consumes: Tasks 1-7 verified behavior and the normative design's Slice 3 acceptance contract.
- Produces: accurate public support statement, operator/test commands, explicit Cordis deferral, repository-shape guard, and cumulative Slice 3 evidence block.

- [ ] **Step 1: Write failing repository-truth assertions**

```rust
#[test]
fn public_docs_name_both_v1_acp_profiles_and_exclude_dynamic_cordis() {
    let readme = read("README.md");
    let guide = read("docs/guides/building-on-kiteframe.md");
    assert!(readme.contains("Codex ACP and DeepSeek Harness ACP"));
    assert!(guide.contains("portable ACP core"));
    assert!(guide.contains("Cordis auto-composability is deferred from V1"));
}

#[test]
fn no_v1_runtime_surface_exposes_cordis_package_lifecycle() {
    for source in rust_sources() {
        let text = read(&source);
        for forbidden in ["cordis_define", "cordis_run", "cordis_stop", "CompositionProposed", "PackageActivated"] {
            assert!(!text.contains(forbidden), "{forbidden} leaked into {}", source.display());
        }
    }
}
```

- [ ] **Step 2: Run repository-shape tests and confirm stale documentation**

Run: `cargo test --test repository_shape -- --nocapture`

Expected: FAIL because public documentation does not yet claim the reviewed dual-harness Slice 3 result or state the Cordis exclusion.

- [ ] **Step 3: Document the exact authority and portability contract**

Update public docs with these facts only after Tasks 1-7 pass:

```text
- Built-in Responses, Codex ACP, and DeepSeek Harness ACP implement one HarnessPlugin contract.
- Codex and DeepSeek share the same required fresh-session/text/permission-once/cancel/committed-output core.
- Rich Codex features are optional and fail closed when requested from a profile that lacks them.
- DeepSeek uses fresh connection-owned sessions and does not promise resume, editor, MCP, or rich progress surfaces.
- Harness output is bounded invocation evidence; only the Rust kernel commits durable task/goal state.
- Cordis auto-composability and dynamic package lifecycle are deferred from V1.
```

Do not claim GitHub connectors, IDE delivery, guarded worker leases, Slice 4 context/event infrastructure, Slice 5 work delivery, Slice 6 connector control plane, Slice 7 editor path, or Slice 8 `babysit_pr` E2E completion.

- [ ] **Step 4: Document deterministic and live commands**

Add exactly these operator commands to `docs/guides/testing-kiteframe.md`:

```bash
cargo test --features test-support --test harness_acp_conformance --test harness_acp_recovery
cargo test --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance
cargo test --test harness_store
KITEFRAME_CODEX_ACP_COMMAND_JSON='["/absolute/codex-acp"]' KITEFRAME_CODEX_ACP_SOURCE_REVISION='296069e841634cd4bb9bc4515602d836e49231ec' cargo test --test harness_acp_live codex_acp_live_core -- --exact --nocapture
KITEFRAME_DSH_ACP_COMMAND_JSON='["pnpm","--dir","/absolute/deepseek-harness","run","demo:acp"]' KITEFRAME_DSH_ACP_SOURCE_REVISION='141eb6fef83422698aef7a981029e843e8161534' cargo test --test harness_acp_live deepseek_harness_acp_live_core -- --exact --nocapture
```

State that each executable/check-out must be built from the exact checked-in repository and full commit, and that the source-revision environment value is a deployment attestation checked before spawn. A different revision or incompatible observed ACP protocol/capability set fails before invocation; self-reported implementation names/versions are audit metadata only.

- [ ] **Step 5: Run the complete Slice 3 release matrix**

Run:

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo test --features test-support --test harness_profiles --test harness_plugin_protocol --test harness_builtin_conformance --test harness_acp_conformance --test harness_acp_recovery --test repository_shape
cargo test --test harness_store --test postgres_lock_order
cargo test --all-features --test agent_runtime_contract --test agent_lifecycle_store --test agent_start --test kite_agent_runtime --test agent_transition_store --test agent_goal_authority --test agent_direct_bridge --test agent_protocol --test e2e_agent_restart --test acp_conformance --test transports
cargo test --all-features
cargo check --no-default-features --lib --bin kiteframe-harness
python -m pytest clients/python/tests
bun --cwd clients/typescript test
bun --cwd clients/typescript run typecheck
git diff --check
```

Expected: every available deterministic, PostgreSQL, Rust, Python, and TypeScript gate PASS. If live credentials/commands are absent, record the two exact SKIP lines separately; do not convert a skip into live-adapter implementation evidence.

- [ ] **Step 6: Record implementation evidence without overstating later slices**

Add a dated Slice 3 evidence block to `docs/reference/agent-runtime-and-goal-lifecycle.md` containing the implementation commit, review status, deterministic command results, PostgreSQL result, Codex live result or exact skip, DeepSeek live result or exact skip, and explicit `Slices 4-8: not implemented by this evidence` statement.

- [ ] **Step 7: Commit the verified public truth**

```bash
git add README.md docs/guides/building-on-kiteframe.md docs/guides/testing-kiteframe.md docs/reference/agent-runtime-and-goal-lifecycle.md tests/repository_shape.rs
git commit -m "docs: publish dual ACP harness Slice 3 evidence"
```

## Cross-task review gate B: Tasks 1-8 and Slice 4 handoff

This gate is blocking and cumulative. Use a fresh reviewer who did not implement Tasks 5-8. The reviewer must compare the diff with every Harness plugin, testing/acceptance, V1 boundary, and Slice 3 roadmap clause in the spec, plus the exact Slice 2 handoff.

The reviewer must reject the slice unless:

- built-in, fake ACP, Codex ACP, and DeepSeek Harness ACP all use the same versioned plugin types and normative conformance assertions;
- Codex and DeepSeek selection occurs only through deployment-owned profile IDs and exact identities, never `.kite` source;
- the common contract requires only the portable ACP core and every optional feature fails closed;
- DeepSeek session behavior is fresh, text-first, one-in-flight, connection-owned, committed-output, one-shot-permission, and cancellation-safe;
- prompt acceptance is a durable boundary and accepted process death becomes `harness_outcome_unknown` without transparent reassignment;
- replay reuses command, invocation, profile, payload digest, idempotency key, and terminal record;
- cancellation and late success converge through existing kernel compare-and-set authority without duplicate evidence;
- a harness terminal result cannot complete or block the durable goal;
- both external live suites are independently gated, and a skip is reported as a skip;
- Cordis auto-composability, dynamic packages, marketplace, hot reload, and runtime-generated authority are absent;
- public docs claim Slice 3 only and explicitly leave Slices 4-8 prospective.

Run:

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo test --all-features
cargo test --features test-support --test harness_acp_conformance --test harness_acp_recovery
cargo check --no-default-features --lib --bin kiteframe-harness
python -m pytest clients/python/tests
bun --cwd clients/typescript test
bun --cwd clients/typescript run typecheck
rg -n "cordis_define|cordis_run|cordis_stop|CompositionProposed|PackageActivated" src tests config
git diff --check HEAD~8..HEAD
git status --short
```

Expected: all gates PASS; the Cordis scan has no matches outside the explicit negative-test strings in `tests/repository_shape.rs`; status contains no unexpected staged paths and preserves unrelated `fuzz/` and `site/` entries.

Record every finding, fix it in a path-scoped commit, rerun the relevant focused command and the cumulative gate, and approve Slice 3 only with zero unresolved findings.

## Slice 4 Handoff

Slice 4 receives, without renaming or weakening:

```rust
HarnessProfile
HarnessCapabilities
HarnessInvocationId
HarnessInvocationRequest
HarnessInvocationEvent
HarnessTerminal
HarnessFailure
HarnessPlugin
HarnessApprovalBroker
HarnessRegistry
BuiltinHarnessPlugin
AcpHarnessPlugin
PostgresStore::{
    register_harness_plugin,
    prepare_harness_invocation,
    mark_harness_invocation_started,
    record_harness_retryable_failure,
    mark_harness_prompt_accepted,
    settle_harness_invocation,
    recover_harness_invocations,
}
```

Slice 4 may add immutable provenance-bearing context artifacts and semantic event ingress to bounded invocations. It must not add provider names to `.kite`, treat ACP session persistence as durable Kiteframe state, weaken prompt-acceptance recovery, or allow context providers to complete goals. Rich Codex context and future DeepSeek capabilities remain profile-negotiated delivery optimizations; the portable core remains sufficient for V1 semantics.

## Plan Self-review Evidence

- **Spec coverage:** Tasks 1-4 cover stable registration, versioned envelopes, capabilities, built-in extraction, persistence, authority, common conformance, and recovery foundations. Tasks 5-7 cover ACP stdio, both V1 profiles, approval, cancellation, incompatibility, process death, idempotency, and restart. Task 8 covers release evidence, accurate public truth, and the Slice 4 handoff. Cordis deferral is enforced in Global Constraints, Tasks 1, 5, 8, and both review gates.
- **Placeholder scan:** A case-sensitive scan for the skill's forbidden placeholder tokens and vague implementation phrases returns no matches; every test and implementation step names its concrete behavior, command, and expected result.
- **Type consistency:** `HarnessProfileId`, `HarnessInvocationId`, `HarnessInvocationRequest`, `HarnessTerminal`, `HarnessFailure`, `HarnessPlugin`, `HarnessApprovalBroker`, `HarnessRegistry`, and every store method use the exact names/signatures established in the Frozen Interfaces ledger and consumed unchanged by later tasks.
- **Checkout grounding:** Existing paths/symbols were verified at `ca09f32`: `src/engine/harness.rs` owns `HarnessCommandExecutor`, `DirectLoopHarnessExecutor`, and `AgentCommandDispatcher`; `src/main.rs` constructs it; `src/store/agent.rs` owns Slice 2 command recovery; `migrations/0013_agent_lifecycle.sql` is the latest migration; `agent-client-protocol = 2.0.0`; and the Slice 2 handoff names the frozen consumed types.
- **External contract grounding:** DeepSeek Harness is pinned to official source commit `141eb6fef83422698aef7a981029e843e8161534`; its automation server is JSON-RPC stdio, creates fresh sessions, allows one in-flight prompt per session, returns committed output, supports cancellation and one-shot permission, and does not advertise the rich editor/session/MCP surface. Codex ACP is pinned to official source commit `296069e841634cd4bb9bc4515602d836e49231ec`; richer capabilities remain optional.
- **Task sizing:** Each of eight tasks creates an independently testable and reviewable deliverable; cumulative fresh-review gates block after Tasks 1-4 and Tasks 1-8.
