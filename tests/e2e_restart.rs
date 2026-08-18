use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::Arc;

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    AcceptedUserTask, AgentSelector, CatalogTool, ClosedSchema, CreateSession, EffectKind,
    GoalContract, HarnessRole, IdempotencyKey, PostgresStore, PrincipalId, PublishedDefinition,
    RecoverySupport, SessionId, SessionView, StartRun, StoreError, SubmitUserTask,
    SuccessCriterion, TaskPreferences, TaskTarget, ToolCatalogSnapshot,
};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
mod support;
use support::ResponsesServer;
use time::{Duration, OffsetDateTime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use uuid::Uuid;

struct HarnessChild {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

struct IsolatedDatabase {
    name: String,
    admin_url: String,
    url: String,
}

struct RestartFixture {
    store: PostgresStore,
    verification_pool: PgPool,
    database_url: String,
    session: SessionView,
    principal: PrincipalId,
    agent: PublishedDefinition,
}

impl RestartFixture {
    async fn new() -> Option<Self> {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return None;
        };
        let store = PostgresStore::connect(&database_url).await.unwrap();
        let verification_pool = PgPool::connect(&database_url).await.unwrap();
        let principal = PrincipalId::new(format!("restart-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/restart-agent-foundation".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let catalog = ToolCatalogSnapshot::new(
            session.id.clone(),
            Uuid::now_v7(),
            [CatalogTool::new(
                "github/context",
                "github",
                "context",
                ClosedSchema::object("input", Default::default()),
                ClosedSchema::object("output", Default::default()),
                BTreeSet::new(),
                EffectKind::ReadOnly,
                RecoverySupport::NotRequired,
            )
            .unwrap()],
        )
        .unwrap();
        store.append_catalog_epoch(&catalog).await.unwrap();
        let name = format!("babysit_pr_{}", Uuid::now_v7().simple());
        let source = include_str!("fixtures/dsl/agent_babysit_pr.kite").replacen(
            "agent babysit_pr",
            &format!("agent {name}"),
            1,
        );
        let compiled = compile(&source, &catalog, &Limits::default()).unwrap();
        let agent = store
            .publish(session.id.clone(), &compiled, &source)
            .await
            .unwrap();
        Some(Self {
            store,
            verification_pool,
            database_url,
            session,
            principal,
            agent,
        })
    }

    async fn submit_canonical_task(&self) -> AcceptedUserTask {
        self.store
            .submit_user_task(
                &self.principal,
                submit_request(
                    self.session.id.clone(),
                    self.agent.name.clone(),
                    Some(self.agent.version),
                ),
            )
            .await
            .unwrap()
    }
}

fn submit_request(
    session_id: SessionId,
    agent_name: String,
    version: Option<u32>,
) -> SubmitUserTask {
    SubmitUserTask {
        session_id,
        agent: AgentSelector {
            name: agent_name,
            version,
        },
        target: TaskTarget::GithubPullRequest {
            repository: "acme/widgets".to_owned(),
            number: 142,
        },
        preferences: TaskPreferences {
            harness_role: Some(HarnessRole::Reasoning),
        },
        goal: GoalContract {
            version: 1,
            objective: "Keep the pull request mergeable".to_owned(),
            success: vec![SuccessCriterion {
                id: "reviews_resolved".to_owned(),
                statement: "All actionable reviews are resolved".to_owned(),
                evidence: "github.review_thread".to_owned(),
            }],
            constraints: vec!["Do not rewrite published history".to_owned()],
            blocked_when: vec!["Required credentials are unavailable".to_owned()],
        },
        idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7())).unwrap(),
    }
}

async fn task_and_goal_counts(pool: &PgPool, session_id: &SessionId) -> (i64, i64) {
    let tasks = sqlx::query_scalar("SELECT count(*) FROM user_tasks WHERE session_id = $1")
        .bind(session_id.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap();
    let goals = sqlx::query_scalar(
        "SELECT count(*) FROM goal_contracts g \
         JOIN user_tasks t ON t.id = g.task_id WHERE t.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    (tasks, goals)
}

async fn execution_counts(pool: &PgPool, session_id: &SessionId) -> [i64; 6] {
    let runs = sqlx::query_scalar("SELECT count(*) FROM runs WHERE session_id = $1")
        .bind(session_id.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap();
    let frames = sqlx::query_scalar(
        "SELECT count(*) FROM frames f JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    let events = sqlx::query_scalar(
        "SELECT count(*) FROM events e JOIN runs r ON r.id = e.run_id WHERE r.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    let model_items = sqlx::query_scalar(
        "SELECT count(*) FROM model_items m JOIN frames f ON f.id = m.frame_id \
         JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    let tool_calls = sqlx::query_scalar(
        "SELECT count(*) FROM tool_calls t JOIN frames f ON f.id = t.frame_id \
         JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    let permission_requests = sqlx::query_scalar(
        "SELECT count(*) FROM permission_requests p JOIN runs r ON r.id = p.run_id \
         WHERE r.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    [
        runs,
        frames,
        events,
        model_items,
        tool_calls,
        permission_requests,
    ]
}

impl IsolatedDatabase {
    async fn create(base_url: &str) -> Self {
        let mut admin = url::Url::parse(base_url).expect("database URL");
        admin.set_path("/postgres");
        let name = format!("kiteframe_e2e_{}", Uuid::now_v7().simple());
        let admin_url = admin.to_string();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("connect database administrator");
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&pool)
            .await
            .expect("create isolated acceptance database");
        drop(pool);
        let mut database = admin;
        database.set_path(&format!("/{name}"));
        Self {
            name,
            admin_url,
            url: database.to_string(),
        }
    }

    async fn drop(self) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .expect("connect database administrator for cleanup");
        sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
            .execute(&pool)
            .await
            .expect("drop isolated acceptance database");
    }
}

impl HarnessChild {
    async fn start(database_url: &str, principal: &str, responses_base_url: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kiteframe-harness"))
            .env("KITEFRAME_DATABASE_URL", database_url)
            .env("KITEFRAME_PRINCIPAL", principal)
            .env("KITEFRAME_RESPONSES_BASE_URL", responses_base_url)
            .env("KITEFRAME_RESPONSES_API_KEY", "process-restart-test")
            .env("KITEFRAME_RESPONSES_MODEL", "test-model")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn harness child");
        Self {
            stdin: child.stdin.take().expect("child stdin"),
            stdout: BufReader::new(child.stdout.take().expect("child stdout")).lines(),
            child,
        }
    }

    async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let request = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
        self.stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write child request");
        self.stdin.flush().await.expect("flush child request");
        let response = loop {
            let line =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.stdout.next_line())
                    .await
                    .expect("child response timeout")
                    .expect("read child response")
                    .expect("child response EOF");
            let response: serde_json::Value =
                serde_json::from_str(&line).expect("child JSON-RPC response");
            if response["method"] == "session/request_permission" {
                let approval = json!({
                    "jsonrpc":"2.0",
                    "id":response["id"],
                    "result":{"outcome":{"outcome":"selected", "optionId":"allow-once"}}
                });
                self.stdin
                    .write_all(format!("{approval}\n").as_bytes())
                    .await
                    .expect("approve child permission");
                self.stdin
                    .flush()
                    .await
                    .expect("flush child permission approval");
                continue;
            }
            break response;
        };
        assert_eq!(response["id"], id, "unexpected child response: {response}");
        assert!(response.get("error").is_none(), "child error: {response}");
        response["result"].clone()
    }

    async fn close(mut self) {
        self.stdin.shutdown().await.expect("close child stdin");
        drop(self.stdin);
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), self.child.wait())
            .await
            .expect("child shutdown timeout")
            .expect("wait for child");
        assert!(
            status.success(),
            "harness child exited unsuccessfully: {status}"
        );
    }
}

