use serde::Deserialize;
use serde_json::{Value, json};

use kiteframe::{
    ApplicationCommand, ApplicationResult, ApplicationSession, AuthenticatedConnection,
    IdempotencyKey, PermissionChoice, PermissionRequestId, PermissionResponseContext, PrincipalId,
    ProtocolAdapter, ProtocolConnection, ProtocolError, ProtocolNegotiation, RunId, SessionId,
    SessionView, TransportKind, V1Adapter,
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V1Fixture {
    initialize: Value,
    session_new: Value,
    session_load: Value,
    session_new_response: Value,
    session_load_response: Value,
    published_response: Value,
    permission: Value,
    permission_response: Value,
    extensions: Vec<ExtensionFixture>,
}

#[derive(Deserialize)]
struct ExtensionFixture {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Value,
    kind: String,
    command: Value,
}

fn v1_fixture() -> V1Fixture {
    serde_json::from_str(include_str!("fixtures/acp/v1.json")).expect("valid v1 fixture")
}

fn message_params(message: Value, expected_method: &str) -> Value {
    assert_eq!(message["jsonrpc"], "2.0");
    assert_eq!(message["method"], expected_method);
    assert!(message["id"].is_number());
    message["params"].clone()
}

#[test]
fn v1_golden_messages_use_sdk_types_and_only_v1_lifecycle() {
    let fixture = v1_fixture();
    let adapter = V1Adapter;

    let response = adapter
        .initialize(message_params(fixture.initialize, "initialize"))
        .expect("initialize v1");
    assert_eq!(response["protocolVersion"], 1);
    assert_eq!(response["agentCapabilities"]["loadSession"], true);
    assert_eq!(response["_meta"]["future.trace"]["id"], "trace-v1");

    assert!(matches!(
        adapter.decode_session(
            "session/new",
            message_params(fixture.session_new, "session/new")
        ),
        Ok(ApplicationCommand::NewSession(_))
    ));
    assert!(matches!(
        adapter.decode_session(
            "session/load",
            message_params(fixture.session_load, "session/load")
        ),
        Ok(ApplicationCommand::ResumeSession(_))
    ));
    assert!(matches!(
        adapter.decode_session("session/list", json!({})),
        Err(ProtocolError::UnsupportedMethod(_))
    ));
    assert!(matches!(
        adapter.decode_session("session/close", json!({})),
        Err(ProtocolError::UnsupportedMethod(_))
    ));

    let permission = adapter
        .decode_permission(message_params(
            fixture.permission,
            "session/request_permission",
        ))
        .expect("v1 tool-call permission");
    let permission = serde_json::to_value(permission).expect("serialize permission");
    assert_eq!(permission["toolCall"]["toolCallId"], "call-1");
    assert_eq!(permission["_meta"]["future.permission"][1], 1);
}

#[test]
fn v1_session_mcp_configuration_stays_connection_local() {
    let command = V1Adapter
        .decode_session(
            "session/new",
            json!({
                "cwd": "/tmp/kiteframe-mcp",
                "mcpServers": [{
                    "type": "http",
                    "name": "docs",
                    "url": "http://127.0.0.1:3010/mcp",
                    "headers": [{"name": "Authorization", "value": "Bearer ephemeral"}]
                }]
            }),
        )
        .expect("standard ACP MCP declaration is accepted");
    let ApplicationCommand::NewSession(session) = command else {
        panic!("expected session command");
    };
    assert_eq!(session.mcp_servers.len(), 1);
    assert!(matches!(
        &session.mcp_servers[0],
        kiteframe::McpServerConfig::StreamableHttp { alias, headers, .. }
            if alias == "docs" && headers.len() == 1
    ));
}

fn fixture_application_session(cwd: &str) -> ApplicationSession {
    ApplicationSession {
        view: SessionView {
            id: SessionId::new(Uuid::parse_str("019ff3d8-9d86-7de0-9070-0f0185d5f413").unwrap()),
            principal: PrincipalId::new("fixture-principal").unwrap(),
            policy_revision: kiteframe::PolicyRevision::new(0),
            current_catalog_epoch: None,
            mcp_attached: false,
        },
        working_directory: cwd.to_owned(),
    }
}

fn permission_context() -> PermissionResponseContext {
    PermissionResponseContext::new(
        SessionId::new(Uuid::parse_str("019ff3d8-9d86-7de0-9070-0f0185d5f413").unwrap()),
        RunId::new(Uuid::parse_str("019ff3d8-9d86-7de0-9070-0f0185d5f414").unwrap()),
        PermissionRequestId::new(Uuid::parse_str("019ff3d8-9d86-7de0-9070-0f0185d5f415").unwrap()),
        IdempotencyKey::new("permission-response-1").unwrap(),
    )
    .with_option("allow-once", PermissionChoice::AllowOnce)
}

#[test]
fn v1_standard_responses_use_sdk_dtos_and_permission_reaches_core() {
    let fixture = v1_fixture();
    let adapter = V1Adapter;
    assert_eq!(
        adapter
            .encode_session_result(
                "session/new",
                ApplicationResult::Session(fixture_application_session("/tmp/kiteframe-v1")),
            )
            .unwrap(),
        fixture.session_new_response
    );
    assert_eq!(
        adapter
            .encode_session_result(
                "session/load",
                ApplicationResult::Session(fixture_application_session("/tmp/kiteframe-v1")),
            )
            .unwrap(),
        fixture.session_load_response
    );

    let command = adapter
        .decode_permission_response(fixture.permission_response, permission_context())
        .expect("translate standard ACP permission response");
    let ApplicationCommand::ResolvePermission(request) = command else {
        panic!("expected internal permission resolution command");
    };
    assert_eq!(request.session_id, permission_context().session_id);
    assert_eq!(request.run_id, permission_context().run_id);
    assert_eq!(
        request.permission_request_id,
        permission_context().permission_request_id
    );
    assert!(matches!(request.choice, PermissionChoice::AllowOnce));
    assert_eq!(request.idempotency_key.as_str(), "permission-response-1");

    assert_eq!(
        adapter
            .encode_result(ApplicationResult::Published(
                kiteframe::PublishedDefinition {
                    id: Uuid::parse_str("019ff3d8-9d86-7de0-9070-0f0185d5f416").unwrap(),
                    name: "support".to_owned(),
                    version: 7,
                    definition_hash: "sha256:fixture".to_owned(),
                }
            ))
            .unwrap(),
        fixture.published_response
    );
    assert!(matches!(
        adapter.encode_result(ApplicationResult::Session(fixture_application_session(
            "/tmp/kiteframe-v1"
        ))),
        Err(ProtocolError::UnexpectedResult(_))
    ));
}

#[test]
fn protocol_negotiation_is_owned_by_the_connection() {
    let connection = ProtocolConnection::new(
        AuthenticatedConnection {
            principal: PrincipalId::new("fixture-principal").unwrap(),
            transport: TransportKind::Stdio,
        },
        false,
    );
    let fixture = v1_fixture();
    let response = connection
        .initialize(message_params(fixture.initialize, "initialize"))
        .expect("connection initialize");
    assert_eq!(response["protocolVersion"], 1);
    assert_eq!(
        connection.negotiated_protocol(),
        Some(kiteframe::NegotiatedProtocol::V1)
    );
    assert!(matches!(
        connection.initialize(json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": { "name": "again", "version": "1" }
        })),
        Err(ProtocolError::AlreadyNegotiated)
    ));
}

