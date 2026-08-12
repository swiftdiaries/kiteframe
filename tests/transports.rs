use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_client_protocol::{
    Channel, Client, ConnectTo, RawJsonRpcMessage, TransportFrame, schema::v1::RequestId,
};
use agent_client_protocol_http::HttpClient;
use async_trait::async_trait;
use async_tungstenite::{
    WebSocketStream,
    tokio::{ConnectStream, connect_async},
    tungstenite::{Message as WsMessage, client::IntoClientRequest},
};
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use kiteframe::protocol::transport::{
    AcpService, BearerAuthenticator, RemoteService, serve_acp_stream,
};
use kiteframe::{
    Application, AuthenticatedConnection, CatalogTool, ClosedSchema, EffectKind, Engine,
    EngineConfig, InProcessToolRegistry, ModelClient, ModelError, ModelRequest, ModelResponse,
    PermissionRequestId, PersistedAuthorizer, PostgresStore, PrincipalId, RecoverySupport, RunId,
    RunState, Secret, SessionId, ToolCatalogSnapshot, TransportKind,
};
use serde_json::{Value, json};
use sqlx::Row;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use uuid::Uuid;

struct UnusedModel;

struct ScriptedModel(Mutex<VecDeque<Vec<Value>>>);

#[derive(Clone)]
struct CaptureLogs(Arc<Mutex<Vec<u8>>>);

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CaptureLogs {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

impl Write for CaptureLogs {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log capture poisoned").extend(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl ModelClient for UnusedModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Protocol(
            "model is not used by transport tests".to_owned(),
        ))
    }
}

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            items: self
                .0
                .lock()
                .expect("scripted model lock poisoned")
                .pop_front()
                .expect("scripted model response"),
        })
    }
}

fn principal(label: &str) -> PrincipalId {
    PrincipalId::new(format!("transport-{label}-{}", Uuid::now_v7())).unwrap()
}

fn ready_model() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

async fn application() -> (Arc<Application>, Arc<PostgresStore>) {
    let (application, store, _) = application_with_model(Arc::new(UnusedModel)).await;
    (application, store)
}

async fn application_with_model(
    model: Arc<dyn ModelClient>,
) -> (Arc<Application>, Arc<PostgresStore>, Arc<Engine>) {
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL is required for transport conformance");
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let engine = Arc::new(Engine {
        store: store.clone(),
        model,
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
        config: EngineConfig::default(),
    });
    (
        Arc::new(Application::new(store.clone(), engine.clone())),
        store,
        engine,
    )
}

#[async_trait]
trait RpcPeer {
    async fn request(&mut self, method: &str, params: Value) -> Value;
    async fn next_message(&mut self) -> Value;
    async fn respond(&mut self, id: Value, result: Value);
    async fn close(&mut self);
}

struct StdioPeer {
    lines: Lines<BufReader<ReadHalf<tokio::io::DuplexStream>>>,
    writer: WriteHalf<tokio::io::DuplexStream>,
    server: Option<tokio::task::JoinHandle<agent_client_protocol::Result<()>>>,
    next_id: i64,
}

impl StdioPeer {
    fn new(
        application: Arc<Application>,
        store: Arc<PostgresStore>,
        principal: PrincipalId,
    ) -> Self {
        Self::with_v2(application, store, principal, false)
    }

    fn with_v2(
        application: Arc<Application>,
        store: Arc<PostgresStore>,
        principal: PrincipalId,
        v2_runtime_enabled: bool,
    ) -> Self {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let server = tokio::spawn(serve_acp_stream(
            server_reader,
            server_writer,
            application,
            store,
            AuthenticatedConnection {
                principal,
                transport: TransportKind::Stdio,
            },
            v2_runtime_enabled,
        ));
        Self {
            lines: BufReader::new(reader).lines(),
            writer,
            server: Some(server),
            next_id: 1,
        }
    }

    async fn next_message(&mut self) -> Value {
        let line = self
            .lines
            .next_line()
            .await
            .expect("read stdio message")
            .expect("stdio message before EOF");
        serde_json::from_str(&line).expect("JSON-RPC message")
    }

    async fn respond(&mut self, id: Value, result: Value) {
        let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
        self.writer
            .write_all(format!("{response}\n").as_bytes())
            .await
            .expect("write stdio response");
        self.writer.flush().await.expect("flush stdio response");
    }

    async fn respond_error(&mut self, id: Value) {
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32800, "message": "client declined response"}
        });
        self.writer
            .write_all(format!("{response}\n").as_bytes())
            .await
            .expect("write stdio error response");
        self.writer
            .flush()
            .await
            .expect("flush stdio error response");
    }
}

#[async_trait]
impl RpcPeer for StdioPeer {
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write stdio request");
        self.writer.flush().await.expect("flush stdio request");
        let response = self.next_message().await;
        assert_eq!(response["id"], id);
        response
    }

    async fn next_message(&mut self) -> Value {
        StdioPeer::next_message(self).await
    }

    async fn respond(&mut self, id: Value, result: Value) {
        StdioPeer::respond(self, id, result).await;
    }

    async fn close(&mut self) {
        self.writer.shutdown().await.expect("close stdio input");
        self.server
            .take()
            .expect("stdio server")
            .await
            .expect("join stdio server")
            .expect("serve stdio");
    }
}

struct HttpPeer {
    channel: Channel,
    driver: Option<tokio::task::JoinHandle<agent_client_protocol::Result<()>>>,
    next_id: i64,
}

impl HttpPeer {
    fn new(address: std::net::SocketAddr, token: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("HTTP client");
        let transport = HttpClient::with_endpoint_and_client(format!("http://{address}/acp"), http)
            .expect("ACP HTTP client");
        let (channel, driver) = ConnectTo::<Client>::into_channel_and_future(transport);
        Self {
            channel,
            driver: Some(tokio::spawn(driver)),
            next_id: 1,
        }
    }

    async fn receive(&mut self) -> Value {
        let frame = self
            .channel
            .rx
            .next()
            .await
            .expect("HTTP message before close");
        serde_json::from_str(&frame.to_json().expect("serialize HTTP message"))
            .expect("JSON-RPC HTTP message")
    }
}

