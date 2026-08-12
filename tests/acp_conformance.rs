use serde::Deserialize;
use serde_json::{Value, json};

use kiteframe::{
    ApplicationCommand, ProtocolAdapter, ProtocolError, ProtocolNegotiation, V1Adapter,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V1Fixture {
    initialize: Value,
    session_new: Value,
    session_load: Value,
    permission: Value,
    extensions: Vec<ExtensionFixture>,
}

#[derive(Deserialize)]
struct ExtensionFixture {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Value,
    kind: String,
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
fn every_advertised_extension_maps_to_one_application_command() {
    let fixture = v1_fixture();
    let adapter = V1Adapter;

    for extension in fixture.extensions {
        assert_eq!(extension.jsonrpc, "2.0");
        assert!(extension.id >= 10);
        let command = adapter
            .decode_extension(&extension.method, extension.params)
            .unwrap_or_else(|error| panic!("{}: {error}", extension.method));
        let actual = command_kind(command);
        assert_eq!(actual, extension.kind);
    }
}

fn command_kind(command: ApplicationCommand) -> &'static str {
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
        PersistedAuthorizer, PostgresStore, PrincipalId, ResumeSession,
    };
    use uuid::Uuid;

    struct UnusedModel;

    #[async_trait]
    impl ModelClient for UnusedModel {
        async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Err(ModelError::Protocol(
                "model is not used by session lifecycle tests".to_owned(),
            ))
        }
    }

    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping ACP dispatcher test: DATABASE_URL is not configured");
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let engine = Arc::new(Engine {
        store: store.clone(),
        model: Arc::new(UnusedModel),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
        config: EngineConfig::default(),
    });
    let application = Application::new(store, engine);
    let alice = PrincipalId::new(format!("acp-alice-{}", Uuid::now_v7())).expect("principal");
    let alice_context = ApplicationContext {
        connection_principal: alice.clone(),
        session_id: None,
    };

    let ApplicationResult::Session(created) = application
        .dispatch(
            &alice_context,
            ApplicationCommand::NewSession(NewSession {
                working_directory: "/tmp/kiteframe-acp".to_owned(),
            }),
        )
        .await
        .expect("create session")
    else {
        panic!("expected created session");
    };
    assert_eq!(created.principal, alice);

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
                    session_id: created.id.clone(),
                    working_directory: "/tmp/kiteframe-acp".to_owned(),
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
    assert!(sessions.iter().any(|session| session.id == created.id));

    assert!(matches!(
        application
            .dispatch(
                &alice_context,
                ApplicationCommand::CloseSession(CloseSession {
                    session_id: created.id.clone(),
                }),
            )
            .await,
        Ok(ApplicationResult::Closed)
    ));
    assert!(
        application
            .dispatch(
                &alice_context,
                ApplicationCommand::ResumeSession(ResumeSession {
                    session_id: created.id,
                    working_directory: "/tmp/kiteframe-acp".to_owned(),
                }),
            )
            .await
            .is_err()
    );
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
            assert_eq!(command_kind(command), extension.kind);
        }
    }
}
