#![cfg(feature = "container-tests")]

use std::{
    process::{Command, Output},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const OPENFGA_IMAGE_ENV: &str = "KITEFRAME_OPENFGA_TEST_IMAGE";
const DEFAULT_OPENFGA_IMAGE: &str = "openfga/openfga:v1.15.0";

#[tokio::test]
async fn live_openfga_enforces_pinned_policy_lifecycle() {
    let container = OpenFgaContainer::start().await;
    let admin = OpenFgaAdmin::new(container.base_url());
    let store_id = admin.create_store().await;
    let model_1 = admin.write_model(&store_id).await;
    let tuples = stored_policy_tuples();
    admin.write_tuples(&store_id, &model_1, &tuples).await;

    let backend_1 = backend(container.base_url(), &store_id, &model_1);
    let revisions_1 = backend_1.revisions().await.unwrap();
    assert_eq!(
        revisions_1.entries(),
        [
            revision("openfga-model", &model_1),
            revision("openfga-store", &store_id),
            revision("tenant-policy", "tenant-policy-7"),
        ]
    );

    let admission = backend_1
        .list_admissible(&admission_request(
            principals(u64::MAX),
            revisions_1.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(admission.admissible(), &[identity()]);

    let allowed = backend_1
        .check(&invocation_request(
            principals(u64::MAX),
            revisions_1.clone(),
        ))
        .await
        .unwrap();
    assert!(matches!(allowed, AuthorizationDecision::Allow { .. }));

    for revoked in &tuples {
        admin.delete_tuple(&store_id, &model_1, revoked).await;
        let denied = backend_1
            .check(&invocation_request(
                principals(u64::MAX),
                revisions_1.clone(),
            ))
            .await
            .unwrap();
        assert!(
            matches!(denied, AuthorizationDecision::Deny { .. }),
            "removing stored policy tuple {revoked} must revoke point-of-use access"
        );

        admin
            .write_tuples(&store_id, &model_1, std::slice::from_ref(revoked))
            .await;
        let restored = backend_1
            .check(&invocation_request(
                principals(u64::MAX),
                revisions_1.clone(),
            ))
            .await
            .unwrap();
        assert!(
            matches!(restored, AuthorizationDecision::Allow { .. }),
            "restoring stored policy tuple {revoked} must restore the complete intersection"
        );
    }

    let expired = backend_1
        .check(&invocation_request(principals(2), revisions_1.clone()))
        .await
        .unwrap_err();
    assert_eq!(expired.code.as_str(), "KF-AUTH-004");

    let model_2 = admin.write_model(&store_id).await;
    assert_ne!(model_1, model_2);
    let backend_2 = backend(container.base_url(), &store_id, &model_2);
    let revisions_2 = backend_2.revisions().await.unwrap();
    let migrated = backend_2
        .check(&invocation_request(
            principals(u64::MAX),
            revisions_2.clone(),
        ))
        .await
        .unwrap();
    assert!(matches!(migrated, AuthorizationDecision::Allow { .. }));

    let stale = backend_2
        .check(&invocation_request(principals(u64::MAX), revisions_1))
        .await
        .unwrap_err();
    assert_eq!(stale.code.as_str(), "KF-AUTH-004");

    let unavailable = unavailable_backend()
        .check(&invocation_request(
            principals(u64::MAX),
            revision_set("store-unavailable", "model-unavailable"),
        ))
        .await
        .unwrap_err();
    assert_eq!(unavailable.code.as_str(), "KF-AUTH-004");
}

struct OpenFgaContainer {
    id: String,
    base_url: String,
}

impl OpenFgaContainer {
    async fn start() -> Self {
        let info = docker(&["info"]);
        assert!(
            info.status.success(),
            "Docker is required for container-tests: {}",
            String::from_utf8_lossy(&info.stderr)
        );

        let image =
            std::env::var(OPENFGA_IMAGE_ENV).unwrap_or_else(|_| DEFAULT_OPENFGA_IMAGE.to_owned());
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("kiteframe-openfga-{}-{unique}", std::process::id());
        let run = docker(&[
            "run",
            "--detach",
            "--rm",
            "--name",
            &name,
            "--publish",
            "127.0.0.1::8080",
            &image,
            "run",
        ]);
        assert!(
            run.status.success(),
            "failed to start {image}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let id = String::from_utf8(run.stdout).unwrap().trim().to_owned();
        let mut container = Self {
            id,
            base_url: String::new(),
        };
        let port = docker(&["port", &container.id, "8080/tcp"]);
        assert!(
            port.status.success(),
            "failed to resolve OpenFGA test port: {}",
            String::from_utf8_lossy(&port.stderr)
        );
        let binding = String::from_utf8(port.stdout).unwrap();
        let port = binding
            .trim()
            .rsplit_once(':')
            .map(|(_, port)| port)
            .expect("Docker port output must contain a host port");
        container.base_url = format!("http://127.0.0.1:{port}");
        container.wait_until_ready().await;
        container
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn wait_until_ready(&self) {
        let client = Client::new();
        for _ in 0..100 {
            if let Ok(response) = client
                .get(format!("{}/healthz", self.base_url))
                .send()
                .await
                && response.status() == StatusCode::OK
            {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("OpenFGA container did not become healthy");
    }
}

impl Drop for OpenFgaContainer {
    fn drop(&mut self) {
        let _ = docker(&["rm", "--force", &self.id]);
    }
}

fn docker(arguments: &[&str]) -> Output {
    Command::new("docker")
        .args(arguments)
        .output()
        .expect("Docker CLI must be installed for container-tests")
}

struct OpenFgaAdmin {
    base_url: String,
    client: Client,
}

impl OpenFgaAdmin {
    fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_owned(),
            client: Client::new(),
        }
    }

    async fn create_store(&self) -> String {
        let response = self
            .post("/stores", &json!({"name": "kiteframe-container-test"}))
            .await;
        string_field(&response, "id")
    }

    async fn write_model(&self, store_id: &str) -> String {
        let response = self
            .post(
                &format!("/stores/{store_id}/authorization-models"),
                &authorization_model(),
            )
            .await;
        string_field(&response, "authorization_model_id")
    }

    async fn write_tuples(&self, store_id: &str, model_id: &str, tuples: &[Value]) {
        self.post(
            &format!("/stores/{store_id}/write"),
            &json!({
                "authorization_model_id": model_id,
                "writes": {"tuple_keys": tuples},
            }),
        )
        .await;
    }

    async fn delete_tuple(&self, store_id: &str, model_id: &str, tuple: &Value) {
        self.post(
            &format!("/stores/{store_id}/write"),
            &json!({
                "authorization_model_id": model_id,
                "deletes": {
                    "tuple_keys": [tuple],
                    "on_missing": "error",
                },
            }),
        )
        .await;
    }

    async fn post(&self, path: &str, body: &Value) -> Value {
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        assert!(
            status.is_success(),
            "OpenFGA admin request {path} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        }
    }
}

fn authorization_model() -> Value {
    json!({
        "schema_version": "1.1",
        "type_definitions": [
            {"type": "actor"},
            {
                "type": "workload",
                "relations": {"actor": {"this": {}}},
                "metadata": {"relations": {
                    "actor": {"directly_related_user_types": [{"type": "actor"}]}
                }}
            },
            {
                "type": "task",
                "relations": {"actor": {"this": {}}},
                "metadata": {"relations": {
                    "actor": {"directly_related_user_types": [{"type": "actor"}]}
                }}
            },
            {
                "type": "agent",
                "relations": {
                    "assigned_task": {"this": {}},
                    "actor": {"tupleToUserset": {
                        "tupleset": {"relation": "assigned_task"},
                        "computedUserset": {"relation": "actor"}
                    }}
                },
                "metadata": {"relations": {
                    "assigned_task": {"directly_related_user_types": [{"type": "task"}]},
                    "actor": {}
                }}
            },
            {
                "type": "session",
                "relations": {
                    "task": {"this": {}},
                    "actor": {"tupleToUserset": {
                        "tupleset": {"relation": "task"},
                        "computedUserset": {"relation": "actor"}
                    }}
                },
                "metadata": {"relations": {
                    "task": {"directly_related_user_types": [{"type": "task"}]},
                    "actor": {}
                }}
            },
            {
                "type": "capability",
                "relations": {
                    "allowed_actor": {"this": {}},
                    "allowed_task_actor": {"this": {}},
                    "allowed_workload_actor": {"this": {}},
                    "can_invoke": {"intersection": {"child": [
                        {"computedUserset": {"relation": "allowed_actor"}},
                        {"computedUserset": {"relation": "allowed_task_actor"}},
                        {"computedUserset": {"relation": "allowed_workload_actor"}}
                    ]}}
                },
                "metadata": {"relations": {
                    "allowed_actor": {"directly_related_user_types": [{"type": "actor"}]},
                    "allowed_task_actor": {"directly_related_user_types": [
                        {"type": "task", "relation": "actor"},
                        {"type": "session", "relation": "actor"},
                        {"type": "agent", "relation": "actor"}
                    ]},
                    "allowed_workload_actor": {"directly_related_user_types": [
                        {"type": "workload", "relation": "actor"}
                    ]},
                    "can_invoke": {}
                }}
            },
            {
                "type": "resource",
                "relations": {
                    "capability": {"this": {}},
                    "can_invoke": {"tupleToUserset": {
                        "tupleset": {"relation": "capability"},
                        "computedUserset": {"relation": "can_invoke"}
                    }}
                },
                "metadata": {"relations": {
                    "capability": {"directly_related_user_types": [{"type": "capability"}]},
                    "can_invoke": {}
                }}
            }
        ]
    })
}

fn stored_policy_tuples() -> Vec<Value> {
    vec![
        tuple(&actor(), "actor", &task()),
        tuple(&actor(), "actor", &workload()),
        tuple(&actor(), "allowed_actor", &capability()),
        tuple(
            &format!("{}#actor", agent()),
            "allowed_task_actor",
            &capability(),
        ),
        tuple(
            &format!("{}#actor", workload()),
            "allowed_workload_actor",
            &capability(),
        ),
        tuple(&capability(), "capability", &resource_object()),
    ]
}

fn tuple(user: &str, relation: &str, object: &str) -> Value {
    json!({"user": user, "relation": relation, "object": object})
}

fn actor() -> String {
    typed("actor", "actor-1")
}

fn workload() -> String {
    scoped("workload", &["tenant-1", "workload-1"])
}

fn task() -> String {
    scoped("task", &["tenant-1", "task-1"])
}

fn agent() -> String {
    scoped("agent", &["tenant-1", "agent-1"])
}

fn capability() -> String {
    typed("capability", "cases.read@1.0.0")
}

fn resource_object() -> String {
    typed("resource", "tenant:t1/case:case-7")
}

fn scoped(object_type: &str, parts: &[&str]) -> String {
    typed(object_type, &parts.join("\0"))
}

fn typed(object_type: &str, value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}").unwrap();
    }
    format!("{object_type}:{encoded}")
}

fn string_field(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("OpenFGA response is missing {field}: {value}"))
        .to_owned()
}

fn backend(base_url: &str, store_id: &str, model_id: &str) -> OpenFgaAuthorizationBackend {
    let config = OpenFgaConfig::try_new(base_url, store_id, model_id, "tenant-policy-7")
        .unwrap()
        .with_request_timeout(Duration::from_secs(3))
        .unwrap();
    OpenFgaAuthorizationBackend::try_new(config).unwrap()
}

fn unavailable_backend() -> OpenFgaAuthorizationBackend {
    backend(
        "http://127.0.0.1:9",
        "store-unavailable",
        "model-unavailable",
    )
}

fn admission_request(
    principals: AuthenticatedInvocationContext,
    revisions: AuthorityRevisionSet,
) -> AdmissionAuthorizationRequest {
    AdmissionAuthorizationRequest::new(principals, identity(), resource(), revisions)
}

fn invocation_request(
    principals: AuthenticatedInvocationContext,
    revisions: AuthorityRevisionSet,
) -> InvocationAuthorizationRequest {
    InvocationAuthorizationRequest::new(
        principals,
        identity(),
        resource(),
        Sha256Digest::from_bytes([7; 32]),
        revisions,
    )
}

fn principals(expires_at: u64) -> AuthenticatedInvocationContext {
    let human = VerifiedHumanPrincipal::try_new(
        "tenant-1",
        "human-1",
        ActorRef::new("actor-1").unwrap(),
        Timestamp::new(expires_at),
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
        Timestamp::new(expires_at),
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

fn revision_set(store_id: &str, model_id: &str) -> AuthorityRevisionSet {
    AuthorityRevisionSet::try_new(vec![
        revision("openfga-model", model_id),
        revision("openfga-store", store_id),
        revision("tenant-policy", "tenant-policy-7"),
    ])
    .unwrap()
}

fn revision(source: &str, value: &str) -> AuthorityRevision {
    AuthorityRevision::try_new(source, value).unwrap()
}
