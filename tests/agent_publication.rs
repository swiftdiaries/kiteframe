use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kiteframe::dsl::DefinitionKind;
use kiteframe::{
    Application, ApplicationCommand, ApplicationContext, ApplicationResult, CreateSession, Engine,
    EngineConfig, InProcessToolRegistry, ModelClient, ModelError, ModelRequest, ModelResponse,
    PersistedAuthorizer, PostgresStore, PrincipalId, ProtocolError, PublishDefinition,
};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tracing::instrument::WithSubscriber;
use uuid::Uuid;

struct UnusedModel;

#[async_trait]
impl ModelClient for UnusedModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Protocol(
            "model is not used by publication tests".to_owned(),
        ))
    }
}

#[derive(Clone)]
struct CaptureLogs(Arc<Mutex<Vec<u8>>>);

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CaptureLogs {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

impl Write for CaptureLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log capture poisoned").extend(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn application_fixture() -> Option<(Application, ApplicationContext, PgPool)> {
    let database_url = std::env::var("DATABASE_URL").ok()?;
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let engine = Arc::new(Engine {
        store: store.clone(),
        model: Arc::new(UnusedModel),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
        config: EngineConfig::default(),
    });
    let principal = PrincipalId::new(format!("agent-publication-{}", Uuid::now_v7())).unwrap();
    let session = store
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/agent-publication".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let context = ApplicationContext {
        connection_principal: principal,
        session_id: Some(session.id),
    };
    let pool = store.pool().clone();
    Some((Application::new(store, engine), context, pool))
}

fn agent_source(name: &str, prompt: &str) -> String {
    format!(
        r#"agent {name}(repository: string) -> string {{
  entry inspect(repository = repository)
  events ["github.pull_request.changed.v1"]
  limits {{ steps: 20 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task inspect(repository: string) -> string {{
    model reasoning
    prompt """{prompt}"""
    tools []
    on done(summary: string) {{ done summary }}
  }}
}}"#
    )
}

fn workflow_source(name: &str, prompt: &str) -> String {
    format!(
        "workflow {name}() -> string {{ start triage() }} \
         task triage() -> string {{ prompt \"\"\"{prompt}\"\"\" tools [] \
         on done(answer: string) {{ done answer }} }}"
    )
}

#[tokio::test]
async fn publication_persists_agent_as_an_immutable_tagged_definition() {
    let Some((application, bound_context, pool)) = application_fixture().await else {
        eprintln!("skipping agent publication test: DATABASE_URL is not configured");
        return;
    };
    let name = format!("babysit_{}", Uuid::now_v7().simple());
    let source = agent_source(&name, "Inspect the pull request.");
    let changed = agent_source(&name, "Inspect the pull request and summarize risk.");

    let publish = |source: String| {
        application.dispatch(
            &bound_context,
            ApplicationCommand::Publish(PublishDefinition {
                expected_kind: None,
                source,
            }),
        )
    };
    let ApplicationResult::Published(first) = publish(source.clone())
        .await
        .expect("foundation slice publishes compiled agents")
    else {
        panic!("expected published definition");
    };
    let ApplicationResult::Published(replay) = publish(source.clone())
        .await
        .expect("identical source replays")
    else {
        panic!("expected replayed definition");
    };
    let ApplicationResult::Published(second) = publish(changed.clone())
        .await
        .expect("changed source advances the version")
    else {
        panic!("expected changed definition");
    };

    assert_eq!(first.kind, DefinitionKind::Agent);
    assert_eq!(replay, first);
    assert_eq!(second.kind, DefinitionKind::Agent);
    assert_eq!(second.version, first.version + 1);

    let rows: Vec<(i32, String, String)> = sqlx::query_as(
        "SELECT version, source, definition_kind FROM workflow_definitions \
         WHERE name = $1 ORDER BY version",
    )
    .bind(&name)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (first.version as i32, source, "agent".to_owned()),
            (second.version as i32, changed, "agent".to_owned()),
        ]
    );
}

#[tokio::test]
async fn publication_kind_mismatch_fails_before_persistence() {
    let Some((application, bound_context, pool)) = application_fixture().await else {
        eprintln!("skipping agent publication test: DATABASE_URL is not configured");
        return;
    };
    let name = format!("mismatch_{}", Uuid::now_v7().simple());
    let error = application
        .dispatch(
            &bound_context,
            ApplicationCommand::Publish(PublishDefinition {
                expected_kind: Some(DefinitionKind::Workflow),
                source: agent_source(&name, "Must not persist."),
            }),
        )
        .await
        .expect_err("legacy method must reject agent source");
    assert!(matches!(
        error,
        ProtocolError::StableApplication {
            code: "definition_kind_mismatch",
            ..
        }
    ));
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflow_definitions WHERE name = $1")
            .bind(name)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn deprecated_workflow_warning_contains_metadata_but_not_source() {
    let Some((application, bound_context, _pool)) = application_fixture().await else {
        eprintln!("skipping agent publication test: DATABASE_URL is not configured");
        return;
    };
    let name = format!("legacy_{}", Uuid::now_v7().simple());
    let sentinel = "publication-source-must-not-be-logged";
    let captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::WARN)
        .with_writer(CaptureLogs(captured.clone()))
        .finish();

    application
        .dispatch(
            &bound_context,
            ApplicationCommand::Publish(PublishDefinition {
                expected_kind: Some(DefinitionKind::Workflow),
                source: workflow_source(&name, sentinel),
            }),
        )
        .with_subscriber(subscriber)
        .await
        .expect("legacy workflow publishes");

    let logs = String::from_utf8(captured.lock().expect("log capture poisoned").clone()).unwrap();
    assert!(logs.contains("deprecated definition syntax"));
    assert!(logs.contains("deprecated_workflow"));
    assert!(logs.contains(&name));
    assert!(logs.contains("span_start"));
    assert!(logs.contains("span_end"));
    assert!(!logs.contains(sentinel));
}