#[async_trait]
impl RpcPeer for HttpPeer {
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let message = RawJsonRpcMessage::request(method.to_owned(), params, RequestId::Number(id))
            .expect("JSON-RPC request");
        self.channel
            .tx
            .unbounded_send(TransportFrame::Single(message))
            .expect("send HTTP request");
        let response = self.receive().await;
        assert_eq!(response["id"], id);
        response
    }

    async fn next_message(&mut self) -> Value {
        self.receive().await
    }

    async fn respond(&mut self, id: Value, result: Value) {
        let id = serde_json::from_value(id).expect("JSON-RPC request id");
        self.channel
            .tx
            .unbounded_send(TransportFrame::Single(RawJsonRpcMessage::response(
                id,
                Ok(result),
            )))
            .expect("send HTTP response");
    }

    async fn close(&mut self) {
        self.channel.tx.close_channel();
        self.driver
            .take()
            .expect("HTTP driver")
            .await
            .expect("join HTTP driver")
            .expect("drive HTTP transport");
    }
}

struct WebSocketPeer {
    socket: WebSocketStream<ConnectStream>,
    next_id: i64,
}

impl WebSocketPeer {
    async fn connect(address: std::net::SocketAddr, token: &str) -> Self {
        let mut request = format!("ws://{address}/acp")
            .into_client_request()
            .expect("WebSocket request");
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let (socket, response) = connect_async(request).await.expect("connect WebSocket");
        assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);
        Self { socket, next_id: 1 }
    }

    async fn receive(&mut self) -> Value {
        loop {
            match self.socket.next().await.expect("WebSocket message") {
                Ok(WsMessage::Text(text)) => {
                    return serde_json::from_str(&text).expect("JSON-RPC WebSocket message");
                }
                Ok(WsMessage::Ping(_) | WsMessage::Pong(_)) => {}
                Ok(other) => panic!("unexpected WebSocket frame: {other:?}"),
                Err(error) => panic!("WebSocket receive failed: {error}"),
            }
        }
    }
}

#[async_trait]
impl RpcPeer for WebSocketPeer {
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.socket
            .send(WsMessage::Text(request.to_string().into()))
            .await
            .expect("send WebSocket request");
        let response = self.receive().await;
        assert_eq!(response["id"], id);
        response
    }

    async fn next_message(&mut self) -> Value {
        self.receive().await
    }

    async fn respond(&mut self, id: Value, result: Value) {
        let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
        self.socket
            .send(WsMessage::Text(response.to_string().into()))
            .await
            .expect("send WebSocket response");
    }

    async fn close(&mut self) {
        self.socket.close(None).await.expect("close WebSocket");
    }
}

struct TestServer {
    address: std::net::SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl TestServer {
    async fn start(service: &RemoteService) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ACP server");
        let address = listener.local_addr().expect("ACP server address");
        let (shutdown, receiver) = oneshot::channel();
        let router = service.router();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
        });
        Self {
            address,
            shutdown: Some(shutdown),
            task,
        }
    }

    async fn stop(mut self) {
        let _ = self.shutdown.take().expect("shutdown sender").send(());
        self.task
            .await
            .expect("join ACP server")
            .expect("serve ACP");
    }
}

fn success(response: Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "JSON-RPC error: {response}"
    );
    response["result"].clone()
}

struct PendingPermissionFixture {
    session: Value,
    session_id: SessionId,
    run_id: RunId,
    request_id: PermissionRequestId,
}

