use std::{
    collections::BTreeMap,
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
    CatalogIdentity, DelegationAncestry, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, EvidenceReferences, InvocationId, InvocationOutcome, InvocationRequest,
    InvocationStatus, SessionRef, Sha256Digest, TaskRef, Timestamp, TraceContext,
};
use kiteframe_provider::{
    VerifiedHumanPrincipal, VerifiedProviderPrincipals, VerifiedWorkloadPrincipal,
};
use kiteframe_provider_http::{
    HttpErrorKind, ProviderHttpError, ProviderHttpServices, ProviderHttpState,
    ProviderPrincipalVerifier, ProviderRequestContext, ServerBindConfig, provider_router,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TRACEPARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";

#[tokio::test]
async fn exact_v1_routes_return_stable_native_contract_bodies() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()),
        allowing_principal_verifier(events),
    );

    let catalog_response = send(&app, request(Method::GET, "/v1/capability-catalog", None)).await;
    assert_eq!(catalog_response.status(), StatusCode::OK);
    assert_eq!(
        response_json(catalog_response).await,
        serde_json::to_value(catalog()).unwrap()
    );

    let admission_response = send(
        &app,
        request(
            Method::POST,
            "/v1/capability-admissions",
            Some(serde_json::to_value(admission_request()).unwrap()),
        ),
    )
    .await;
    assert_eq!(admission_response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(admission_response).await["diagnostics"][0]["code"],
        "KF-CAP-002"
    );

    let invocation_response = send(
        &app,
        request(
            Method::POST,
            "/v1/capability-invocations/cases.read",
            Some(serde_json::to_value(invocation_request()).unwrap()),
        ),
    )
    .await;
    assert_eq!(invocation_response.status(), StatusCode::OK);
    assert_eq!(
        response_json(invocation_response).await,
        json!({"status": "deferred", "invocation_id": "inv-1"})
    );

    let status_response = send(
        &app,
        request(Method::GET, "/v1/capability-invocations/inv-1", None),
    )
    .await;
    assert_eq!(status_response.status(), StatusCode::OK);
    assert_eq!(
        response_json(status_response).await,
        json!({"status": "pending", "invocation_id": "inv-1"})
    );

    let unknown = send(&app, request(Method::GET, "/v1/other", None)).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(unknown).await["diagnostics"][0]["code"],
        "KF-RUNTIME-002"
    );
}

#[tokio::test]
async fn catalog_etag_revalidation_returns_bodyless_304_without_client_fetch_result() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()),
        allowing_principal_verifier(events.clone()),
    );
    let first = send(&app, request(Method::GET, "/v1/capability-catalog", None)).await;
    let etag = first.headers()[header::ETAG].clone();

    let mut second_request = request(Method::GET, "/v1/capability-catalog", None);
    second_request
        .headers_mut()
        .insert(header::IF_NONE_MATCH, etag);
    let second = send(&app, second_request).await;

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
    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "catalog_200",
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "catalog_304",
        ]
    );
}

#[tokio::test]
async fn every_route_traces_then_authenticates_both_principals_before_service_logic() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()),
        allowing_principal_verifier(events.clone()),
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
            "/v1/capability-invocations/cases.read",
            Some(serde_json::to_value(invocation_request()).unwrap()),
        ),
        request(Method::GET, "/v1/capability-invocations/inv-1", None),
    ];
    for request in requests {
        let _ = send(&app, request).await;
    }

    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "catalog_200",
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "admit",
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "invoke",
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "status",
        ]
    );
}

#[tokio::test]
async fn credential_headers_are_visible_only_to_verifier_and_never_to_services() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let verifier = Arc::new(RecordingVerifier {
        events: events.clone(),
        observed_authorization: Arc::new(Mutex::new(Vec::new())),
    });
    let services = Arc::new(RecordingServices {
        events,
        observed_contexts: Arc::new(Mutex::new(Vec::new())),
    });
    let app = provider_router(ProviderHttpState::new(services.clone()), verifier.clone());
    let mut request = request(Method::GET, "/v1/capability-catalog", None);
    request.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer human-secret".parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("x-workload-token", "workload-secret".parse().unwrap());
    request
        .headers_mut()
        .insert(header::COOKIE, "session=cookie-secret".parse().unwrap());

    let response = send(&app, request).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        verifier.observed_authorization.lock().unwrap().as_slice(),
        ["Bearer human-secret", "Bearer human-secret"]
    );
    let contexts = services.observed_contexts.lock().unwrap();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].0, "human-7");
    assert_eq!(contexts[0].1, "workload-2");
}

#[tokio::test]
async fn independently_verified_human_and_workload_tenant_mismatch_denies_before_service() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()),
        Arc::new(MismatchedTenantVerifier {
            events: events.clone(),
        }),
    );

    let response = send(&app, request(Method::GET, "/v1/capability-catalog", None)).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["trace", "authenticate_human", "authenticate_workload"]
    );
}

