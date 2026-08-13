use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::Arc;

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, IdempotencyKey, PostgresStore,
    PrincipalId, RecoverySupport, StartRun, ToolCatalogSnapshot,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
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
    async fn start(database_url: &str, principal: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kiteframe-harness"))
            .env("KITEFRAME_DATABASE_URL", database_url)
            .env("KITEFRAME_PRINCIPAL", principal)
            .env("KITEFRAME_RESPONSES_BASE_URL", "http://127.0.0.1:9/v1")
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
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), self.stdout.next_line())
            .await
            .expect("child response timeout")
            .expect("read child response")
            .expect("child response EOF");
        let response: serde_json::Value =
            serde_json::from_str(&line).expect("child JSON-RPC response");
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
    let mut first = HarnessChild::start(&database.url, &principal).await;
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

    let mut restarted = HarnessChild::start(&database.url, &principal).await;
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
    assert_eq!(
        restarted
            .get_run(session.id.clone(), run.id.clone())
            .await
            .expect("durable run")
            .root_frame_id,
        run.root_frame_id
    );
    let events = restarted
        .events_after(session.id.clone(), run.id, 0, 8)
        .await
        .expect("cursor replay");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "run.started");
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