async fn create_pending_permission<P: RpcPeer + Send>(
    peer: &mut P,
    store: &PostgresStore,
    engine: &Engine,
    protocol_version: u16,
) -> PendingPermissionFixture {
    let initialize = if protocol_version == 1 {
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "permission-test", "version": "1.0.0"}
        })
    } else {
        json!({
            "protocolVersion": 2,
            "capabilities": {},
            "info": {"name": "permission-test", "version": "2.0.0"}
        })
    };
    success(peer.request("initialize", initialize).await);
    let new_session = if protocol_version == 1 {
        json!({"cwd": "/tmp/kiteframe-permission", "mcpServers": []})
    } else {
        json!({"cwd": "/tmp/kiteframe-permission"})
    };
    let session = success(peer.request("session/new", new_session).await);
    let session_id = SessionId::new(
        Uuid::parse_str(session["sessionId"].as_str().expect("session id")).unwrap(),
    );
    let descriptor = CatalogTool::new(
        "docs/search",
        "docs",
        "Search records",
        ClosedSchema::object("input", BTreeMap::new()),
        ClosedSchema::object("output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("permission tool");
    let catalog = ToolCatalogSnapshot::new(session_id.clone(), Uuid::now_v7(), [descriptor])
        .expect("permission catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("append permission catalog");
    let workflow_name = format!("permission{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {workflow_name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"permission\"\"\" tools [search] on ok(answer: string) {{ done answer }} }}"
    );
    let definition = success(
        peer.request("_kiteframe/workflow/publish", json!({"source": source}))
            .await,
    );
    let started = success(
        peer.request(
            "_kiteframe/run/start",
            json!({
                "sessionId": session["sessionId"],
                "workflowName": workflow_name,
                "version": definition["version"],
                "input": {},
                "idempotencyKey": format!("permission-{}", Uuid::now_v7())
            }),
        )
        .await,
    );
    let run_id = RunId::new(
        Uuid::parse_str(started["id"].as_str().expect("started run id")).expect("run UUID"),
    );
    assert_eq!(engine.tick_run(run_id.clone()).await.expect("tick run"), 1);
    assert_eq!(
        store
            .get_run(session_id.clone(), run_id.clone())
            .await
            .expect("waiting run")
            .state,
        RunState::WaitingForPermission
    );
    let request_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM permission_requests WHERE run_id = $1 AND state = 'pending'",
    )
    .bind(*run_id.as_uuid())
    .fetch_one(store.pool())
    .await
    .expect("pending permission id");
    let request_id = PermissionRequestId::new(request_id);
    PendingPermissionFixture {
        session,
        session_id,
        run_id,
        request_id,
    }
}

async fn exercise_permission<P: RpcPeer + Send>(
    peer: &mut P,
    store: &PostgresStore,
    engine: &Engine,
    protocol_version: u16,
) {
    let PendingPermissionFixture {
        session,
        session_id,
        run_id,
        request_id,
    } = create_pending_permission(peer, store, engine, protocol_version).await;
    let permission = tokio::time::timeout(Duration::from_secs(2), peer.next_message())
        .await
        .expect("transport must emit an unsolicited permission request");
    assert_eq!(permission["method"], "session/request_permission");
    assert_eq!(permission["params"]["sessionId"], session["sessionId"]);
    if protocol_version == 1 {
        assert_eq!(permission["params"]["toolCall"]["status"], "pending");
    } else {
        assert!(permission["params"]["title"].is_string());
        assert_eq!(permission["params"]["subject"]["type"], "tool_call");
    }
    assert!(
        permission["params"]["options"]
            .as_array()
            .expect("permission options")
            .iter()
            .any(|option| option["optionId"] == "deny-once")
    );

    peer.respond(
        json!(format!("unknown-{}", Uuid::now_v7())),
        json!({"outcome": {"outcome": "selected", "optionId": "deny-once"}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !store
            .pending_permission(session_id.clone(), run_id.clone(), request_id.clone())
            .await
            .expect("unknown response leaves permission pending")
            .resolved
    );

    peer.respond(
        permission["id"].clone(),
        json!({"outcome": {"outcome": "selected", "optionId": "deny-once"}}),
    )
    .await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .pending_permission(session_id.clone(), run_id.clone(), request_id.clone())
                .await
                .expect("read permission")
                .resolved
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("permission response must resolve the durable request");

    peer.respond(
        permission["id"].clone(),
        json!({"outcome": {"outcome": "selected", "optionId": "allow-once"}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let row = sqlx::query(
        "SELECT t.state, t.error, f.state AS frame_state \
         FROM tool_calls t JOIN frames f ON f.id = t.frame_id \
         WHERE f.run_id = $1",
    )
    .bind(*run_id.as_uuid())
    .fetch_one(store.pool())
    .await
    .expect("denied tool call");
    assert_eq!(row.get::<String, _>("state"), "failed");
    assert_eq!(row.get::<String, _>("frame_state"), "runnable");
    assert_eq!(
        row.get::<Option<Value>, _>("error"),
        Some(json!({
            "code": "permission_denied",
            "message": "Tool invocation denied by user"
        }))
    );
    assert_eq!(
        store
            .get_run(session_id.clone(), run_id.clone())
            .await
            .expect("denied run")
            .state,
        RunState::Running
    );
    let event_kinds = store
        .events_after(session_id, run_id, 0, 64)
        .await
        .expect("permission events")
        .into_iter()
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    let requested = event_kinds
        .iter()
        .position(|kind| kind == "permission.requested")
        .expect("permission.requested event");
    let resolved = event_kinds
        .iter()
        .position(|kind| kind == "permission.resolved")
        .expect("permission.resolved event");
    let failed = event_kinds
        .iter()
        .position(|kind| kind == "tool.failed")
        .expect("tool.failed event");
    assert!(requested < resolved && resolved < failed);
    assert_eq!(
        event_kinds
            .iter()
            .filter(|kind| kind.as_str() == "permission.resolved")
            .count(),
        1
    );
}

#[tokio::test]
async fn all_transports_correlate_unsolicited_permission_responses() {
    let scripted = VecDeque::from([
        vec![
            json!({"type":"function_call","call_id":"permission-call","name":"docs/search","arguments":{}}),
        ],
        vec![
            json!({"type":"function_call","call_id":"permission-call","name":"docs/search","arguments":{}}),
        ],
        vec![
            json!({"type":"function_call","call_id":"permission-call","name":"docs/search","arguments":{}}),
        ],
        vec![
            json!({"type":"function_call","call_id":"permission-call","name":"docs/search","arguments":{}}),
        ],
    ]);
    let (application, store, engine) =
        application_with_model(Arc::new(ScriptedModel(Mutex::new(scripted)))).await;
    let authenticator = Arc::new(
        BearerAuthenticator::new([
            (Secret::new("permission-http"), principal("permission-http")),
            (
                Secret::new("permission-websocket"),
                principal("permission-websocket"),
            ),
        ])
        .expect("permission bearer bindings"),
    );
    let remote = RemoteService::new(
        application.clone(),
        authenticator,
        store.clone(),
        ready_model(),
    );
    remote.mark_ready();
    let server = TestServer::start(&remote).await;

    let mut stdio = StdioPeer::new(
        application.clone(),
        store.clone(),
        principal("permission-stdio"),
    );
    exercise_permission(&mut stdio, &store, &engine, 1).await;
    stdio.close().await;

    let mut http = HttpPeer::new(server.address, "permission-http");
    exercise_permission(&mut http, &store, &engine, 1).await;
    http.close().await;

    let mut websocket = WebSocketPeer::connect(server.address, "permission-websocket").await;
    exercise_permission(&mut websocket, &store, &engine, 1).await;
    websocket.close().await;

    #[cfg(feature = "acp-v2")]
    {
        let mut v2 = StdioPeer::with_v2(
            application.clone(),
            store.clone(),
            principal("permission-v2"),
            true,
        );
        exercise_permission(&mut v2, &store, &engine, 2).await;
        v2.close().await;
    }

    remote.begin_shutdown();
    server.stop().await;
}

#[tokio::test]
async fn externally_resolved_permissions_cancel_outstanding_sdk_requests() {
    let scripted = VecDeque::from([vec![json!({
        "type": "function_call",
        "call_id": "permission-external-resolution",
        "name": "docs/search",
        "arguments": {}
    })]]);
    let (application, store, engine) =
        application_with_model(Arc::new(ScriptedModel(Mutex::new(scripted)))).await;
    let owner = principal("permission-external-resolution");
    let mut first = StdioPeer::new(application.clone(), store.clone(), owner.clone());
    let PendingPermissionFixture {
        session,
        session_id,
        run_id,
        request_id,
    } = create_pending_permission(&mut first, &store, &engine, 1).await;
    let first_request = tokio::time::timeout(Duration::from_secs(2), first.next_message())
        .await
        .expect("first connection permission request");

    let mut second = StdioPeer::new(application, store.clone(), owner);
    success(
        second
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "permission-resolver", "version": "1.0.0"}
                }),
            )
            .await,
    );
    success(
        second
            .request(
                "session/load",
                json!({
                    "sessionId": session["sessionId"],
                    "cwd": "/tmp/kiteframe-permission",
                    "mcpServers": []
                }),
            )
            .await,
    );
    let second_request = tokio::time::timeout(Duration::from_secs(2), second.next_message())
        .await
        .expect("second connection permission request");
    second
        .respond(
            second_request["id"].clone(),
            json!({"outcome": {"outcome": "selected", "optionId": "deny-once"}}),
        )
        .await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .pending_permission(session_id.clone(), run_id.clone(), request_id.clone())
                .await
                .expect("externally resolved permission")
                .resolved
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second connection must resolve permission");

    let cancellation = tokio::time::timeout(Duration::from_secs(3), first.next_message())
        .await
        .expect("first connection SDK request must be cancelled");
    assert_eq!(cancellation["method"], "$/cancel_request");
    assert_eq!(cancellation["params"]["requestId"], first_request["id"]);
    assert!(cancellation.get("id").is_none());

    second.close().await;
    first.close().await;
}

#[tokio::test]
async fn session_rebinding_preserves_an_already_correlated_permission_response() {
    let scripted = VecDeque::from([vec![json!({
        "type": "function_call",
        "call_id": "permission-rebind",
        "name": "docs/search",
        "arguments": {}
    })]]);
    let (application, store, engine) =
        application_with_model(Arc::new(ScriptedModel(Mutex::new(scripted)))).await;
    let mut peer = StdioPeer::new(application, store.clone(), principal("permission-rebind"));
    let PendingPermissionFixture {
        session_id,
        run_id,
        request_id,
        ..
    } = create_pending_permission(&mut peer, &store, &engine, 1).await;
    let permission = tokio::time::timeout(Duration::from_secs(2), peer.next_message())
        .await
        .expect("permission request before session rebind");

    success(
        peer.request(
            "session/new",
            json!({"cwd": "/tmp/kiteframe-rebound", "mcpServers": []}),
        )
        .await,
    );
    peer.respond(
        permission["id"].clone(),
        json!({"outcome": {"outcome": "selected", "optionId": "deny-once"}}),
    )
    .await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .pending_permission(session_id.clone(), run_id.clone(), request_id.clone())
                .await
                .expect("original permission")
                .resolved
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("captured response must resolve its original session permission");
    peer.close().await;
}

#[tokio::test]
async fn permission_discovery_progresses_past_the_oldest_256_pending_rows() {
    let scripted = VecDeque::from([vec![json!({
        "type": "function_call",
        "call_id": "permission-page-root",
        "name": "docs/search",
        "arguments": {}
    })]]);
    let (application, store, engine) =
        application_with_model(Arc::new(ScriptedModel(Mutex::new(scripted)))).await;
    let owner = principal("permission-page");
    let mut peer = StdioPeer::new(application.clone(), store.clone(), owner.clone());
    let fixture = create_pending_permission(&mut peer, &store, &engine, 1).await;
    let frame_id: Uuid =
        sqlx::query_scalar("SELECT frame_id FROM permission_requests WHERE id = $1")
            .bind(*fixture.request_id.as_uuid())
            .fetch_one(store.pool())
            .await
            .expect("permission frame");
    let descriptor_digest: String =
        sqlx::query_scalar("SELECT descriptor_digest FROM permission_requests WHERE id = $1")
            .bind(*fixture.request_id.as_uuid())
            .fetch_one(store.pool())
            .await
            .expect("permission descriptor");
    sqlx::query(
        "UPDATE permission_requests SET created_at = CURRENT_TIMESTAMP - INTERVAL '10 minutes' WHERE id = $1",
    )
    .bind(*fixture.request_id.as_uuid())
    .execute(store.pool())
    .await
    .expect("age root permission");
    sqlx::query(
        "WITH calls AS ( \
           INSERT INTO tool_calls (id, frame_id, provider_call_id, qualified_name, descriptor_digest, idempotency_key, arguments, state, created_at, updated_at) \
           SELECT gen_random_uuid(), $1, 'overflow-' || n, 'docs/search', $2, 'overflow-key-' || n, '{}'::jsonb, 'waiting_for_permission', CURRENT_TIMESTAMP - INTERVAL '9 minutes' + n * INTERVAL '1 millisecond', CURRENT_TIMESTAMP \
           FROM generate_series(1, 256) AS n \
           RETURNING provider_call_id, created_at \
         ) \
         INSERT INTO permission_requests (id, run_id, frame_id, provider_call_id, qualified_tool, descriptor_digest, input_digest, policy_revision, state, created_at) \
         SELECT gen_random_uuid(), $3, $1, provider_call_id, 'docs/search', $2, 'overflow-input', 0, 'pending', created_at FROM calls",
    )
    .bind(frame_id)
    .bind(&descriptor_digest)
    .bind(*fixture.run_id.as_uuid())
    .execute(store.pool())
    .await
    .expect("overflow permission rows");

    let mut outstanding = Vec::new();
    for _ in 0..32 {
        outstanding.push(
            tokio::time::timeout(Duration::from_secs(2), peer.next_message())
                .await
                .expect("permission window must fill"),
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.next_message())
            .await
            .is_err(),
        "an unanswered client must have a bounded outstanding permission window"
    );

    let first_page = outstanding
        .iter()
        .map(|message| {
            message["params"]["toolCall"]["toolCallId"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    let abandoned = outstanding[0]["params"]["toolCall"]["toolCallId"]
        .as_str()
        .unwrap()
        .to_owned();
    peer.close().await;

    let mut lock_tx = store.pool().begin().await.expect("permission row lock");
    let locked = sqlx::query(
        "SELECT id, provider_call_id FROM permission_requests \
         WHERE run_id = $1 AND state = 'pending' AND last_offered_at IS NULL \
         ORDER BY created_at, id LIMIT 1 FOR UPDATE",
    )
    .bind(*fixture.run_id.as_uuid())
    .fetch_one(&mut *lock_tx)
    .await
    .expect("oldest unoffered permission");
    let locked_id: Uuid = locked.get("id");
    let locked_provider_call_id: String = locked.get("provider_call_id");
    sqlx::query(
        "UPDATE permission_requests SET state = 'resolved', resolution = $1, \
         resolution_idempotency_key = $2, resolved_at = CURRENT_TIMESTAMP WHERE id = $3",
    )
    .bind(json!({"kind": "denied"}))
    .bind(format!("locked-resolution-{locked_id}"))
    .bind(locked_id)
    .execute(&mut *lock_tx)
    .await
    .expect("resolve permission while retaining its row lock");

    let producing = Arc::new(AtomicBool::new(true));
    let inserted = Arc::new(AtomicUsize::new(0));
    let (progress_tx, mut progress_rx) = tokio::sync::watch::channel(0_usize);
    let live_store = store.clone();
    let live_run = fixture.run_id.clone();
    let live_descriptor = descriptor_digest.clone();
    let live_producing = producing.clone();
    let live_inserted = inserted.clone();
    let live_inserter = tokio::spawn(async move {
        let producer = Uuid::now_v7().simple().to_string();
        while live_producing.load(Ordering::Acquire) {
            let n = live_inserted.fetch_add(1, Ordering::AcqRel) + 1;
            let provider_call_id = format!("live-{producer}-{n}");
            sqlx::query(
                "WITH call AS ( \
                   INSERT INTO tool_calls (id, frame_id, provider_call_id, qualified_name, descriptor_digest, idempotency_key, arguments, state) \
                   VALUES (gen_random_uuid(), $1, $2, 'docs/search', $3, $4, '{}'::jsonb, 'waiting_for_permission') \
                 ) \
                 INSERT INTO permission_requests (id, run_id, frame_id, provider_call_id, qualified_tool, descriptor_digest, input_digest, policy_revision, state) \
                 VALUES (gen_random_uuid(), $5, $1, $2, 'docs/search', $3, 'live-input', 0, 'pending')",
            )
            .bind(frame_id)
            .bind(&provider_call_id)
            .bind(&live_descriptor)
            .bind(format!("live-key-{producer}-{n}"))
            .bind(*live_run.as_uuid())
            .execute(live_store.pool())
            .await
            .expect("insert sustained permission");
            progress_tx.send_replace(n);
            tokio::task::yield_now().await;
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while *progress_rx.borrow() < 32 {
            progress_rx.changed().await.expect("producer progress");
        }
    })
    .await
    .expect("sustained producer must start");

    let mut first = StdioPeer::new(application.clone(), store.clone(), owner.clone());
    let mut second = StdioPeer::new(application, store.clone(), owner);
    let (first_initialize, second_initialize) = tokio::join!(
        first.request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {"name": "fairness-first", "version": "1.0.0"}}),
        ),
        second.request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {"name": "fairness-second", "version": "1.0.0"}}),
        ),
    );
    success(first_initialize);
    success(second_initialize);
    let session = fixture.session["sessionId"].clone();
    let (first_load, second_load) = tokio::join!(
        first.request(
            "session/load",
            json!({"sessionId": session, "cwd": "/tmp/kiteframe-permission", "mcpServers": []}),
        ),
        second.request(
            "session/load",
            json!({"sessionId": session, "cwd": "/tmp/kiteframe-permission", "mcpServers": []}),
        ),
    );
    success(first_load);
    success(second_load);

    let (mut first_window, mut second_window) =
        tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(
                async {
                    let mut window = Vec::with_capacity(32);
                    for _ in 0..32 {
                        window.push(first.next_message().await);
                    }
                    window
                },
                async {
                    let mut window = Vec::with_capacity(32);
                    for _ in 0..32 {
                        window.push(second.next_message().await);
                    }
                    window
                },
            )
        })
        .await
        .expect("both observers must claim around the locked row");
    assert_eq!(first_window.len(), 32);
    assert_eq!(second_window.len(), 32);
    assert!(
        first_window.iter().chain(&second_window).all(|message| {
            message["params"]["toolCall"]["toolCallId"] != locked_provider_call_id
        }),
        "the locked row must be skipped by both concurrent observers"
    );
    assert!(
        first_window.iter().chain(&second_window).any(|message| {
            !first_page.contains(
                message["params"]["toolCall"]["toolCallId"]
                    .as_str()
                    .unwrap(),
            )
        }),
        "concurrent observers must continue beyond the abandoned window"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), first.next_message())
            .await
            .is_err(),
        "the first observer must retain at most 32 requests"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), second.next_message())
            .await
            .is_err(),
        "the second observer must retain at most 32 requests"
    );

    lock_tx.commit().await.expect("commit locked resolution");
    let locked_state: String =
        sqlx::query_scalar("SELECT state FROM permission_requests WHERE id = $1")
            .bind(locked_id)
            .fetch_one(store.pool())
            .await
            .expect("resolved locked permission");
    assert_eq!(locked_state, "resolved");

    let mut found_tail = false;
    let mut retried_abandoned = false;
    let observe = |message: &Value| {
        let provider_call_id = message["params"]["toolCall"]["toolCallId"]
            .as_str()
            .expect("provider call id");
        (
            provider_call_id == "overflow-256",
            provider_call_id == abandoned,
        )
    };
    for message in first_window.iter().chain(&second_window) {
        let (tail, abandoned) = observe(message);
        found_tail |= tail;
        retried_abandoned |= abandoned;
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        while !(found_tail && retried_abandoned) {
            let message = first_window.pop().expect("full first observer window");
            first
                .respond(
                    message["id"].clone(),
                    json!({"outcome": {"outcome": "cancelled"}}),
                )
                .await;
            let replacement = first.next_message().await;
            let (tail, abandoned) = observe(&replacement);
            found_tail |= tail;
            retried_abandoned |= abandoned;
            first_window.push(replacement);
            assert_eq!(first_window.len(), 32);

            let message = second_window.pop().expect("full second observer window");
            second
                .respond(
                    message["id"].clone(),
                    json!({"outcome": {"outcome": "cancelled"}}),
                )
                .await;
            let replacement = second.next_message().await;
            let (tail, abandoned) = observe(&replacement);
            found_tail |= tail;
            retried_abandoned |= abandoned;
            second_window.push(replacement);
            assert_eq!(second_window.len(), 32);
        }
    })
    .await
    .expect("old permissions must progress under sustained arrivals");
    assert!(found_tail, "the 257th pending permission must be offered");
    assert!(
        retried_abandoned,
        "an abandoned old permission must be retried during sustained inserts"
    );
    assert!(producing.load(Ordering::Acquire));
    let progress_at_targets = *progress_rx.borrow();
    tokio::time::timeout(Duration::from_secs(2), async {
        while *progress_rx.borrow() <= progress_at_targets {
            progress_rx.changed().await.expect("producer remains live");
        }
    })
    .await
    .expect("producer must insert after both historical targets are observed");
    let pending_tail: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM permission_requests WHERE run_id = $1 AND state = 'pending'",
    )
    .bind(*fixture.run_id.as_uuid())
    .fetch_one(store.pool())
    .await
    .expect("pending tail size");
    assert!(
        pending_tail > 64,
        "the producer must keep a nonempty tail beyond both observer windows"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), first.next_message())
            .await
            .is_err(),
        "the first observer must remain bounded during rotation"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), second.next_message())
            .await
            .is_err(),
        "the second observer must remain bounded during rotation"
    );

    producing.store(false, Ordering::Release);
    live_inserter.await.expect("sustained permission producer");
    assert!(inserted.load(Ordering::Acquire) > progress_at_targets);
    first.close().await;
    second.close().await;
}