#[tokio::test]
async fn trace_baggage_is_allowlisted_and_sensitive_names_are_rejected_before_auth() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()),
        allowing_principal_verifier(events.clone()),
    );
    let mut allowed = request(Method::GET, "/v1/capability-catalog", None);
    allowed.headers_mut().insert(
        "baggage",
        "kiteframe.request_id=0123456789abcdef0123456789abcdef,other=dropme"
            .parse()
            .unwrap(),
    );
    let allowed_response = send(&app, allowed).await;
    assert_eq!(allowed_response.status(), StatusCode::OK);
    assert_eq!(allowed_response.headers()["traceparent"], TRACEPARENT);

    let mut sensitive = request(Method::GET, "/v1/capability-catalog", None);
    sensitive.headers_mut().insert(
        "baggage",
        "authorization=0123456789abcdef0123456789abcdef"
            .parse()
            .unwrap(),
    );
    let sensitive_response = send(&app, sensitive).await;
    assert_eq!(sensitive_response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(sensitive_response).await["diagnostics"][0]["code"],
        "KF-PKG-001"
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "catalog_200",
        ]
    );
}

#[tokio::test]
async fn malformed_oversized_and_identity_mismatch_requests_are_stable_diagnostics() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()),
        allowing_principal_verifier(events),
    );
    let malformed = send(
        &app,
        request(
            Method::POST,
            "/v1/capability-admissions",
            Some(json!({"not": "native"})),
        ),
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert!(response_json(malformed).await.get("diagnostics").is_some());

    let mut oversized = request(
        Method::POST,
        "/v1/capability-admissions",
        Some(json!({"padding": "x".repeat(1_048_577)})),
    );
    oversized
        .headers_mut()
        .insert(header::CONTENT_LENGTH, "1048600".parse().unwrap());
    let oversized = send(&app, oversized).await;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(response_json(oversized).await.get("diagnostics").is_some());

    let mismatched = send(
        &app,
        request(
            Method::POST,
            "/v1/capability-invocations/other.read",
            Some(serde_json::to_value(invocation_request()).unwrap()),
        ),
    )
    .await;
    assert_eq!(mismatched.status(), StatusCode::FORBIDDEN);
}

#[test]
fn tls_and_origin_configuration_refuses_plaintext_non_loopback_and_redirect_origins() {
    assert!(ServerBindConfig::tls("0.0.0.0:8443", "cert.pem", "key.pem").is_ok());
    assert!(ServerBindConfig::insecure_loopback("127.0.0.1:8080").is_ok());
    assert!(ServerBindConfig::insecure_loopback("0.0.0.0:8080").is_err());
    assert!(ServerBindConfig::origin("https://provider.example").is_ok());
    assert!(ServerBindConfig::origin("http://127.0.0.1:8080").is_ok());
    assert!(ServerBindConfig::origin("http://provider.example").is_err());
    assert!(ServerBindConfig::origin("https://provider.example/redirect").is_err());
    assert!(ServerBindConfig::origin("https://provider.example?next=elsewhere").is_err());
}

#[derive(Clone)]
struct RecordingVerifier {
    events: Arc<Mutex<Vec<&'static str>>>,
    observed_authorization: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ProviderPrincipalVerifier for RecordingVerifier {
    fn observe_trace(&self, _trace_context: &TraceContext) {
        self.events.lock().unwrap().push("trace");
    }

    async fn verify_human(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedHumanPrincipal, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_human");
        if let Some(value) = headers.get(header::AUTHORIZATION) {
            self.observed_authorization
                .lock()
                .unwrap()
                .push(value.to_str().unwrap().to_owned());
        }
        Ok(verified_principals().into_parts().0)
    }

    async fn verify_workload(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedWorkloadPrincipal, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_workload");
        if let Some(value) = headers.get(header::AUTHORIZATION) {
            self.observed_authorization
                .lock()
                .unwrap()
                .push(value.to_str().unwrap().to_owned());
        }
        Ok(verified_principals().into_parts().1)
    }
}

#[derive(Clone)]
struct RecordingServices {
    events: Arc<Mutex<Vec<&'static str>>>,
    observed_contexts: Arc<Mutex<Vec<(String, String)>>>,
}

struct MismatchedTenantVerifier {
    events: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl ProviderPrincipalVerifier for MismatchedTenantVerifier {
    fn observe_trace(&self, _trace_context: &TraceContext) {
        self.events.lock().unwrap().push("trace");
    }

    async fn verify_human(
        &self,
        _headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedHumanPrincipal, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_human");
        Ok(VerifiedHumanPrincipal::try_new(
            "tenant-1",
            "human-7",
            ActorRef::new("actor-7").unwrap(),
            Timestamp::new(500),
        )
        .unwrap())
    }

    async fn verify_workload(
        &self,
        _headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedWorkloadPrincipal, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_workload");
        Ok(VerifiedWorkloadPrincipal::try_new(
            "tenant-other",
            "workload-2",
            "run-9",
            AgentRef::new("agent-2").unwrap(),
            TaskRef::new("task-4").unwrap(),
            SessionRef::new("session-3").unwrap(),
            AdmissionId::new("admission-5").unwrap(),
            Timestamp::new(500),
        )
        .unwrap())
    }
}

#[async_trait]
impl ProviderHttpServices for RecordingServices {
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
        self.observed_contexts.lock().unwrap().push((
            context.principals().human().human_ref().as_str().to_owned(),
            context
                .principals()
                .workload()
                .workload_ref()
                .as_str()
                .to_owned(),
        ));
        Ok(catalog())
    }

