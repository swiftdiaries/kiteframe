use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    CapabilityCatalog, CapabilityDescriptor, CapabilityDescriptorParts, CapabilityGrantSet,
    CapabilityIdentity, CapabilityName, CapabilityReleaseVersion, CatalogIdentity,
    ConfirmationRequirement, ConsentRequirement, DelegationAncestry, DelegationEdge, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, EffectClassification, EvidenceReferences,
    ExecutionMode, FreshnessRequirement, IdempotencyKey, IdempotencyRequirement, IdempotencyScope,
    InvocationId, InvocationOutcome, InvocationRequest, InvocationStatus, NonEmptySet,
    NormalizedResourceSelector, ResourceSelectorSchema, RetryClass, SessionRef, Sha256Digest,
    SourceRange, TaskRef, Timestamp, TraceContext,
};
use kiteframe_provider::{
    IdempotencyScopeValue, InMemoryInvocationStore, InvocationReservationInput,
    InvocationStatusContext, InvocationStore, VerifiedHumanPrincipal, VerifiedProviderPrincipals,
    VerifiedWorkloadPrincipal,
};
use kiteframe_provider_http::{
    AuthenticatedStatusRequest, HttpErrorKind, ProviderHttpError, ProviderHttpServices,
    ProviderHttpState, ProviderPrincipalVerifier, ProviderRequestContext, ServerBindConfig,
    VerifiedHumanAuthentication, VerifiedWorkloadAuthentication, provider_router,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TRACEPARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";

#[tokio::test]
async fn exact_v1_routes_return_stable_native_contract_bodies() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
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
    assert_eq!(invocation_response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(invocation_response).await["diagnostics"][0]["code"],
        "KF-AUTH-003"
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
        test_services(events.clone()).await,
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
        test_services(events.clone()).await,
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
        observed_traces: Arc::new(Mutex::new(Vec::new())),
    });
    let app = provider_router(
        ProviderHttpState::new(services.clone(), Arc::new(InMemoryInvocationStore::new())),
        verifier.clone(),
    );
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
    request
        .headers_mut()
        .insert("x-signature", "signature-opaque".parse().unwrap());
    request
        .headers_mut()
        .insert("x-jwt", "jwt-opaque".parse().unwrap());
    request
        .headers_mut()
        .insert("client-assertion", "assertion-opaque".parse().unwrap());

    let response = send(&app, request).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        verifier.observed_authorization.lock().unwrap().as_slice(),
        [
            "Bearer human-secret",
            "session=cookie-secret",
            "signature-opaque",
            "assertion-opaque",
            "workload-secret",
            "jwt-opaque",
        ]
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
        test_services(events.clone()).await,
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
async fn status_request_for_another_invocation_is_denied_with_full_principal_context() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services_with_foreign_invocation(events.clone()).await,
        allowing_principal_verifier(events.clone()),
    );

    let response = send(
        &app,
        request(Method::GET, "/v1/capability-invocations/inv-other", None),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["trace", "authenticate_human", "authenticate_workload"]
    );
}

#[tokio::test]
async fn opaque_sensitive_diagnostic_fields_are_default_deny_projected() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        ProviderHttpState::new(
            Arc::new(AdversarialDiagnosticServices),
            Arc::new(InMemoryInvocationStore::new()),
        ),
        allowing_principal_verifier(events),
    );

    let response = send(&app, request(Method::GET, "/v1/capability-catalog", None)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    let diagnostic = &body["diagnostics"][0];
    assert_eq!(diagnostic["code"], "KF-RUNTIME-002");
    assert_eq!(diagnostic["category"], "runtime");
    assert_eq!(diagnostic["stage"], "runtime");
    assert_eq!(diagnostic["retry"], "after_refresh");
    assert_eq!(diagnostic["message"], "provider request failed");
    assert!(diagnostic["package_path"].is_null());
    assert!(diagnostic["source_range"].is_null());
    assert!(diagnostic["help"].is_null());
    assert_eq!(diagnostic["details"], json!({}));
    let wire = body.to_string();
    for opaque in [
        "eyJhbGciOiJub25lIn0.opaque.signature",
        "f43e9b7a1d0c4e8f",
        "urn:private:payload:7c2a",
        "blob-4d9190c2",
    ] {
        assert!(!wire.contains(opaque));
    }
}

