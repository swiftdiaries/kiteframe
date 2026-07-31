use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, CapabilityCatalog,
    CapabilityGrantSet, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    DelegationAncestry, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage,
    EvidenceReferences, InvocationId, InvocationOutcome, InvocationRequest, InvocationStatus,
    SessionRef, Sha256Digest, TaskRef, Timestamp, TraceContext,
};
use kiteframe_provider::{
    InMemoryInvocationStore, VerifiedHumanPrincipal, VerifiedWorkloadPrincipal,
};
use kiteframe_provider_http::{
    AuthenticatedStatusRequest, HttpErrorKind, ProviderHttpError, ProviderHttpServices,
    ProviderHttpState, ProviderPrincipalVerifier, ProviderRequestContext,
    VerifiedHumanAuthentication, VerifiedWorkloadAuthentication, provider_router,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;

const TRACEPARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
const HUMAN_CREDENTIAL: &str = "Bearer human-workforce-secret";
const WORKLOAD_CREDENTIAL: &str = "workload-workforce-secret";

#[tokio::test]
async fn workforce_profile_authenticates_every_route_and_never_returns_credentials() {
    let catalog = load_catalog();
    let events = Arc::new(Mutex::new(Vec::new()));
    let state = ProviderHttpState::new(
        Arc::new(WorkforceServices {
            catalog,
            events: Arc::clone(&events),
        }),
        Arc::new(InMemoryInvocationStore::new()),
    );
    let app = provider_router(
        state,
        Arc::new(WorkforceVerifier {
            events: Arc::clone(&events),
        }),
    );

    let requests = [
        request(Method::GET, "/v1/capability-catalog", None),
        request(
            Method::POST,
            "/v1/capability-admissions",
            Some(serde_json::to_value(admission_request()).unwrap()),
        ),
        request(
            Method::POST,
            "/v1/capability-invocations/workforce.absence.propose",
            Some(serde_json::to_value(invocation_request()).unwrap()),
        ),
        request(
            Method::GET,
            "/v1/capability-invocations/inv-absence-1",
            None,
        ),
    ];
    let mut response_bodies = Vec::new();
    for request in requests {
        let response = app.clone().oneshot(request).await.unwrap();
        response_bodies
            .extend_from_slice(&response.into_body().collect().await.unwrap().to_bytes());
    }

    let events = events.lock().unwrap();
    assert_eq!(events.iter().filter(|event| **event == "trace").count(), 4);
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "authenticate_human")
            .count(),
        4
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "authenticate_workload")
            .count(),
        4
    );
    assert!(events.iter().any(|event| *event == "catalog"));
    assert!(events.iter().any(|event| *event == "admit"));
    assert!(events.iter().any(|event| *event == "invoke"));
    let response_text = String::from_utf8(response_bodies).unwrap();
    assert!(!response_text.contains(HUMAN_CREDENTIAL));
    assert!(!response_text.contains(WORKLOAD_CREDENTIAL));
    assert!(!response_text.contains("x-workload-token"));
    assert!(!response_text.contains("Bearer "));
}