    async fn admit(
        &self,
        context: &ProviderRequestContext,
        _request: AdmissionRequest,
    ) -> Result<CapabilityGrantSet, ProviderHttpError> {
        self.observe(context, "admit");
        Err(ProviderHttpError::new(
            HttpErrorKind::Conflict,
            Diagnostic::error(
                DiagnosticCode::ResultInvalid,
                DiagnosticCategory::Capability,
                DiagnosticStage::Admit,
                "test admission conflict",
            ),
        ))
    }

    async fn invoke(
        &self,
        context: &ProviderRequestContext,
        _request: InvocationRequest,
    ) -> Result<InvocationOutcome, ProviderHttpError> {
        self.observe(context, "invoke");
        Ok(InvocationOutcome::Deferred {
            invocation_id: InvocationId::new("inv-1").unwrap(),
        })
    }

    async fn status(
        &self,
        context: &ProviderRequestContext,
        request: kiteframe_contract::StatusRequest,
    ) -> Result<InvocationStatus, ProviderHttpError> {
        self.observe(context, "status");
        Ok(InvocationStatus::Pending {
            invocation_id: request.invocation_id().clone(),
        })
    }
}

impl RecordingServices {
    fn observe(&self, context: &ProviderRequestContext, event: &'static str) {
        self.events.lock().unwrap().push(event);
        self.observed_contexts.lock().unwrap().push((
            context.principals().human().human_ref().as_str().to_owned(),
            context
                .principals()
                .workload()
                .workload_ref()
                .as_str()
                .to_owned(),
        ));
    }
}

fn test_services(events: Arc<Mutex<Vec<&'static str>>>) -> ProviderHttpState {
    ProviderHttpState::new(Arc::new(RecordingServices {
        events,
        observed_contexts: Arc::new(Mutex::new(Vec::new())),
    }))
}

fn allowing_principal_verifier(
    events: Arc<Mutex<Vec<&'static str>>>,
) -> Arc<dyn ProviderPrincipalVerifier> {
    Arc::new(RecordingVerifier {
        events,
        observed_authorization: Arc::new(Mutex::new(Vec::new())),
    })
}

fn request(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let body = body
        .map(|body| Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap_or_else(Body::empty);
    Request::builder()
        .method(method)
        .uri(uri)
        .header("traceparent", TRACEPARENT)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap()
}

async fn send(app: &axum::Router, request: Request<Body>) -> axum::http::Response<Body> {
    app.clone().oneshot(request).await.unwrap()
}

async fn response_json(response: axum::http::Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn catalog() -> CapabilityCatalog {
    CapabilityCatalog::try_new(
        CatalogIdentity {
            name: "test-provider".to_owned(),
            revision: "1".to_owned(),
        },
        Timestamp::new(1),
        None,
        vec![],
    )
    .unwrap()
}

fn admission_request() -> AdmissionRequest {
    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor-7").unwrap(),
        agent: AgentRef::new("agent-2").unwrap(),
        task: TaskRef::new("task-4").unwrap(),
        session: SessionRef::new("session-3").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        catalog_identity: catalog().identity().clone(),
        catalog_digest: *catalog().catalog_digest(),
        required_capabilities: vec![],
        optional_capabilities: vec![],
        resolved_requirements: vec![],
        delegation_ancestry: DelegationAncestry::try_new(vec![]).unwrap(),
        contextual_facts: BTreeMap::new(),
        trace_context: trace_context(TRACEPARENT),
    })
    .unwrap()
}

fn invocation_request() -> InvocationRequest {
    InvocationRequest::try_new(
        InvocationId::new("inv-1").unwrap(),
        AdmissionId::new("admission-5").unwrap(),
        digest(4),
        digest(5),
        capability_identity(),
        "case:42",
        json!({"caseId": "42"}),
        BTreeMap::new(),
        None,
        EvidenceReferences::try_new(BTreeMap::new()).unwrap(),
        trace_context(TRACEPARENT),
    )
    .unwrap()
}

fn verified_principals() -> VerifiedProviderPrincipals {
    VerifiedProviderPrincipals::new(
        VerifiedHumanPrincipal::try_new(
            "tenant-1",
            "human-7",
            ActorRef::new("actor-7").unwrap(),
            Timestamp::new(500),
        )
        .unwrap(),
        VerifiedWorkloadPrincipal::try_new(
            "tenant-1",
            "workload-2",
            "run-9",
            AgentRef::new("agent-2").unwrap(),
            TaskRef::new("task-4").unwrap(),
            SessionRef::new("session-3").unwrap(),
            AdmissionId::new("admission-5").unwrap(),
            Timestamp::new(500),
        )
        .unwrap(),
    )
}

fn trace_context(traceparent: &str) -> TraceContext {
    TraceContext::try_new(traceparent, None, BTreeMap::new()).unwrap()
}

fn capability_identity() -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.read").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}