#[test]
fn every_advertised_extension_maps_to_one_application_command() {
    let fixture = v1_fixture();
    let adapter = V1Adapter;

    for extension in fixture.extensions {
        assert_eq!(extension.jsonrpc, "2.0");
        assert!(extension.id >= 10);
        let command = adapter
            .decode_extension(&extension.method, extension.params)
            .unwrap_or_else(|error| panic!("{}: {error}", extension.method));
        let actual = command_kind(&command);
        assert_eq!(actual, extension.kind);
        assert_eq!(command_snapshot(&command), extension.command);
    }
}

fn command_kind(command: &ApplicationCommand) -> &'static str {
    match command {
        ApplicationCommand::Publish(_) => "publish",
        ApplicationCommand::Start(_) => "start",
        ApplicationCommand::Get(_) => "get",
        ApplicationCommand::Answer(_) => "answer",
        ApplicationCommand::ResolvePermission(_) => "permission",
        ApplicationCommand::Cancel(_) => "cancel",
        ApplicationCommand::UpdatePolicy(_) => "policy",
        ApplicationCommand::ReplayEvents(_) => "events",
        other => panic!("unexpected lifecycle command: {other:?}"),
    }
}

fn command_snapshot(command: &ApplicationCommand) -> Value {
    match command {
        ApplicationCommand::Publish(request) => json!({"source": request.source}),
        ApplicationCommand::Start(request) => json!({
            "sessionId": request.session_id.as_uuid(),
            "workflowName": request.workflow_name,
            "version": request.version,
            "input": request.input,
            "idempotencyKey": request.idempotency_key.as_str(),
        }),
        ApplicationCommand::Get(request) => json!({"runId": request.run_id.as_uuid()}),
        ApplicationCommand::Answer(request) => json!({
            "sessionId": request.session_id.as_uuid(),
            "runId": request.run_id.as_uuid(),
            "questionId": request.question_id,
            "answer": request.answer,
            "idempotencyKey": request.idempotency_key.as_str(),
        }),
        ApplicationCommand::Cancel(request) => json!({
            "sessionId": request.session_id.as_uuid(),
            "runId": request.run_id.as_uuid(),
            "reason": request.reason,
            "idempotencyKey": request.idempotency_key.as_str(),
        }),
        ApplicationCommand::UpdatePolicy(request) => json!({
            "expectedRevision": request.expected_revision.get(),
            "idempotencyKey": request.idempotency_key.as_str(),
            "policy": request.policy,
        }),
        ApplicationCommand::ReplayEvents(request) => json!({
            "runId": request.run_id.as_uuid(),
            "after": request.after,
            "limit": request.limit,
        }),
        other => panic!("unexpected command snapshot: {other:?}"),
    }
}