#[tokio::test]
async fn stdio_harness_child_restarts_and_reloads_an_authenticated_session() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let database = IsolatedDatabase::create(&database_url).await;
    let principal = format!("process-restart-{}", Uuid::now_v7());
    let cwd = "/tmp/kiteframe-process-restart";
    let mut first = HarnessChild::start(&database.url, &principal, "http://127.0.0.1:9/v1").await;
    first
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1, "clientCapabilities":{}, "clientInfo":{"name":"e2e","version":"1"}}),
        )
        .await;
    let created = first
        .request(2, "session/new", json!({"cwd":cwd, "mcpServers":[]}))
        .await;
    let session_id = created["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    first.close().await;

    let mut restarted =
        HarnessChild::start(&database.url, &principal, "http://127.0.0.1:9/v1").await;
    restarted
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1, "clientCapabilities":{}, "clientInfo":{"name":"e2e","version":"1"}}),
        )
        .await;
    assert_eq!(
        restarted
            .request(
                2,
                "session/load",
                json!({"sessionId":session_id, "cwd":cwd, "mcpServers":[]})
            )
            .await,
        json!({})
    );
    restarted.close().await;
    database.drop().await;
}

#[tokio::test]
async fn stdio_harness_child_completes_a_real_model_workflow_and_replays_events() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let database = IsolatedDatabase::create(&database_url).await;
    let responses = ResponsesServer::start(json!({
        "output": [{
            "type":"function_call",
            "call_id":"complete-1",
            "name":"complete_task",
            "arguments":"{\"outcome\":\"ok\",\"fields\":{}}"
        }]
    }))
    .await;
    let principal = format!("process-model-{}", Uuid::now_v7());
    let cwd = "/tmp/kiteframe-process-model";
    let mut child = HarnessChild::start(&database.url, &principal, responses.base_url()).await;
    child
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1, "clientCapabilities":{}, "clientInfo":{"name":"e2e","version":"1"}}),
        )
        .await;
    let session = child
        .request(2, "session/new", json!({"cwd":cwd, "mcpServers":[]}))
        .await;
    let session_id = session["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let workflow_name = format!("processmodel{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {workflow_name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"done\" }} }}"
    );
    let published = child
        .request(3, "_kiteframe/workflow/publish", json!({"source":source}))
        .await;
    let run = child
        .request(
            4,
            "_kiteframe/run/start",
            json!({
                "sessionId":session_id,
                "workflowName":workflow_name,
                "version":published["version"],
                "input":{},
                "idempotencyKey":format!("process-model-run-{}", Uuid::now_v7())
            }),
        )
        .await;
    let run_id = run["id"].clone();
    let mut completed = None;
    for request_id in 5..=40 {
        let state = child
            .request(request_id, "_kiteframe/run/get", json!({"runId":run_id}))
            .await;
        if state["state"] == "completed" {
            completed = Some(state);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        completed.expect("model workflow completed")["state"],
        "completed"
    );
    let replay = child
        .request(
            41,
            "_kiteframe/run/event",
            json!({"runId":run_id, "after":0, "limit":32}),
        )
        .await;
    let kinds = replay["events"]
        .as_array()
        .expect("replayed events")
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "run.started",
            "model.intent",
            "model.committed",
            "model.completed_task",
            "frame.completed"
        ]
    );
    let request = responses.one_request().await;
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/responses");
    assert_eq!(request.body["model"], "test-model");
    child.close().await;
    database.drop().await;
}

