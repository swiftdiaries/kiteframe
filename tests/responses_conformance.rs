mod support;

use std::time::Duration;

use kiteframe::{
    ModelErrorKind, ModelRequest, ModelResponse, ModelResponseItemKind, ResponsesClient,
    ResponsesConfig, Secret,
};
use serde_json::json;
use support::ResponsesServer;

#[tokio::test]
async fn client_replays_full_ordered_history_and_preserves_unknown_output_items() {
    let unknown = json!({"type": "provider_extension", "opaque": {"ordinal": 3}});
    let server = ResponsesServer::start(json!({
        "output": [
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ready"}]},
            unknown
        ]
    }))
    .await;
    let client = ResponsesClient::new(ResponsesConfig::new(
        server.base_url(),
        Secret::new("test-model-secret"),
        "test-model",
        Duration::from_secs(2),
        4096,
    ))
    .expect("valid configuration");

    let response = client
        .respond(ModelRequest {
            instructions: "Complete the task.".to_owned(),
            input: vec![
                json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "first"}]}),
                json!({"type": "function_call", "call_id": "call_1", "name": "docs/search", "arguments": "{\"query\":\"rust\"}"}),
                json!({"type": "function_call_output", "call_id": "call_1", "output": "result"}),
            ],
            tools: vec![json!({"type": "function", "name": "docs/search", "strict": true, "parameters": {"type": "object", "additionalProperties": false}})],
        })
        .await
        .expect("model response");

    let request = server.one_request().await;
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/responses");
    assert_eq!(
        request.body,
        json!({
            "model": "test-model",
            "instructions": "Complete the task.",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "first"}]},
                {"type": "function_call", "call_id": "call_1", "name": "docs/search", "arguments": "{\"query\":\"rust\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "result"}
            ],
            "tools": [{"type": "function", "name": "docs/search", "strict": true, "parameters": {"type": "object", "additionalProperties": false}}],
            "tool_choice": "auto",
            "store": false
        })
    );
    assert_eq!(response.items[1], unknown);
    assert_eq!(
        ModelResponse::item_kind(&response.items[0]),
        ModelResponseItemKind::Message
    );
    assert_eq!(
        ModelResponse::item_kind(&response.items[1]),
        ModelResponseItemKind::Unknown
    );
}

#[test]
fn client_rejects_unsafe_or_unbounded_configuration() {
    for base_url in [
        "not a url",
        "ftp://provider/v1",
        "https://key@provider/v1",
        "https://provider/",
    ] {
        let result = ResponsesClient::new(ResponsesConfig::new(
            base_url,
            Secret::new("secret"),
            "model",
            Duration::from_secs(1),
            1,
        ));
        assert_eq!(
            result.expect_err("configuration must be rejected").kind(),
            ModelErrorKind::Configuration
        );
    }
    let result = ResponsesClient::new(ResponsesConfig::new(
        "https://provider/v1",
        Secret::new("secret"),
        "model",
        Duration::ZERO,
        1,
    ));
    assert_eq!(
        result.expect_err("zero timeout must be rejected").kind(),
        ModelErrorKind::Configuration
    );
}

#[tokio::test]
async fn client_rejects_incomplete_and_malformed_output() {
    for response in [
        json!({"status": "incomplete", "output": []}),
        json!({"output": ["not an item"]}),
        json!({"output": [{"type": "function_call", "call_id": "call_1", "name": "tool", "arguments": "not json"}]}),
        json!({"output": [
            {"type": "function_call", "call_id": "call_1", "name": "tool", "arguments": "{}"},
            {"type": "function_call", "call_id": "call_1", "name": "tool", "arguments": "{}"}
        ]}),
    ] {
        let server = ResponsesServer::start(response).await;
        let client = client(&server);
        let error = client
            .respond(empty_request())
            .await
            .expect_err("invalid output");
        assert_eq!(error.kind(), ModelErrorKind::Protocol);
    }
}

#[tokio::test]
async fn client_classifies_rate_limits_and_server_errors_as_transient() {
    let rate_limited = ResponsesServer::start_with_status(429, json!({"error": "slow down"})).await;
    let error = client(&rate_limited)
        .respond(empty_request())
        .await
        .expect_err("rate limit");
    assert_eq!(error.kind(), ModelErrorKind::RateLimited);

    let unavailable = ResponsesServer::start_with_status(503, json!({"error": "down"})).await;
    let error = client(&unavailable)
        .respond(empty_request())
        .await
        .expect_err("server failure");
    assert_eq!(error.kind(), ModelErrorKind::Unavailable);
}

fn client(server: &ResponsesServer) -> ResponsesClient {
    ResponsesClient::new(ResponsesConfig::new(
        server.base_url(),
        Secret::new("test-model-secret"),
        "test-model",
        Duration::from_secs(2),
        4096,
    ))
    .expect("valid configuration")
}

fn empty_request() -> ModelRequest {
    ModelRequest {
        instructions: "test".to_owned(),
        input: Vec::new(),
        tools: Vec::new(),
    }
}
