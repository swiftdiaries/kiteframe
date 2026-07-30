use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};

use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, AuthorityRevision, AuthorityRevisionSet, CapabilityIdentity,
    CapabilityName, CapabilityReleaseVersion, NormalizedResourceSelector, SessionRef, Sha256Digest,
    TaskRef, Timestamp,
};
use kiteframe_openfga::{OpenFgaAuthorizationBackend, OpenFgaConfig};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AuthenticatedInvocationContext, AuthorizationBackend,
    AuthorizationDecision, InvocationAuthorizationRequest, PortableInvocationRefs, RunRef,
    VerifiedHumanPrincipal, VerifiedWorkloadPrincipal, correlate_principals,
};
use serde_json::{Value, json};

#[tokio::test]
async fn admission_uses_list_objects_with_pinned_model() {
    let server = FakeOpenFga::json(
        200,
        json!({"objects": ["resource:74656e616e743a74312f636173653a636173652d37"]}),
    );
    let backend = backend_for(&server);

    let result = backend
        .list_admissible(&admission_auth_request(backend.revisions().await.unwrap()))
        .await
        .unwrap();

    assert_eq!(result.admissible(), &[identity()]);
    let request = server.last_request();
    assert_eq!(request.path, "/stores/store-1/list-objects");
    assert_eq!(request.json["authorization_model_id"], "model-1");
    assert_eq!(request.json["type"], "resource");
    assert_eq!(request.json["relation"], "can_invoke");
    assert_eq!(request.json["user"], "actor:6163746f722d31");
}

#[tokio::test]
async fn invocation_uses_higher_consistency_check() {
    let server = FakeOpenFga::json(200, json!({"allowed": true, "resolution": "allow"}));
    let backend = backend_for(&server);

    let decision = backend
        .check(&invocation_auth_request(backend.revisions().await.unwrap()))
        .await
        .unwrap();

    assert!(matches!(decision, AuthorizationDecision::Allow { .. }));
    let request = server.last_request();
    assert_eq!(request.path, "/stores/store-1/check");
    assert_eq!(request.json["authorization_model_id"], "model-1");
    assert_eq!(request.json["consistency"], "HIGHER_CONSISTENCY");
    assert_eq!(
        request.json["tuple_key"],
        json!({
            "user": "actor:6163746f722d31",
            "relation": "can_invoke",
            "object": "resource:74656e616e743a74312f636173653a636173652d37"
        })
    );
}

#[tokio::test]
async fn requests_bind_correlated_principals_and_ephemeral_context() {
    let server = FakeOpenFga::json(200, json!({"allowed": false, "resolution": "deny"}));
    let backend = backend_for(&server);

    backend
        .check(&invocation_auth_request(backend.revisions().await.unwrap()))
        .await
        .unwrap();

    let body = server.last_request().json;
    assert_eq!(body["context"]["tenant_ref"], "tenant-1");
    assert_eq!(body["context"]["human_ref"], "human-1");
    assert_eq!(body["context"]["workload_ref"], "workload-1");
    assert_eq!(body["context"]["run_ref"], "run-1");
    assert_eq!(body["context"]["task_ref"], "task-1");
    assert_eq!(body["context"]["agent_ref"], "agent-1");
    assert_eq!(body["context"]["session_ref"], "session-1");
    assert_eq!(body["context"]["admission_ref"], "admission-1");
    assert_eq!(
        body["context"]["grant_digest"],
        "0707070707070707070707070707070707070707070707070707070707070707"
    );
    assert!(body["context"]["current_timestamp"].as_u64().is_some());
    assert_eq!(
        body["contextual_tuples"]["tuple_keys"],
        json!([
            {
                "user": "task:74656e616e742d31007461736b2d31",
                "relation": "assigned_task",
                "object": "agent:74656e616e742d31006167656e742d31"
            },
            {
                "user": "task:74656e616e742d31007461736b2d31",
                "relation": "task",
                "object": "session:74656e616e742d310073657373696f6e2d31"
            }
        ])
    );
}

#[tokio::test]
async fn outage_fails_closed() {
    let backend = unavailable_backend();
    let revisions = backend.revisions().await.unwrap();

    let error = backend
        .check(&invocation_auth_request(revisions))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-004");
}

#[tokio::test]
async fn stale_revision_fails_closed_without_calling_openfga() {
    let server = FakeOpenFga::json(200, json!({"allowed": true}));
    let backend = backend_for(&server);

    let error = backend
        .check(&invocation_auth_request(revision_set(
            "different-model",
            &[],
        )))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-004");
    assert!(server.no_request_received());
}

#[tokio::test]
async fn revision_set_records_model_store_tenant_and_deployment_policy_sources() {
    let server = FakeOpenFga::json(200, json!({"allowed": true}));
    let backend = backend_for_with_deployment_policy(&server);

    let revisions = backend.revisions().await.unwrap();

    assert_eq!(
        revisions.entries(),
        [
            revision("deployment-policy", "deployment-policy-4"),
            revision("openfga-model", "model-1"),
            revision("openfga-store", "store-1"),
            revision("tenant-policy", "tenant-policy-7"),
        ]
    );
    assert_eq!(
        revisions.authority_revision_digest(),
        revision_set(
            "model-1",
            &[revision("deployment-policy", "deployment-policy-4")]
        )
        .authority_revision_digest()
    );
}

#[tokio::test]
async fn deny_is_a_safe_authorization_decision() {
    let server = FakeOpenFga::json(200, json!({"allowed": false, "resolution": "deny"}));
    let backend = backend_for(&server);

    let decision = backend
        .check(&invocation_auth_request(backend.revisions().await.unwrap()))
        .await
        .unwrap();

    assert!(matches!(decision, AuthorizationDecision::Deny { .. }));
}