#[test]
fn extensions_cannot_override_connection_identity_or_add_semantic_ingress() {
    let adapter = V1Adapter;
    let spoofed = json!({
        "runId": "019ff3d8-9d86-7de0-9070-0f0185d5f414",
        "connectionPrincipal": "mallory"
    });
    assert!(matches!(
        adapter.decode_extension("_kiteframe/run/get", spoofed),
        Err(ProtocolError::InvalidPayload(_))
    ));

    for method in [
        "_kiteframe/run/steer",
        "_kiteframe/run/resolve-permission",
        "session/prompt",
        "session/cancel",
    ] {
        assert!(matches!(
            adapter.decode_extension(method, json!({})),
            Err(ProtocolError::UnsupportedMethod(_))
        ));
    }
}

#[test]
fn one_connection_cannot_switch_protocol_versions() {
    let mut negotiation = ProtocolNegotiation::new(false);
    negotiation.select_v1().expect("select v1");
    assert!(matches!(
        negotiation.select_v1(),
        Err(ProtocolError::AlreadyNegotiated)
    ));
}

#[tokio::test]
async fn dispatcher_owns_durable_session_lifecycle_and_principal_binding() {
    use std::sync::Arc;

    use async_trait::async_trait;
    use kiteframe::{
        Application, ApplicationContext, ApplicationResult, CloseSession, Engine, EngineConfig,
        InProcessToolRegistry, ModelClient, ModelError, ModelRequest, ModelResponse, NewSession,
        PersistedAuthorizer, PostgresStore, PrincipalId, PublishWorkflow, ResumeSession,
    };

    struct UnusedModel;

    #[async_trait]
    impl ModelClient for UnusedModel {
        async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Err(ModelError::Protocol(
                "model is not used by session lifecycle tests".to_owned(),
            ))
        }
    }

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL is required for ACP dispatcher conformance");
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let engine = Arc::new(Engine {
        store: store.clone(),
        model: Arc::new(UnusedModel),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
        config: EngineConfig::default(),
    });
    let application = Application::new(store.clone(), engine.clone());
    let alice = PrincipalId::new(format!("acp-alice-{}", Uuid::now_v7())).expect("principal");
    let alice_context = ApplicationContext {
        connection_principal: alice.clone(),
        session_id: None,
    };

    let (mut client, server) = tokio::io::duplex(4096);
    let (server_reader, server_writer) = tokio::io::split(server);
    let transport_connection = AuthenticatedConnection {
        principal: alice.clone(),
        transport: TransportKind::Stdio,
    };
    let serve = kiteframe::serve_ndjson(
        tokio::io::BufReader::new(server_reader),
        server_writer,
        &application,
        &transport_connection,
    );
    let client_io = async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let messages = [
            "{".to_owned(),
            json!({"jsonrpc":"1.0", "id":2, "method":"initialize", "params":{}}).to_string(),
            json!({"jsonrpc":"1.0", "method":"initialize", "params":{}}).to_string(),
            json!({"jsonrpc":"2.0", "params":{}}).to_string(),
            json!({
                "jsonrpc":"2.0", "id":{"invalid":true}, "method":"initialize", "params":{}
            })
            .to_string(),
            v1_fixture().initialize.to_string(),
            json!({"jsonrpc":"2.0", "method":"_kiteframe/unknown", "params":{}}).to_string(),
            json!({"jsonrpc":"2.0", "id":3, "method":"_kiteframe/run/get", "params":{}})
                .to_string(),
            json!({"jsonrpc":"2.0", "id":4, "method":"_kiteframe/unknown", "params":{}})
                .to_string(),
            json!({
                "jsonrpc":"2.0", "id":5, "method":"_kiteframe/run/get",
                "params":{"runId":"019ff3d8-9d86-7de0-9070-0f0185d5f414"}
            })
            .to_string(),
        ];
        client
            .write_all(format!("{}\n", messages.join("\n")).as_bytes())
            .await
            .expect("write JSON-RPC messages");
        client.shutdown().await.expect("close request stream");
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .await
            .expect("read initialize response");
        response
    };
    let (serve_result, response) = tokio::join!(serve, client_io);
    serve_result.expect("serve standard initialization");
    let responses = response
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON-RPC response"))
        .collect::<Vec<_>>();
    assert_eq!(
        responses.len(),
        9,
        "notifications must not receive responses"
    );
    assert_eq!(
        responses[0],
        json!({
            "jsonrpc":"2.0", "id":null,
            "error":{"code":-32700, "message":"Parse error"}
        })
    );
    assert_eq!(
        responses[1],
        json!({
            "jsonrpc":"2.0", "id":2,
            "error":{"code":-32600, "message":"Invalid Request"}
        })
    );
    for response in &responses[2..=4] {
        assert_eq!(
            response,
            &json!({
                "jsonrpc":"2.0", "id":null,
                "error":{"code":-32600, "message":"Invalid Request"}
            })
        );
    }
    assert_eq!(responses[5]["jsonrpc"], "2.0");
    assert_eq!(responses[5]["id"], 1);
    assert_eq!(responses[5]["result"]["protocolVersion"], 1);
    assert_eq!(responses[6]["id"], 3);
    assert_eq!(responses[6]["error"]["code"], -32602);
    assert_eq!(
        responses[7],
        json!({
            "jsonrpc":"2.0", "id":4,
            "error":{"code":-32601, "message":"Method not found"}
        })
    );
    assert_eq!(responses[8]["id"], 5);
    assert_eq!(responses[8]["error"]["code"], -32000);

    let ApplicationResult::Session(created) = application
        .dispatch(
            &alice_context,
            ApplicationCommand::NewSession(NewSession {
                working_directory: "/tmp/kiteframe-acp".to_owned(),
                mcp_servers: vec![],
            }),
        )
        .await
        .expect("create session")
    else {
        panic!("expected created session");
    };
    assert_eq!(created.view.principal, alice);
    assert_eq!(created.working_directory, "/tmp/kiteframe-acp");

    assert!(matches!(
        application
            .dispatch(
                &alice_context,
                ApplicationCommand::ResumeSession(ResumeSession {
                    session_id: created.view.id.clone(),
                    working_directory: "/tmp/not-the-session-cwd".to_owned(),
                    mcp_servers: vec![],
                }),
            )
            .await,
        Err(ProtocolError::WorkingDirectoryMismatch)
    ));

    let bob_context = ApplicationContext {
        connection_principal: PrincipalId::new(format!("acp-bob-{}", Uuid::now_v7()))
            .expect("principal"),
        session_id: None,
    };
    assert!(matches!(
        application
            .dispatch(
                &bob_context,
                ApplicationCommand::ResumeSession(ResumeSession {
                    session_id: created.view.id.clone(),
                    working_directory: "/tmp/kiteframe-acp".to_owned(),
                    mcp_servers: vec![],
                }),
            )
            .await,
        Err(ProtocolError::Application(_))
    ));

    let ApplicationResult::Sessions(sessions) = application
        .dispatch(&alice_context, ApplicationCommand::ListSessions)
        .await
        .expect("list sessions")
    else {
        panic!("expected session list");
    };
    assert!(
        sessions
            .iter()
            .any(|session| session.view.id == created.view.id)
    );

    let catalog = store
        .current_catalog(created.view.id.clone())
        .await
        .expect("new sessions have an empty principal-scoped catalog");
    assert!(catalog.tools().is_empty());
    let bound_context = ApplicationContext {
        connection_principal: alice.clone(),
        session_id: Some(created.view.id.clone()),
    };
    let policy = kiteframe::SessionPolicy {
        version: 1,
        rules: vec![],
    };
    let policy_key = IdempotencyKey::new(format!("policy-{}", Uuid::now_v7())).unwrap();
    for _ in 0..2 {
        let result = application
            .dispatch(
                &bound_context,
                ApplicationCommand::UpdatePolicy(kiteframe::UpdateSessionPolicy {
                    expected_revision: kiteframe::PolicyRevision::new(0),
                    idempotency_key: policy_key.clone(),
                    policy: policy.clone(),
                }),
            )
            .await
            .expect("policy update or identical replay");
        assert!(matches!(
            result,
            ApplicationResult::PolicyUpdated { revision } if revision.get() == 1
        ));
    }
    assert!(matches!(
        application
            .dispatch(
                &bound_context,
                ApplicationCommand::UpdatePolicy(kiteframe::UpdateSessionPolicy {
                    expected_revision: kiteframe::PolicyRevision::new(0),
                    idempotency_key: IdempotencyKey::new(format!(
                        "stale-policy-{}",
                        Uuid::now_v7()
                    ))
                    .unwrap(),
                    policy,
                }),
            )
            .await,
        Err(ProtocolError::Application(_))
    ));

    let workflow_name = format!("acp{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {workflow_name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok(answer: string) {{ done answer }} }}"
    );
    let ApplicationResult::Published(definition) = application
        .dispatch(
            &bound_context,
            ApplicationCommand::Publish(PublishWorkflow { source }),
        )
        .await
        .expect("publish workflow after the initial policy")
    else {
        panic!("expected published workflow");
    };
    let start_key = IdempotencyKey::new(format!("start-{}", Uuid::now_v7())).unwrap();
    let start = kiteframe::StartRun {
        session_id: created.view.id.clone(),
        workflow_name: definition.name,
        version: Some(definition.version),
        input: json!({}),
        idempotency_key: start_key.clone(),
    };
    let ApplicationResult::Run(run) = application
        .dispatch(&bound_context, ApplicationCommand::Start(start.clone()))
        .await
        .expect("run")
    else {
        panic!("expected run");
    };
    let ApplicationResult::Run(replayed) = application
        .dispatch(&bound_context, ApplicationCommand::Start(start.clone()))
        .await
        .expect("identical start replay")
    else {
        panic!("expected replayed run");
    };
    assert_eq!(replayed, run);
    assert!(matches!(
        application
            .dispatch(
                &bound_context,
                ApplicationCommand::Start(kiteframe::StartRun {
                    input: json!({"different": true}),
                    ..start
                }),
            )
            .await,
        Err(ProtocolError::Application(_))
    ));
    let ApplicationResult::Events(events) = application
        .dispatch(
            &bound_context,
            ApplicationCommand::ReplayEvents(kiteframe::EventCursor {
                run_id: run.id.clone(),
                after: 0,
                limit: 8,
            }),
        )
        .await
        .expect("replay events")
    else {
        panic!("expected events");
    };
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );

    let mut run_blocker = store.pool().begin().await.expect("run blocker transaction");
    sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE")
        .bind(*run.id.as_uuid())
        .execute(&mut *run_blocker)
        .await
        .expect("block run lock");
    let cancel = store.cancel_run(
        run.id.clone(),
        run.revision,
        Some("operator request".to_owned()),
        format!("concurrent-cancel-{}", Uuid::now_v7()),
    );
    let close_after_cancel_starts = async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        application
            .dispatch(
                &alice_context,
                ApplicationCommand::CloseSession(CloseSession {
                    session_id: created.view.id.clone(),
                }),
            )
            .await
    };
    let release_run = async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        run_blocker.commit().await.expect("release run lock");
    };
    let (cancel_result, close_result, ()) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(cancel, close_after_cancel_starts, release_run)
        })
        .await
        .expect("store cancel and session close must not deadlock");
    assert!(cancel_result.is_ok(), "cancel failed: {cancel_result:?}");
    assert!(
        matches!(close_result, Ok(ApplicationResult::Closed)),
        "close failed: {close_result:?}"
    );
    assert_eq!(
        store
            .get_run(created.view.id.clone(), run.id.clone())
            .await
            .expect("closed run")
            .state,
        kiteframe::RunState::Cancelled
    );
    assert!(
        store
            .runnable_frames_for_run(run.id, 8)
            .await
            .expect("runnable frames")
            .is_empty()
    );
    assert!(
        application
            .dispatch(
                &alice_context,
                ApplicationCommand::ResumeSession(ResumeSession {
                    session_id: created.view.id,
                    working_directory: "/tmp/kiteframe-acp".to_owned(),
                    mcp_servers: vec![],
                }),
            )
            .await
            .is_err()
    );

    let expired_application =
        Application::new(store, engine).with_session_retention(time::Duration::seconds(-1));
    let ApplicationResult::Session(expired) = expired_application
        .dispatch(
            &alice_context,
            ApplicationCommand::NewSession(NewSession {
                working_directory: "/tmp/kiteframe-expired".to_owned(),
                mcp_servers: vec![],
            }),
        )
        .await
        .expect("create expired session")
    else {
        panic!("expected expired session");
    };
    assert!(matches!(
        expired_application
            .dispatch(
                &alice_context,
                ApplicationCommand::ResumeSession(ResumeSession {
                    session_id: expired.view.id,
                    working_directory: expired.working_directory,
                    mcp_servers: vec![],
                }),
            )
            .await,
        Err(ProtocolError::SessionExpired)
    ));
}