#[tokio::test]
async fn trace_baggage_is_allowlisted_and_sensitive_names_are_rejected_before_auth() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
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
async fn repeated_tracestate_and_baggage_headers_are_combined() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let services = Arc::new(RecordingServices {
        events: events.clone(),
        observed_contexts: Arc::new(Mutex::new(Vec::new())),
        observed_traces: Arc::new(Mutex::new(Vec::new())),
    });
    let app = provider_router(
        ProviderHttpState::new(services.clone(), Arc::new(InMemoryInvocationStore::new())),
        allowing_principal_verifier(events),
    );
    let mut request = request(Method::GET, "/v1/capability-catalog", None);
    request
        .headers_mut()
        .append("tracestate", "vendor1=value1".parse().unwrap());
    request
        .headers_mut()
        .append("tracestate", "vendor2=value2".parse().unwrap());
    request.headers_mut().append(
        "baggage",
        "kiteframe.request_id=0123456789abcdef0123456789abcdef"
            .parse()
            .unwrap(),
    );
    request.headers_mut().append(
        "baggage",
        "kiteframe.task_id=1123456789abcdef0123456789abcdef"
            .parse()
            .unwrap(),
    );

    assert_eq!(send(&app, request).await.status(), StatusCode::OK);
    let traces = services.observed_traces.lock().unwrap();
    let trace = &traces[0];
    assert_eq!(trace.tracestate(), Some("vendor1=value1,vendor2=value2"));
    assert_eq!(
        trace.baggage()["kiteframe.request_id"].as_str(),
        "0123456789abcdef0123456789abcdef"
    );
    assert_eq!(
        trace.baggage()["kiteframe.task_id"].as_str(),
        "1123456789abcdef0123456789abcdef"
    );
}

#[tokio::test]
async fn malformed_oversized_and_identity_mismatch_requests_are_stable_diagnostics() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
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

#[tokio::test]
async fn oversized_bodies_are_rejected_on_catalog_and_status_routes() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
        allowing_principal_verifier(events),
    );
    for uri in ["/v1/capability-catalog", "/v1/capability-invocations/inv-1"] {
        let mut oversized = request(
            Method::GET,
            uri,
            Some(json!({"padding": "x".repeat(1_048_577)})),
        );
        oversized
            .headers_mut()
            .insert(header::CONTENT_LENGTH, "1048600".parse().unwrap());
        let response = send(&app, oversized).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response_json(response).await["diagnostics"][0]["message"],
            "provider request failed"
        );
    }
}

#[tokio::test]
async fn chunked_or_undeclared_get_bodies_are_rejected_before_route_logic() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
        allowing_principal_verifier(events.clone()),
    );

    let mut chunked = request(
        Method::GET,
        "/v1/capability-catalog",
        Some(json!({"small": true})),
    );
    chunked
        .headers_mut()
        .insert(header::TRANSFER_ENCODING, "chunked".parse().unwrap());
    let response = send(&app, chunked).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let undeclared = request(
        Method::GET,
        "/v1/capability-invocations/inv-1",
        Some(json!({"small": true})),
    );
    let response = send(&app, undeclared).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "trace",
            "authenticate_human",
            "authenticate_workload",
        ]
    );
}

#[tokio::test]
async fn only_the_four_declared_method_path_contracts_are_accepted() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
        allowing_principal_verifier(events),
    );
    for (method, uri) in [
        (Method::HEAD, "/v1/capability-catalog"),
        (Method::POST, "/v1/capability-catalog"),
        (Method::GET, "/v1/capability-admissions"),
        (Method::PUT, "/v1/capability-admissions"),
        (Method::HEAD, "/v1/capability-invocations/inv-1"),
        (Method::DELETE, "/v1/capability-invocations/inv-1"),
    ] {
        let is_head = method == Method::HEAD;
        let response = send(&app, request(method, uri, None)).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        if is_head {
            assert!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
        } else {
            assert!(response_json(response).await.get("diagnostics").is_some());
        }
    }
}