#[tokio::test]
async fn stdio_harness_child_discovers_and_invokes_a_real_mcp_tool() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let database = IsolatedDatabase::create(&database_url).await;
    let responses = ResponsesServer::start_sequence(vec![
        json!({"output":[{"type":"function_call","call_id":"tool-1","name":"docs/search","arguments":"{}"}]}),
        json!({"output":[{"type":"function_call","call_id":"complete-1","name":"complete_task","arguments":"{\"outcome\":\"ok\",\"fields\":{}}"}]}),
    ])
    .await;
    let log_path = std::env::temp_dir().join(format!("kiteframe-e2e-mcp-{}.log", Uuid::now_v7()));
    let fixture = format!("{}/tests/fixtures/mcp_stdio.py", env!("CARGO_MANIFEST_DIR"));
    let principal = format!("process-mcp-{}", Uuid::now_v7());
    let mut child = HarnessChild::start(&database.url, &principal, responses.base_url()).await;
    child
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1, "clientCapabilities":{}, "clientInfo":{"name":"e2e","version":"1"}}),
        )
        .await;
    let session = child
        .request(
            2,
            "session/new",
            json!({
                "cwd":"/tmp/kiteframe-process-mcp",
                "mcpServers":[{
                    "type":"stdio",
                    "name":"docs",
                    "command":"python3",
                    "args":[fixture],
                    "env":[{"name":"KITEFRAME_E2E_MCP_LOG", "value":log_path}]
                }]
            }),
        )
        .await;
    let session_id = session["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let workflow_name = format!("processmcp{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {workflow_name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok() {{ done \"done\" }} }}"
    );
    let published = child
        .request(3, "_kiteframe/workflow/publish", json!({"source":source}))
        .await;
    let run = child
        .request(
            4,
            "_kiteframe/run/start",
            json!({
                "sessionId":session_id,
                "workflowName":workflow_name,
                "version":published["version"],
                "input":{},
                "idempotencyKey":format!("process-mcp-run-{}", Uuid::now_v7())
            }),
        )
        .await;
    let run_id = run["id"].clone();
    let mut completed = false;
    for request_id in 5..=40 {
        let state = child
            .request(request_id, "_kiteframe/run/get", json!({"runId":run_id}))
            .await;
        if state["state"] == "completed" {
            completed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(completed, "MCP workflow completed");
    let invocations = std::fs::read_to_string(&log_path).expect("MCP invocation log");
    assert_eq!(invocations.lines().collect::<Vec<_>>(), ["tools/call"]);
    child.close().await;
    let mut restarted = HarnessChild::start(&database.url, &principal, responses.base_url()).await;
    restarted
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1, "clientCapabilities":{}, "clientInfo":{"name":"e2e","version":"1"}}),
        )
        .await;
    assert_eq!(
        restarted
            .request(
                2,
                "session/load",
                json!({
                    "sessionId":session_id,
                    "cwd":"/tmp/kiteframe-process-mcp",
                    "mcpServers":[{
                        "type":"stdio",
                        "name":"docs",
                        "command":"python3",
                        "args":[fixture],
                        "env":[{"name":"KITEFRAME_E2E_MCP_LOG", "value":log_path}]
                    }]
                }),
            )
            .await,
        json!({})
    );
    restarted.close().await;
    std::fs::remove_file(&log_path).expect("remove MCP log");
    database.drop().await;
}

#[tokio::test]
async fn restart_reconstructs_principal_scoped_run_and_cursor_replay() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let first = Arc::new(
        PostgresStore::connect(&database_url)
            .await
            .expect("first store"),
    );
    let principal = PrincipalId::new(format!("restart-a-{}", Uuid::now_v7())).unwrap();
    let session = first
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/restart-a".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let other_principal = PrincipalId::new(format!("restart-b-{}", Uuid::now_v7())).unwrap();
    let other_session = first
        .create_session(CreateSession {
            principal: other_principal.clone(),
            working_directory: "/restart-b".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("other session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [tool("docs/search")])
            .expect("catalog");
    first
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let source = format!(
        "workflow restart{}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok() {{ done \"ok\" }} }}",
        Uuid::now_v7().simple()
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("compile");
    let definition = first
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let run = first
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("restart-run-{}", Uuid::now_v7()))
                .unwrap(),
        })
        .await
        .expect("start");
    drop(first);

    let restarted = Arc::new(
        PostgresStore::connect(&database_url)
            .await
            .expect("restarted store"),
    );
    assert_eq!(
        restarted
            .resume_session(session.id.clone(), &principal)
            .await
            .expect("authenticated resume")
            .id,
        session.id
    );
    assert!(
        restarted
            .resume_session(session.id.clone(), &other_principal)
            .await
            .is_err()
    );
    assert!(
        restarted
            .get_run(other_session.id, run.id.clone())
            .await
            .is_err()
    );
    let restored_run = restarted
        .get_run(session.id.clone(), run.id.clone())
        .await
        .expect("durable run");
    assert_eq!(restored_run.root_frame_id, run.root_frame_id);
    assert_eq!(
        restored_run.definition.kind,
        kiteframe::dsl::DefinitionKind::Workflow,
    );
    let events = restarted
        .events_after(session.id.clone(), run.id, 0, 8)
        .await
        .expect("cursor replay");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "run.started");
}

#[tokio::test]
async fn accepted_task_and_goal_survive_restart_without_becoming_runnable() {
    let Some(fixture) = RestartFixture::new().await else {
        return;
    };
    let accepted = fixture.submit_canonical_task().await;
    let database_url = fixture.database_url.clone();
    let verification_pool = fixture.verification_pool.clone();
    drop(fixture.store);

    let reopened = PostgresStore::connect(&database_url).await.unwrap();
    let restored = reopened
        .get_user_task(accepted.session_id.clone(), accepted.id.clone())
        .await
        .unwrap();

    assert_eq!(restored, accepted);
    assert_eq!(restored.id, accepted.id);
    assert_eq!(restored.session_id, accepted.session_id);
    assert_eq!(restored.principal, accepted.principal);
    assert_eq!(restored.definition, accepted.definition);
    assert_eq!(restored.target, accepted.target);
    assert_eq!(restored.preferences, accepted.preferences);
    assert_eq!(restored.goal, accepted.goal);
    assert_eq!(restored.goal_digest, accepted.goal.digest().unwrap());
    assert_eq!(restored.revision, 0);
    assert_eq!(restored.created_at, accepted.created_at);
    assert_eq!(
        execution_counts(&verification_pool, &accepted.session_id).await,
        [0; 6],
    );
}

#[tokio::test]
async fn task_reads_are_session_and_principal_scoped_after_restart() {
    let Some(fixture) = RestartFixture::new().await else {
        return;
    };
    let accepted = fixture.submit_canonical_task().await;
    let same_principal_session = fixture
        .store
        .create_session(CreateSession {
            principal: fixture.principal.clone(),
            working_directory: "/restart-same-principal".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let other_principal = PrincipalId::new(format!("other-{}", Uuid::now_v7())).unwrap();
    let other_session = fixture
        .store
        .create_session(CreateSession {
            principal: other_principal,
            working_directory: "/restart-other-principal".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let database_url = fixture.database_url.clone();
    drop(fixture.store);
    let reopened = PostgresStore::connect(&database_url).await.unwrap();

    for session_id in [same_principal_session.id, other_session.id] {
        assert!(matches!(
            reopened
                .get_user_task(session_id, accepted.id.clone())
                .await,
            Err(StoreError::UserTaskNotFound),
        ));
    }
}

#[tokio::test]
async fn rejected_submissions_leave_no_partial_task_or_goal() {
    let Some(fixture) = RestartFixture::new().await else {
        return;
    };

    let mut invalid_goal =
        submit_request(fixture.session.id.clone(), fixture.agent.name.clone(), None);
    invalid_goal.goal.success.clear();
    assert!(matches!(
        fixture
            .store
            .submit_user_task(&fixture.principal, invalid_goal)
            .await,
        Err(StoreError::InvalidUserTask(_)),
    ));

    let missing = submit_request(fixture.session.id.clone(), "missing_agent".to_owned(), None);
    assert!(matches!(
        fixture
            .store
            .submit_user_task(&fixture.principal, missing)
            .await,
        Err(StoreError::DefinitionNotFound),
    ));

    let workflow_name = format!("restart_workflow_{}", Uuid::now_v7().simple());
    let workflow_source = format!(
        "workflow {workflow_name}() -> string {{ start t() }} \
         task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"ok\" }} }}"
    );
    let workflow = compile(
        &workflow_source,
        &ToolCatalogSnapshot::new(fixture.session.id.clone(), Uuid::now_v7(), []).unwrap(),
        &Limits::default(),
    )
    .unwrap();
    let workflow = fixture
        .store
        .publish(fixture.session.id.clone(), &workflow, &workflow_source)
        .await
        .unwrap();
    let workflow_request = submit_request(
        fixture.session.id.clone(),
        workflow.name,
        Some(workflow.version),
    );
    assert!(matches!(
        fixture
            .store
            .submit_user_task(&fixture.principal, workflow_request)
            .await,
        Err(StoreError::AgentDefinitionRequired),
    ));

    let wrong_principal = PrincipalId::new("wrong-principal").unwrap();
    let wrong_principal_request =
        submit_request(fixture.session.id.clone(), fixture.agent.name.clone(), None);
    assert!(matches!(
        fixture
            .store
            .submit_user_task(&wrong_principal, wrong_principal_request)
            .await,
        Err(StoreError::PrincipalMismatch),
    ));

    fixture
        .store
        .close_session(fixture.session.id.clone(), &fixture.principal)
        .await
        .unwrap();
    let closed = submit_request(fixture.session.id.clone(), fixture.agent.name.clone(), None);
    assert!(matches!(
        fixture
            .store
            .submit_user_task(&fixture.principal, closed)
            .await,
        Err(StoreError::SessionClosed),
    ));

    assert_eq!(
        task_and_goal_counts(&fixture.verification_pool, &fixture.session.id).await,
        (0, 0),
    );
    assert_eq!(
        execution_counts(&fixture.verification_pool, &fixture.session.id).await,
        [0; 6],
    );
}

#[tokio::test]
async fn conflicting_replay_preserves_the_original_task_and_goal_only() {
    let Some(fixture) = RestartFixture::new().await else {
        return;
    };
    let request = submit_request(
        fixture.session.id.clone(),
        fixture.agent.name.clone(),
        Some(fixture.agent.version),
    );
    let accepted = fixture
        .store
        .submit_user_task(&fixture.principal, request.clone())
        .await
        .unwrap();
    let mut conflict = request;
    conflict.goal.objective = "A conflicting objective".to_owned();

    assert!(matches!(
        fixture
            .store
            .submit_user_task(&fixture.principal, conflict)
            .await,
        Err(StoreError::IdempotencyConflict),
    ));
    assert_eq!(
        fixture
            .store
            .get_user_task(fixture.session.id.clone(), accepted.id.clone())
            .await
            .unwrap(),
        accepted,
    );
    assert_eq!(
        task_and_goal_counts(&fixture.verification_pool, &fixture.session.id).await,
        (1, 1),
    );
    assert_eq!(
        execution_counts(&fixture.verification_pool, &fixture.session.id).await,
        [0; 6],
    );
}

fn tool(name: &str) -> CatalogTool {
    CatalogTool::new(
        name,
        "docs",
        "search",
        ClosedSchema::object("input", Default::default()),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("tool")
}