#[cfg(feature = "acp-v2")]
mod v2 {
    use agent_client_protocol::schema::v2::RequestPermissionSubject;
    use kiteframe::V2Adapter;

    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct V2Fixture {
        initialize: Value,
        session_new: Value,
        session_list: Value,
        session_resume: Value,
        session_close: Value,
        permission: Value,
        session_new_response: Value,
        session_list_response: Value,
        session_resume_response: Value,
        session_close_response: Value,
        permission_response: Value,
    }

    fn fixture() -> V2Fixture {
        serde_json::from_str(include_str!("fixtures/acp/v2.json")).expect("valid v2 fixture")
    }

    #[test]
    fn v2_golden_messages_are_runtime_gated_and_use_v2_sdk_types() {
        let fixture = fixture();
        let initialize = message_params(fixture.initialize, "initialize");
        assert!(matches!(
            V2Adapter::new(false).initialize(initialize.clone()),
            Err(ProtocolError::V2Disabled)
        ));

        let adapter = V2Adapter::new(true);
        let response = adapter.initialize(initialize).expect("initialize v2");
        assert_eq!(response["protocolVersion"], 2);
        assert!(response["capabilities"]["session"].is_object());
        assert_eq!(response["_meta"]["future.trace"]["id"], "trace-v2");

        assert!(matches!(
            adapter.decode_session(
                "session/new",
                message_params(fixture.session_new, "session/new")
            ),
            Ok(ApplicationCommand::NewSession(_))
        ));
        assert!(matches!(
            adapter.decode_session(
                "session/list",
                message_params(fixture.session_list, "session/list")
            ),
            Ok(ApplicationCommand::ListSessions)
        ));
        assert!(matches!(
            adapter.decode_session(
                "session/resume",
                message_params(fixture.session_resume, "session/resume")
            ),
            Ok(ApplicationCommand::ResumeSession(_))
        ));
        assert!(matches!(
            adapter.decode_session(
                "session/close",
                message_params(fixture.session_close, "session/close")
            ),
            Ok(ApplicationCommand::CloseSession(_))
        ));

        let permission = adapter
            .decode_permission(message_params(
                fixture.permission,
                "session/request_permission",
            ))
            .expect("v2 generic permission");
        assert!(matches!(
            permission.subject,
            Some(RequestPermissionSubject::Other(_))
        ));
        let permission = serde_json::to_value(permission).expect("serialize permission");
        assert_eq!(permission["subject"]["futureField"]["preserve"], true);
        assert_eq!(permission["_meta"]["future.permission"][1], 2);
    }