#[tokio::test]
async fn admission_rejects_delegation_ancestry_whose_leaf_is_not_verified_agent() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
        allowing_principal_verifier(events.clone()),
    );
    let response = send(
        &app,
        request(
            Method::POST,
            "/v1/capability-admissions",
            Some(serde_json::to_value(admission_request_with_leaf("agent-other")).unwrap()),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["trace", "authenticate_human", "authenticate_workload"]
    );
}

#[tokio::test]
async fn admission_accepts_only_when_every_nested_delegation_agent_is_verified() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app = provider_router(
        test_services(events.clone()).await,
        allowing_principal_verifier(events.clone()),
    );
    let response = send(
        &app,
        request(
            Method::POST,
            "/v1/capability-admissions",
            Some(serde_json::to_value(admission_request_with_leaf("agent-2")).unwrap()),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            "trace",
            "authenticate_human",
            "authenticate_workload",
            "admit",
        ]
    );
}

#[tokio::test]
async fn running_router_rejects_mismatched_origin_and_redirect_forwarding_hints() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let state = test_services(events.clone())
        .await
        .with_origin("https://provider.example")
        .unwrap();
    let app = provider_router(state, allowing_principal_verifier(events));

    let mut mismatched = request(Method::GET, "/v1/capability-catalog", None);
    mismatched
        .headers_mut()
        .insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
    let response = send(&app, mismatched).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let mut forwarded = request(Method::GET, "/v1/capability-catalog", None);
    forwarded
        .headers_mut()
        .insert("x-forwarded-host", "redirect.example".parse().unwrap());
    let response = send(&app, forwarded).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let mut mismatched_host = request(Method::GET, "/v1/capability-catalog", None);
    mismatched_host
        .headers_mut()
        .insert(header::HOST, "attacker.example".parse().unwrap());
    let response = send(&app, mismatched_host).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let mut matching = request(Method::GET, "/v1/capability-catalog", None);
    matching
        .headers_mut()
        .insert(header::ORIGIN, "https://provider.example".parse().unwrap());
    matching
        .headers_mut()
        .insert(header::HOST, "provider.example".parse().unwrap());
    assert_eq!(send(&app, matching).await.status(), StatusCode::OK);
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
    let config = ServerBindConfig::tls("0.0.0.0:8443", "cert.pem", "key.pem")
        .unwrap()
        .with_origin("https://provider.example")
        .unwrap();
    assert_eq!(config.origin_url().as_str(), "https://provider.example/");
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
    ) -> Result<VerifiedHumanAuthentication, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_human");
        for name in [
            header::AUTHORIZATION.as_str(),
            header::COOKIE.as_str(),
            "x-signature",
            "client-assertion",
        ] {
            if let Some(value) = headers.get(name) {
                self.observed_authorization
                    .lock()
                    .unwrap()
                    .push(value.to_str().unwrap().to_owned());
            }
        }
        Ok(VerifiedHumanAuthentication::new(
            verified_principals().into_parts().0,
            [
                header::AUTHORIZATION,
                header::COOKIE,
                "x-signature".parse().unwrap(),
                "client-assertion".parse().unwrap(),
            ],
        ))
    }

    async fn verify_workload(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedWorkloadAuthentication, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_workload");
        for name in ["x-workload-token", "x-jwt"] {
            if let Some(value) = headers.get(name) {
                self.observed_authorization
                    .lock()
                    .unwrap()
                    .push(value.to_str().unwrap().to_owned());
            }
        }
        Ok(VerifiedWorkloadAuthentication::new(
            verified_principals().into_parts().1,
            [
                "x-workload-token".parse().unwrap(),
                "x-jwt".parse().unwrap(),
            ],
        )
        .with_delegation_agents([
            AgentRef::new("agent-root").unwrap(),
            AgentRef::new("agent-parent").unwrap(),
        ]))
    }
}