#[tokio::test]
async fn oversized_or_malformed_responses_fail_closed() {
    let oversized = FakeOpenFga::raw(200, vec![b'x'; 513]);
    let backend = backend_for_with_max_body(&oversized, 512);
    let error = backend
        .check(&invocation_auth_request(backend.revisions().await.unwrap()))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-004");

    let malformed = FakeOpenFga::raw(200, b"{not-json".to_vec());
    let backend = backend_for(&malformed);
    let error = backend
        .check(&invocation_auth_request(backend.revisions().await.unwrap()))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-004");
}

fn backend_for(server: &FakeOpenFga) -> OpenFgaAuthorizationBackend {
    OpenFgaAuthorizationBackend::try_new(config_for(server)).unwrap()
}

fn backend_for_with_deployment_policy(server: &FakeOpenFga) -> OpenFgaAuthorizationBackend {
    let config = config_for(server)
        .with_deployment_policy_revision("deployment-policy", "deployment-policy-4")
        .unwrap();
    OpenFgaAuthorizationBackend::try_new(config).unwrap()
}

fn backend_for_with_max_body(
    server: &FakeOpenFga,
    max_response_bytes: usize,
) -> OpenFgaAuthorizationBackend {
    let config = config_for(server)
        .with_max_response_bytes(max_response_bytes)
        .unwrap();
    OpenFgaAuthorizationBackend::try_new(config).unwrap()
}

fn unavailable_backend() -> OpenFgaAuthorizationBackend {
    let config = OpenFgaConfig::try_new(
        "http://127.0.0.1:9",
        "store-1",
        "model-1",
        "tenant-policy-7",
    )
    .unwrap()
    .with_request_timeout(Duration::from_millis(100))
    .unwrap();
    OpenFgaAuthorizationBackend::try_new(config).unwrap()
}

fn config_for(server: &FakeOpenFga) -> OpenFgaConfig {
    OpenFgaConfig::try_new(server.base_url(), "store-1", "model-1", "tenant-policy-7")
        .unwrap()
        .with_request_timeout(Duration::from_secs(2))
        .unwrap()
}

fn admission_auth_request(revisions: AuthorityRevisionSet) -> AdmissionAuthorizationRequest {
    AdmissionAuthorizationRequest::new(principals(), identity(), resource(), revisions)
}

fn invocation_auth_request(revisions: AuthorityRevisionSet) -> InvocationAuthorizationRequest {
    InvocationAuthorizationRequest::new(
        principals(),
        identity(),
        resource(),
        Sha256Digest::from_bytes([7; 32]),
        revisions,
    )
}

fn principals() -> AuthenticatedInvocationContext {
    let human = VerifiedHumanPrincipal::try_new(
        "tenant-1",
        "human-1",
        ActorRef::new("actor-1").unwrap(),
        Timestamp::new(u64::MAX),
    )
    .unwrap();
    let workload = VerifiedWorkloadPrincipal::try_new(
        "tenant-1",
        "workload-1",
        "run-1",
        AgentRef::new("agent-1").unwrap(),
        TaskRef::new("task-1").unwrap(),
        SessionRef::new("session-1").unwrap(),
        AdmissionId::new("admission-1").unwrap(),
        Timestamp::new(u64::MAX),
    )
    .unwrap();
    correlate_principals(
        human,
        workload,
        PortableInvocationRefs::new(
            ActorRef::new("actor-1").unwrap(),
            AgentRef::new("agent-1").unwrap(),
            RunRef::new("run-1").unwrap(),
            TaskRef::new("task-1").unwrap(),
            SessionRef::new("session-1").unwrap(),
            AdmissionId::new("admission-1").unwrap(),
            Timestamp::new(1),
        ),
    )
    .unwrap()
}

fn identity() -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.read").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn resource() -> NormalizedResourceSelector {
    NormalizedResourceSelector::new("tenant:t1/case:case-7").unwrap()
}

fn revision_set(model_revision: &str, deployment: &[AuthorityRevision]) -> AuthorityRevisionSet {
    let mut entries = vec![
        revision("openfga-model", model_revision),
        revision("openfga-store", "store-1"),
        revision("tenant-policy", "tenant-policy-7"),
    ];
    entries.extend_from_slice(deployment);
    AuthorityRevisionSet::try_new(entries).unwrap()
}

fn revision(source: &str, value: &str) -> AuthorityRevision {
    AuthorityRevision::try_new(source, value).unwrap()
}

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    json: Value,
}

struct FakeOpenFga {
    address: String,
    requests: mpsc::Receiver<CapturedRequest>,
}

impl FakeOpenFga {
    fn json(status: u16, body: Value) -> Self {
        Self::raw(status, serde_json::to_vec(&body).unwrap())
    }

    fn raw(status: u16, body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (sender, requests) = mpsc::channel();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end;
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(end) = find_header_end(&request) {
                    header_end = end;
                    let content_length = parse_content_length(&request[..end]);
                    while request.len() < end + 4 + content_length {
                        let read = stream.read(&mut chunk).unwrap();
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..read]);
                    }
                    break;
                }
            }
            let request_line = String::from_utf8_lossy(&request[..header_end]);
            let path = request_line
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap()
                .to_owned();
            let json = serde_json::from_slice(&request[header_end + 4..]).unwrap();
            sender.send(CapturedRequest { path, json }).unwrap();
            let reason = if status == 200 { "OK" } else { "Error" };
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        Self { address, requests }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn last_request(&self) -> CapturedRequest {
        self.requests.recv_timeout(Duration::from_secs(2)).unwrap()
    }

    fn no_request_received(&self) -> bool {
        self.requests
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    }
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap()
}