#[cfg(feature = "acp-v2")]
#[tokio::test]
async fn closing_a_session_cancels_its_outstanding_sdk_requests() {
    let scripted = VecDeque::from([vec![json!({
        "type": "function_call",
        "call_id": "permission-session-close",
        "name": "docs/search",
        "arguments": {}
    })]]);
    let (application, store, engine) =
        application_with_model(Arc::new(ScriptedModel(Mutex::new(scripted)))).await;
    let mut peer = StdioPeer::with_v2(
        application,
        store.clone(),
        principal("permission-session-close"),
        true,
    );
    let PendingPermissionFixture { session, .. } =
        create_pending_permission(&mut peer, &store, &engine, 2).await;
    let permission = tokio::time::timeout(Duration::from_secs(2), peer.next_message())
        .await
        .expect("session permission request");

    success(
        peer.request("session/close", json!({"sessionId": session["sessionId"]}))
            .await,
    );
    let cancellation = tokio::time::timeout(Duration::from_secs(2), peer.next_message())
        .await
        .expect("closing a session must cancel its permission request");
    assert_eq!(cancellation["method"], "$/cancel_request");
    assert_eq!(cancellation["params"]["requestId"], permission["id"]);

    peer.close().await;
}

#[tokio::test]
async fn permission_replays_after_errors_cancellation_and_reconnect() {
    let scripted = VecDeque::from([vec![json!({
        "type": "function_call",
        "call_id": "permission-reconnect",
        "name": "docs/search",
        "arguments": {}
    })]]);
    let (application, store, engine) =
        application_with_model(Arc::new(ScriptedModel(Mutex::new(scripted)))).await;
    let owner = principal("permission-reconnect");
    let mut first = StdioPeer::new(application.clone(), store.clone(), owner.clone());
    let PendingPermissionFixture {
        session,
        session_id,
        run_id,
        request_id,
    } = create_pending_permission(&mut first, &store, &engine, 1).await;

    let first_request = tokio::time::timeout(Duration::from_secs(2), first.next_message())
        .await
        .expect("initial permission request");
    first.respond_error(first_request["id"].clone()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !store
            .pending_permission(session_id.clone(), run_id.clone(), request_id.clone())
            .await
            .expect("error response leaves permission pending")
            .resolved
    );

    let second_request = tokio::time::timeout(Duration::from_secs(2), first.next_message())
        .await
        .expect("permission retry after error response");
    assert_ne!(second_request["id"], first_request["id"]);
    first
        .respond(
            second_request["id"].clone(),
            json!({"outcome": {"outcome": "cancelled"}}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !store
            .pending_permission(session_id.clone(), run_id.clone(), request_id.clone())
            .await
            .expect("cancelled outcome leaves permission pending")
            .resolved
    );

    let disconnected_request = tokio::time::timeout(Duration::from_secs(2), first.next_message())
        .await
        .expect("permission retry after cancelled outcome");
    first.close().await;

    let mut resumed = StdioPeer::new(application, store.clone(), owner);
    success(
        resumed
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "permission-resume", "version": "1.0.0"}
                }),
            )
            .await,
    );
    success(
        resumed
            .request(
                "session/load",
                json!({
                    "sessionId": session["sessionId"],
                    "cwd": "/tmp/kiteframe-permission",
                    "mcpServers": []
                }),
            )
            .await,
    );
    let replayed = tokio::time::timeout(Duration::from_secs(2), resumed.next_message())
        .await
        .expect("unresolved permission must replay after reconnect");
    assert_eq!(replayed["method"], "session/request_permission");
    assert_ne!(replayed["id"], disconnected_request["id"]);
    resumed
        .respond(
            replayed["id"].clone(),
            json!({"outcome": {"outcome": "selected", "optionId": "deny-once"}}),
        )
        .await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .pending_permission(session_id.clone(), run_id.clone(), request_id.clone())
                .await
                .expect("replayed permission")
                .resolved
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replayed permission response must resolve");
    assert_eq!(
        store
            .events_after(session_id, run_id, 0, 64)
            .await
            .expect("reconnect events")
            .into_iter()
            .filter(|event| event.kind == "permission.resolved")
            .count(),
        1
    );
    resumed.close().await;
}