#[derive(Clone)]
struct RecordingServices {
    events: Arc<Mutex<Vec<&'static str>>>,
    observed_contexts: Arc<Mutex<Vec<(String, String)>>>,
    observed_traces: Arc<Mutex<Vec<TraceContext>>>,
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
    ) -> Result<VerifiedHumanAuthentication, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_human");
        Ok(VerifiedHumanAuthentication::new(
            VerifiedHumanPrincipal::try_new(
                "tenant-1",
                "human-7",
                ActorRef::new("actor-7").unwrap(),
                Timestamp::new(500),
            )
            .unwrap(),
            [],
        ))
    }

    async fn verify_workload(
        &self,
        _headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedWorkloadAuthentication, Diagnostic> {
        self.events.lock().unwrap().push("authenticate_workload");
        Ok(VerifiedWorkloadAuthentication::new(
            VerifiedWorkloadPrincipal::try_new(
                "tenant-other",
                "workload-2",
                "run-9",
                AgentRef::new("agent-2").unwrap(),
                TaskRef::new("task-4").unwrap(),
                SessionRef::new("session-3").unwrap(),
                AdmissionId::new("admission-5").unwrap(),
                Timestamp::new(500),
            )
            .unwrap(),
            [],
        ))
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
        self.observed_traces
            .lock()
            .unwrap()
            .push(context.trace_context().clone());
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

    async fn observe_admission(
        &self,
        context: &ProviderRequestContext,
        _request: &AdmissionRequest,
    ) -> Result<(), ProviderHttpError> {
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

    async fn observe_invocation(
        &self,
        context: &ProviderRequestContext,
        _request: &InvocationRequest,
    ) -> Result<(), ProviderHttpError> {
        self.observe(context, "invoke");
        Err(ProviderHttpError::new(
            HttpErrorKind::Conflict,
            Diagnostic::error(
                DiagnosticCode::InvocationDenied,
                DiagnosticCategory::Authorization,
                DiagnosticStage::Invoke,
                "test invocation observer stops before the unconfigured plane",
            ),
        ))
    }

    async fn observe_status(
        &self,
        request: &AuthenticatedStatusRequest,
    ) -> Result<(), ProviderHttpError> {
        self.observe(request.context(), "status");
        let status_context = request.status_context();
        assert_eq!(status_context.tenant_ref(), "tenant-1");
        assert_eq!(status_context.human_ref(), "human-7");
        assert_eq!(status_context.workload_ref(), "workload-2");
        assert_eq!(status_context.run_ref(), "run-9");
        assert_eq!(status_context.actor_ref(), "actor-7");
        assert_eq!(status_context.agent_ref(), "agent-2");
        assert_eq!(status_context.task_ref(), "task-4");
        assert_eq!(status_context.session_ref(), "session-3");
        assert_eq!(status_context.admission_ref(), "admission-5");
        if request.request().invocation_id().as_str() != "inv-1" {
            return Err(ProviderHttpError::new(
                HttpErrorKind::IdentityMismatch,
                Diagnostic::error(
                    DiagnosticCode::InvocationDenied,
                    DiagnosticCategory::Authorization,
                    DiagnosticStage::Invoke,
                    "status context does not own invocation",
                ),
            ));
        }
        Ok(())
    }
}

struct AdversarialDiagnosticServices;

#[async_trait]
impl ProviderHttpServices for AdversarialDiagnosticServices {
    async fn catalog(
        &self,
        _context: &ProviderRequestContext,
    ) -> Result<CapabilityCatalog, ProviderHttpError> {
        Err(adversarial_error())
    }

    async fn observe_admission(
        &self,
        _context: &ProviderRequestContext,
        _request: &AdmissionRequest,
    ) -> Result<(), ProviderHttpError> {
        Err(adversarial_error())
    }

    async fn observe_invocation(
        &self,
        _context: &ProviderRequestContext,
        _request: &InvocationRequest,
    ) -> Result<(), ProviderHttpError> {
        Err(adversarial_error())
    }

    async fn observe_status(
        &self,
        _request: &AuthenticatedStatusRequest,
    ) -> Result<(), ProviderHttpError> {
        Err(adversarial_error())
    }
}

fn adversarial_error() -> ProviderHttpError {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::RuntimeConstruction,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Runtime,
        "eyJhbGciOiJub25lIn0.opaque.signature",
    );
    diagnostic.package_path = Some("f43e9b7a1d0c4e8f".to_owned());
    diagnostic.source_range = Some(SourceRange { start: 7, end: 19 });
    diagnostic.help = Some("urn:private:payload:7c2a".into());
    diagnostic.retry = RetryClass::AfterRefresh;
    diagnostic
        .details
        .insert("opaque".to_owned(), json!("blob-4d9190c2"));
    ProviderHttpError::new(HttpErrorKind::ServiceFailure, diagnostic)
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

async fn test_services(events: Arc<Mutex<Vec<&'static str>>>) -> ProviderHttpState {
    test_services_with_store(events, false).await
}

async fn test_services_with_foreign_invocation(
    events: Arc<Mutex<Vec<&'static str>>>,
) -> ProviderHttpState {
    test_services_with_store(events, true).await
}

async fn test_services_with_store(
    events: Arc<Mutex<Vec<&'static str>>>,
    include_foreign: bool,
) -> ProviderHttpState {
    let store = Arc::new(InMemoryInvocationStore::new());
    reserve_invocation(&store, "inv-1", "human-7", "actor-7", "key-1").await;
    if include_foreign {
        reserve_invocation(
            &store,
            "inv-other",
            "human-other",
            "actor-other",
            "key-other",
        )
        .await;
    }
    ProviderHttpState::new(
        Arc::new(RecordingServices {
            events,
            observed_contexts: Arc::new(Mutex::new(Vec::new())),
            observed_traces: Arc::new(Mutex::new(Vec::new())),
        }),
        store,
    )
}

async fn reserve_invocation(
    store: &InMemoryInvocationStore,
    invocation_id: &str,
    human_ref: &str,
    actor_ref: &str,
    idempotency_key: &str,
) {
    store
        .reserve_or_get(
            InvocationReservationInput {
                invocation_id: InvocationId::new(invocation_id).unwrap(),
                status_id: format!("status-{invocation_id}"),
                scope: IdempotencyScopeValue::try_new(
                    ActorRef::new(actor_ref).unwrap(),
                    capability_identity(),
                    NormalizedResourceSelector::new(format!("case:{invocation_id}")).unwrap(),
                    "cases.read",
                )
                .unwrap(),
                idempotency_key: IdempotencyKey::new(idempotency_key).unwrap(),
                request_digest: digest(10),
                admission_id: AdmissionId::new("admission-5").unwrap(),
                grant_digest: digest(11),
                catalog_identity: catalog().identity().clone(),
                catalog_digest: *catalog().catalog_digest(),
                authority_revision_digest: digest(12),
                status_context: InvocationStatusContext::try_new(
                    "tenant-1",
                    human_ref,
                    "workload-2",
                    "run-9",
                    actor_ref,
                    "agent-2",
                    "task-4",
                    "session-3",
                    "admission-5",
                )
                .unwrap(),
                proposal_digest: digest(13),
                protected_evidence_refs: vec![],
            },
            &status_descriptor(),
            Timestamp::new(u64::MAX),
        )
        .await
        .unwrap();
}

fn status_descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability_identity(),
        summary: "Read a case".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId"],
            "properties": {"caseId": {"type": "string"}}
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId"],
            "properties": {"caseId": {"type": "string"}}
        }),
        stable_errors: vec![],
        execution_modes: NonEmptySet::try_new(BTreeSet::from([ExecutionMode::Deferred])).unwrap(),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({
            "type": "string",
            "pattern": "^case:[A-Za-z0-9-]+$"
        }))
        .unwrap(),
        effect: EffectClassification::ReadOnly,
        idempotency: IdempotencyRequirement::Required {
            scope: IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: NonZeroU64::new(3_600).unwrap(),
        },
        freshness: FreshnessRequirement::default(),
        preconditions: vec![],
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
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
    admission_request_with_ancestry(DelegationAncestry::try_new(vec![]).unwrap())
}

fn admission_request_with_leaf(leaf: &str) -> AdmissionRequest {
    admission_request_with_ancestry(
        DelegationAncestry::try_new(vec![
            DelegationEdge::try_new(
                AgentRef::new("agent-root").unwrap(),
                AgentRef::new("agent-parent").unwrap(),
                vec![CapabilityName::new("cases.read").unwrap()],
            )
            .unwrap(),
            DelegationEdge::try_new(
                AgentRef::new("agent-parent").unwrap(),
                AgentRef::new(leaf).unwrap(),
                vec![CapabilityName::new("cases.read").unwrap()],
            )
            .unwrap(),
        ])
        .unwrap(),
    )
}

fn admission_request_with_ancestry(delegation_ancestry: DelegationAncestry) -> AdmissionRequest {
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
        delegation_ancestry,
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