#[tokio::test]
async fn workforce_catalog_etag_is_bodyless_304_with_stable_contract_metadata() {
    let catalog = load_catalog();
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        ProviderHttpState::new(
            Arc::new(WorkforceServices {
                catalog,
                events: Arc::clone(&events),
            }),
            Arc::new(InMemoryInvocationStore::new()),
        ),
        Arc::new(WorkforceVerifier { events }),
    );

    let first = app
        .clone()
        .oneshot(request(Method::GET, "/v1/capability-catalog", None))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()[header::ETAG].clone();
    let first_body = first.into_body().collect().await.unwrap().to_bytes();
    let catalog_json: Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(catalog_json["descriptors"].as_array().unwrap().len(), 2);
    assert!(
        catalog_json["descriptors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|descriptor| descriptor["stableErrors"].as_array().unwrap().len() >= 2)
    );

    let mut revalidation = request(Method::GET, "/v1/capability-catalog", None);
    revalidation
        .headers_mut()
        .insert(header::IF_NONE_MATCH, etag);
    let second = app.clone().oneshot(revalidation).await.unwrap();
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert!(
        second
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

struct WorkforceServices {
    catalog: CapabilityCatalog,
    events: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl ProviderHttpServices for WorkforceServices {
    fn observe_catalog_response(&self, not_modified: bool) {
        self.events.lock().unwrap().push(if not_modified {
            "catalog_304"
        } else {
            "catalog_200"
        });
    }

    async fn catalog(
        &self,
        context: &ProviderRequestContext,
    ) -> Result<CapabilityCatalog, ProviderHttpError> {
        self.observe(context, "catalog");
        Ok(self.catalog.clone())
    }

    async fn admit(
        &self,
        context: &ProviderRequestContext,
        _request: AdmissionRequest,
    ) -> Result<CapabilityGrantSet, ProviderHttpError> {
        self.observe(context, "admit");
        Err(profile_denial())
    }

    async fn invoke(
        &self,
        context: &ProviderRequestContext,
        _request: InvocationRequest,
    ) -> Result<InvocationOutcome, ProviderHttpError> {
        self.observe(context, "invoke");
        Err(profile_denial())
    }

    async fn status(
        &self,
        request: AuthenticatedStatusRequest,
    ) -> Result<InvocationStatus, ProviderHttpError> {
        self.observe(request.context(), "status");
        Err(profile_denial())
    }
}

impl WorkforceServices {
    fn observe(&self, context: &ProviderRequestContext, event: &'static str) {
        assert_eq!(
            context.principals().human().human_ref().as_str(),
            "employee-7"
        );
        assert_eq!(
            context.principals().workload().workload_ref().as_str(),
            "workforce-harness-2"
        );
        assert_eq!(context.trace_context().traceparent(), TRACEPARENT);
        self.events.lock().unwrap().push(event);
    }
}

struct WorkforceVerifier {
    events: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl ProviderPrincipalVerifier for WorkforceVerifier {
    fn observe_trace(&self, trace_context: &TraceContext) {
        assert_eq!(trace_context.traceparent(), TRACEPARENT);
        self.events.lock().unwrap().push("trace");
    }

    async fn verify_human(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedHumanAuthentication, Diagnostic> {
        assert_eq!(
            headers[header::AUTHORIZATION].to_str().unwrap(),
            HUMAN_CREDENTIAL
        );
        self.events.lock().unwrap().push("authenticate_human");
        Ok(VerifiedHumanAuthentication::new(
            VerifiedHumanPrincipal::try_new(
                "tenant-1",
                "employee-7",
                ActorRef::new("employee-7").unwrap(),
                Timestamp::new(900),
            )
            .unwrap(),
            [header::AUTHORIZATION],
        ))
    }

    async fn verify_workload(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedWorkloadAuthentication, Diagnostic> {
        assert_eq!(
            headers["x-workload-token"].to_str().unwrap(),
            WORKLOAD_CREDENTIAL
        );
        self.events.lock().unwrap().push("authenticate_workload");
        Ok(VerifiedWorkloadAuthentication::new(
            VerifiedWorkloadPrincipal::try_new(
                "tenant-1",
                "workforce-harness-2",
                "run-9",
                AgentRef::new("workforce-agent-2").unwrap(),
                TaskRef::new("absence-task-4").unwrap(),
                SessionRef::new("absence-session-3").unwrap(),
                AdmissionId::new("admission-workforce-1").unwrap(),
                Timestamp::new(900),
            )
            .unwrap(),
            ["x-workload-token".parse().unwrap()],
        ))
    }
}

fn profile_denial() -> ProviderHttpError {
    ProviderHttpError::new(
        HttpErrorKind::IdentityMismatch,
        Diagnostic::error(
            DiagnosticCode::InvocationDenied,
            DiagnosticCategory::Authorization,
            DiagnosticStage::Invoke,
            "workforce profile denies this conformance-only service call",
        ),
    )
}

fn request(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let body = body
        .map(|value| Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap_or_else(Body::empty);
    Request::builder()
        .method(method)
        .uri(uri)
        .header("traceparent", TRACEPARENT)
        .header(header::AUTHORIZATION, HUMAN_CREDENTIAL)
        .header("x-workload-token", WORKLOAD_CREDENTIAL)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap()
}

fn admission_request() -> AdmissionRequest {
    let catalog = load_catalog();
    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("employee-7").unwrap(),
        agent: AgentRef::new("workforce-agent-2").unwrap(),
        task: TaskRef::new("absence-task-4").unwrap(),
        session: SessionRef::new("absence-session-3").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        catalog_identity: catalog.identity().clone(),
        catalog_digest: *catalog.catalog_digest(),
        required_capabilities: vec![],
        optional_capabilities: vec![],
        resolved_requirements: vec![],
        delegation_ancestry: DelegationAncestry::try_new(vec![]).unwrap(),
        contextual_facts: BTreeMap::new(),
        trace_context: trace_context(),
    })
    .unwrap()
}

fn invocation_request() -> InvocationRequest {
    InvocationRequest::try_new(
        InvocationId::new("inv-absence-1").unwrap(),
        AdmissionId::new("admission-workforce-1").unwrap(),
        digest(4),
        digest(5),
        capability("workforce.absence.propose"),
        "tenant:tenant-1/employee:employee-7",
        json!({"employeeId": "employee-7", "startDate": "2026-08-01"}),
        BTreeMap::new(),
        Some("absence-proposal-1".to_owned()),
        EvidenceReferences::try_new(BTreeMap::new()).unwrap(),
        trace_context(),
    )
    .unwrap()
}

fn load_catalog() -> CapabilityCatalog {
    load_fixture("catalog.json")
}

fn load_fixture<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = fixture_dir().join(name);
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/provider/fixtures/crankshaft-profile")
}

fn capability(name: &str) -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new(name).unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn trace_context() -> TraceContext {
    TraceContext::try_new(TRACEPARENT, None, BTreeMap::new()).unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}