    #[test]
    fn v2_standard_responses_use_v2_sdk_dtos_and_include_cwd() {
        let fixture = fixture();
        let adapter = V2Adapter::new(true);
        let session = fixture_application_session("/tmp/kiteframe-v2");
        assert_eq!(
            adapter
                .encode_session_result("session/new", ApplicationResult::Session(session.clone()))
                .unwrap(),
            fixture.session_new_response
        );
        assert_eq!(
            adapter
                .encode_session_result(
                    "session/list",
                    ApplicationResult::Sessions(vec![session.clone()]),
                )
                .unwrap(),
            fixture.session_list_response
        );
        assert_eq!(
            adapter
                .encode_session_result("session/resume", ApplicationResult::Session(session))
                .unwrap(),
            fixture.session_resume_response
        );
        assert_eq!(
            adapter
                .encode_session_result("session/close", ApplicationResult::Closed)
                .unwrap(),
            fixture.session_close_response
        );

        let ApplicationCommand::ResolvePermission(request) = adapter
            .decode_permission_response(fixture.permission_response, permission_context())
            .expect("translate v2 permission response")
        else {
            panic!("expected internal permission resolution command");
        };
        assert!(matches!(request.choice, PermissionChoice::AllowOnce));
    }

    #[test]
    fn negotiated_v1_connection_cannot_switch_to_v2() {
        let mut disabled = ProtocolNegotiation::new(false);
        assert!(matches!(
            disabled.select_v2(),
            Err(ProtocolError::V2Disabled)
        ));

        let mut negotiation = ProtocolNegotiation::new(true);
        negotiation.select_v1().expect("select v1");
        assert!(matches!(
            negotiation.select_v2(),
            Err(ProtocolError::AlreadyNegotiated)
        ));

        let fixture = fixture();
        let initialize = message_params(fixture.initialize, "initialize");
        let disabled_connection = ProtocolConnection::new(
            AuthenticatedConnection {
                principal: PrincipalId::new("v2-disabled").unwrap(),
                transport: TransportKind::Stdio,
            },
            false,
        );
        assert!(matches!(
            disabled_connection.initialize(initialize.clone()),
            Err(ProtocolError::V2Disabled)
        ));
        assert_eq!(disabled_connection.negotiated_protocol(), None);

        let enabled_connection = ProtocolConnection::new(
            AuthenticatedConnection {
                principal: PrincipalId::new("v2-enabled").unwrap(),
                transport: TransportKind::Stdio,
            },
            true,
        );
        assert_eq!(
            enabled_connection.initialize(initialize).unwrap()["protocolVersion"],
            2
        );
        assert_eq!(
            enabled_connection.negotiated_protocol(),
            Some(kiteframe::NegotiatedProtocol::V2)
        );
        assert!(matches!(
            enabled_connection.initialize(message_params(v1_fixture().initialize, "initialize")),
            Err(ProtocolError::AlreadyNegotiated)
        ));
    }

    #[test]
    fn v2_extensions_translate_to_the_same_application_commands() {
        let fixture = v1_fixture();
        let adapter = V2Adapter::new(true);
        for extension in fixture.extensions {
            assert_eq!(extension.jsonrpc, "2.0");
            assert!(extension.id >= 10);
            let command = adapter
                .decode_extension(&extension.method, extension.params)
                .unwrap_or_else(|error| panic!("{}: {error}", extension.method));
            assert_eq!(command_kind(&command), extension.kind);
            assert_eq!(command_snapshot(&command), extension.command);
        }

        let fixture = v1_fixture();
        assert_eq!(
            adapter
                .encode_result(ApplicationResult::Published(
                    kiteframe::PublishedDefinition {
                        id: Uuid::parse_str("019ff3d8-9d86-7de0-9070-0f0185d5f416").unwrap(),
                        name: "support".to_owned(),
                        version: 7,
                        definition_hash: "sha256:fixture".to_owned(),
                    }
                ))
                .unwrap(),
            fixture.published_response
        );
    }
}