async fn exercise<P: RpcPeer + Send>(peer: &mut P, store: &PostgresStore) -> Value {
    let initialized = success(
        peer.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": {"name": "transport-test", "version": "1.0.0"}
            }),
        )
        .await,
    );
    let session = success(
        peer.request(
            "session/new",
            json!({"cwd": "/tmp/kiteframe-transport", "mcpServers": []}),
        )
        .await,
    );
    let session_id = SessionId::new(
        Uuid::parse_str(session["sessionId"].as_str().expect("session id")).unwrap(),
    );
    store
        .append_catalog_epoch(
            &ToolCatalogSnapshot::new(session_id.clone(), Uuid::now_v7(), [])
                .expect("empty catalog"),
        )
        .await
        .expect("append empty catalog");

    let source = "workflow transport() -> string { start t() } task t() -> string { prompt \"\"\"transport sentinel content\"\"\" tools [] on ok(answer: string) { done answer } }";
    let published = success(
        peer.request("_kiteframe/workflow/publish", json!({"source": source}))
            .await,
    );
    let started = success(
        peer.request(
            "_kiteframe/run/start",
            json!({
                "sessionId": session["sessionId"],
                "workflowName": "transport",
                "version": published["version"],
                "input": {},
                "idempotencyKey": "transport-start"
            }),
        )
        .await,
    );
    let current = success(
        peer.request("_kiteframe/run/get", json!({"runId": started["id"]}))
            .await,
    );
    let events = success(
        peer.request(
            "_kiteframe/run/event",
            json!({"runId": started["id"], "after": 0, "limit": 64}),
        )
        .await,
    );

    json!({
        "protocolVersion": initialized["protocolVersion"],
        "published": {
            "name": published["name"],
            "version": published["version"],
            "definitionHash": published["definitionHash"],
        },
        "started": {
            "workflowName": started["workflowName"],
            "version": started["version"],
            "state": started["state"],
            "revision": started["revision"],
        },
        "current": {
            "workflowName": current["workflowName"],
            "version": current["version"],
            "state": current["state"],
            "revision": current["revision"],
        },
        "events": events["events"].as_array().expect("events").iter().map(|event| {
            json!({"sequence": event["sequence"], "kind": event["kind"]})
        }).collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn stdio_websocket_and_streamable_http_are_application_equivalent() {
    let (application, store) = application().await;
    let stdio_principal = principal("stdio");
    let http_principal = principal("http");
    let websocket_principal = principal("websocket");
    let authenticator = Arc::new(
        BearerAuthenticator::new([
            (Secret::new("http-secret"), http_principal),
            (Secret::new("websocket-secret"), websocket_principal),
        ])
        .expect("bearer bindings"),
    );
    let remote = RemoteService::new(
        application.clone(),
        authenticator,
        store.clone(),
        ready_model(),
    );
    remote.mark_ready();
    let server = TestServer::start(&remote).await;

    let mut stdio = StdioPeer::new(application, store.clone(), stdio_principal);
    let stdio_result = exercise(&mut stdio, &store).await;
    stdio.close().await;

    let mut http = HttpPeer::new(server.address, "http-secret");
    let http_result = exercise(&mut http, &store).await;
    http.close().await;

    let mut websocket = WebSocketPeer::connect(server.address, "websocket-secret").await;
    let websocket_result = exercise(&mut websocket, &store).await;
    websocket.close().await;

    assert_eq!(http_result, stdio_result);
    assert_eq!(websocket_result, stdio_result);

    remote.begin_shutdown();
    server.stop().await;
}

#[tokio::test]
async fn remote_admission_is_bounded_and_database_gated() {
    let (application, store) = application().await;
    let authenticator = Arc::new(
        BearerAuthenticator::new([(Secret::new("bounded-secret"), principal("bounded"))])
            .expect("bearer binding"),
    );
    let remote = RemoteService::with_v2_and_limit(
        application,
        authenticator,
        store.clone(),
        ready_model(),
        false,
        1,
    );
    remote.mark_ready();
    let server = TestServer::start(&remote).await;

    for _ in 0..2 {
        let mut rejected = format!("ws://{}/acp", server.address)
            .into_client_request()
            .expect("rejected WebSocket request");
        rejected.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer bounded-secret"),
        );
        rejected.headers_mut().insert(
            http::header::ORIGIN,
            HeaderValue::from_static("https://untrusted.example"),
        );
        let error = connect_async(rejected)
            .await
            .expect_err("disabled CORS must reject WebSocket origins");
        let response = match error {
            async_tungstenite::tungstenite::Error::Http(response) => response,
            other => panic!("expected HTTP origin rejection, got {other:?}"),
        };
        assert_eq!(response.status(), http::StatusCode::FORBIDDEN);
        assert_eq!(remote.available_connection_capacity(), 1);
    }

    let mut failed_upgrade = TcpStream::connect(server.address)
        .await
        .expect("connect raw WebSocket upgrade");
    failed_upgrade
        .write_all(
            format!(
                "GET /acp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer bounded-secret\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                server.address
            )
            .as_bytes(),
        )
        .await
        .expect("send accepted WebSocket upgrade");
    let mut failed_upgrade = BufReader::new(failed_upgrade);
    let mut status = String::new();
    failed_upgrade
        .read_line(&mut status)
        .await
        .expect("upgrade status");
    assert!(status.contains("101"), "accepted upgrade status: {status}");
    drop(failed_upgrade);
    tokio::time::timeout(Duration::from_secs(2), async {
        while remote.available_connection_capacity() != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("failed downstream upgrade must restore admission capacity");

    let mut first = WebSocketPeer::connect(server.address, "bounded-secret").await;
    success(
        first
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "admission-holder", "version": "1.0.0"}
                }),
            )
            .await,
    );
    assert_eq!(remote.available_connection_capacity(), 0);

    let mut request = format!("ws://{}/acp", server.address)
        .into_client_request()
        .expect("overload WebSocket request");
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer bounded-secret"),
    );
    let error = connect_async(request)
        .await
        .expect_err("connection limit must reject excess work");
    let response = match error {
        async_tungstenite::tungstenite::Error::Http(response) => response,
        other => panic!("expected HTTP overload response, got {other:?}"),
    };
    assert_eq!(response.status(), http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[http::header::RETRY_AFTER], "1");
    success(
        first
            .request(
                "session/new",
                json!({"cwd": "/tmp/kiteframe-at-capacity", "mcpServers": []}),
            )
            .await,
    );

    first.close().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut retried = WebSocketPeer::connect(server.address, "bounded-secret").await;
    retried.close().await;

    store.pool().close().await;
    let response = reqwest::Client::new()
        .get(format!("http://{}/acp", server.address))
        .bearer_auth("bounded-secret")
        .send()
        .await
        .expect("database-gated ACP response");
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[reqwest::header::RETRY_AFTER], "1");

    remote.begin_shutdown();
    server.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn remote_surface_separates_auth_upgrade_protocol_and_application_failures() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_env_filter("info,agent_client_protocol=off,agent_client_protocol_http=off")
        .with_writer(CaptureLogs(captured.clone()))
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("install transport log capture");
    let (application, store) = application().await;
    let owner = principal("remote-owner");
    let authenticator = Arc::new(
        BearerAuthenticator::new([(Secret::new("sentinel-bearer-secret"), owner.clone())])
            .expect("bearer binding"),
    );
    assert!(!format!("{authenticator:?}").contains("sentinel-bearer-secret"));
    let model_ready = Arc::new(AtomicBool::new(false));
    let remote = RemoteService::new(
        application,
        authenticator,
        store.clone(),
        model_ready.clone(),
    );
    remote.mark_ready();
    let server = TestServer::start(&remote).await;
    let base = format!("http://{}", server.address);
    let client = reqwest::Client::new();

    assert_eq!(
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    model_ready.store(true, Ordering::Release);

    assert_eq!(
        client
            .get(format!("{base}/acp"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{base}/acp"))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("{base}/acp"))
            .bearer_auth("sentinel-bearer-secret")
            .header("Upgrade", "websocket")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UPGRADE_REQUIRED
    );
    assert_eq!(
        client
            .get(format!("{base}/acp"))
            .bearer_auth("sentinel-bearer-secret")
            .header("Connection", "Upgrade")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UPGRADE_REQUIRED
    );
    assert_eq!(
        client
            .get(format!("{base}/acp"))
            .bearer_auth("sentinel-bearer-secret")
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "!!!!!!!!!!!!!!!!!!!!!!!!")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UPGRADE_REQUIRED
    );
    assert_eq!(
        client
            .post(format!("{base}/acp"))
            .bearer_auth("sentinel-bearer-secret")
            .header("Content-Type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    for absent in ["/health", "/workflows", "/events"] {
        assert_eq!(
            client
                .get(format!("{base}{absent}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }

    let mut peer = HttpPeer::new(server.address, "sentinel-bearer-secret");
    success(
        peer.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": {"name": "auth-test", "version": "1.0.0"}
            }),
        )
        .await,
    );
    let injected = peer
        .request(
            "session/new",
            json!({
                "cwd": "/tmp/kiteframe-auth",
                "mcpServers": [],
                "principal": "attacker"
            }),
        )
        .await;
    assert_eq!(injected["error"]["code"], -32602);
    let hostile_key = peer
        .request(
            "session/new",
            json!({
                "cwd": "/tmp/kiteframe-auth",
                "mcpServers": [{
                    "name": "sentinel-private-content",
                    "command": "/bin/false",
                    "args": [],
                    "env": []
                }]
            }),
        )
        .await;
    assert_eq!(hostile_key["error"]["code"], -32602, "{hostile_key}");
    assert!(!hostile_key.to_string().contains("sentinel-private-content"));
    let hostile_method = peer.request("sentinel-private-content", json!({})).await;
    assert_eq!(hostile_method["error"]["code"], -32601);
    assert!(
        !hostile_method
            .to_string()
            .contains("sentinel-private-content")
    );
    let no_session = peer
        .request("_kiteframe/run/get", json!({"runId": Uuid::now_v7()}))
        .await;
    assert_eq!(no_session["error"]["code"], -32000);
    let created = success(
        peer.request(
            "session/new",
            json!({"cwd": "/tmp/kiteframe-auth", "mcpServers": []}),
        )
        .await,
    );
    let session_id =
        SessionId::new(Uuid::parse_str(created["sessionId"].as_str().unwrap()).unwrap());
    assert_eq!(
        store
            .resume_session(session_id.clone(), &owner)
            .await
            .expect("owner session")
            .principal,
        owner
    );
    assert!(
        store
            .resume_session(session_id.clone(), &PrincipalId::new("attacker").unwrap(),)
            .await
            .is_err()
    );
    store
        .append_catalog_epoch(
            &ToolCatalogSnapshot::new(session_id.clone(), Uuid::now_v7(), [])
                .expect("empty catalog"),
        )
        .await
        .expect("append empty catalog");
    success(
        peer.request(
            "_kiteframe/workflow/publish",
            json!({
                "source": "workflow redaction() -> string { start t() } task t() -> string { prompt \"\"\"sentinel-private-content\"\"\" tools [] on ok(answer: string) { done answer } }"
            }),
        )
        .await,
    );
    peer.close().await;

    let mut draining_websocket =
        WebSocketPeer::connect(server.address, "sentinel-bearer-secret").await;
    success(
        draining_websocket
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "shutdown-test", "version": "1.0.0"}
                }),
            )
            .await,
    );
    model_ready.store(false, Ordering::Release);
    let unhealthy = draining_websocket
        .request(
            "session/new",
            json!({"cwd": "/tmp/kiteframe-model-unready", "mcpServers": []}),
        )
        .await;
    assert_eq!(unhealthy["error"]["code"], -32800);
    model_ready.store(true, Ordering::Release);
    remote.begin_shutdown();
    let stopped = draining_websocket
        .request(
            "session/new",
            json!({"cwd": "/tmp/kiteframe-after-shutdown", "mcpServers": []}),
        )
        .await;
    assert_eq!(stopped["error"]["code"], -32800);
    assert_eq!(
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    tokio::time::timeout(Duration::from_secs(1), server.stop())
        .await
        .expect("idle WebSocket must not block graceful shutdown");
    drop(draining_websocket);

    let logs = String::from_utf8(captured.lock().expect("log capture poisoned").clone())
        .expect("logs are UTF-8");
    assert!(logs.contains("ACP request handled"));
    assert!(!logs.contains("sentinel-bearer-secret"));
    assert!(!logs.contains("sentinel-private-content"));
}

#[test]
fn acp_service_is_an_sdk_agent_component() {
    fn assert_component<T: ConnectTo<Client>>() {}
    assert_component::<AcpService>();
}
