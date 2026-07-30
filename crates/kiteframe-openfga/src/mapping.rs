use kiteframe_contract::{
    AuthorityRevisionSet, CapabilityIdentity, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, Timestamp,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AuthenticatedInvocationContext, InvocationAuthorizationRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ListObjectsResponse {
    #[serde(default)]
    pub(crate) objects: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct CheckResponse {
    pub(crate) allowed: bool,
    #[serde(default)]
    pub(crate) resolution: Option<String>,
}

pub(crate) fn list_objects_request(
    request: &AdmissionAuthorizationRequest,
    authorization_model_id: &str,
    now: Timestamp,
) -> Value {
    json!({
        "authorization_model_id": authorization_model_id,
        "type": "resource",
        "relation": "can_invoke",
        "user": actor_user(request.principals()),
        "context": condition_context(request.principals(), request.capability(), request.selected_resource().as_str(), None, now),
        "contextual_tuples": contextual_tuples(request.principals(), request.capability(), request.selected_resource().as_str()),
        "consistency": "HIGHER_CONSISTENCY",
    })
}

pub(crate) fn check_request(
    request: &InvocationAuthorizationRequest,
    authorization_model_id: &str,
    now: Timestamp,
) -> Value {
    json!({
        "authorization_model_id": authorization_model_id,
        "tuple_key": {
            "user": actor_user(request.principals()),
            "relation": "can_invoke",
            "object": resource_object(request.selected_resource().as_str()),
        },
        "context": condition_context(
            request.principals(),
            request.capability(),
            request.selected_resource().as_str(),
            Some(request.grant_digest()),
            now
        ),
        "contextual_tuples": contextual_tuples(request.principals(), request.capability(), request.selected_resource().as_str()),
        "consistency": "HIGHER_CONSISTENCY",
    })
}

pub(crate) fn capability_object(capability: &CapabilityIdentity) -> String {
    typed_object(
        "capability",
        &format!(
            "{}@{}",
            capability.name().as_str(),
            capability.version().as_str()
        ),
    )
}

pub(crate) fn resource_object(resource: &str) -> String {
    typed_object("resource", resource)
}

pub(crate) fn decision_ref(
    request: &Value,
    response: &CheckResponse,
    revisions: &AuthorityRevisionSet,
) -> Result<String, Diagnostic> {
    let payload =
        serde_json::to_vec(&(request, response, revisions.authority_revision_digest()))
            .map_err(|_| mapping_error("failed to construct an OpenFGA decision reference"))?;
    Ok(format!("openfga:{}", hex(&Sha256::digest(payload))))
}

fn condition_context(
    principals: &AuthenticatedInvocationContext,
    capability: &CapabilityIdentity,
    resource: &str,
    grant_digest: Option<&kiteframe_contract::Sha256Digest>,
    now: Timestamp,
) -> Value {
    let mut context = json!({
        "tenant_ref": principals.tenant_ref().as_str(),
        "human_ref": principals.human_ref().as_str(),
        "workload_ref": principals.workload_ref().as_str(),
        "run_ref": principals.run_ref().as_str(),
        "actor_ref": principals.actor_ref().as_str(),
        "agent_ref": principals.agent_ref().as_str(),
        "task_ref": principals.task_ref().as_str(),
        "session_ref": principals.session_ref().as_str(),
        "admission_ref": principals.admission_ref().as_str(),
        "principal_expires_at": principals.expires_at().unix_seconds(),
        "current_timestamp": now.unix_seconds(),
        "capability_name": capability.name().as_str(),
        "capability_version": capability.version().as_str(),
        "selected_resource": resource,
    });
    if let Some(grant_digest) = grant_digest {
        context["grant_digest"] =
            serde_json::to_value(grant_digest).expect("SHA-256 digest serialization is infallible");
    }
    context
}

fn contextual_tuples(
    principals: &AuthenticatedInvocationContext,
    capability: &CapabilityIdentity,
    resource: &str,
) -> Value {
    let actor = actor_user(principals);
    let task = scoped_object(
        "task",
        &[
            principals.tenant_ref().as_str(),
            principals.task_ref().as_str(),
            principals.workload_ref().as_str(),
            principals.run_ref().as_str(),
            principals.session_ref().as_str(),
            principals.admission_ref().as_str(),
        ],
    );
    let agent = scoped_object(
        "agent",
        &[
            principals.tenant_ref().as_str(),
            principals.agent_ref().as_str(),
            principals.workload_ref().as_str(),
            principals.run_ref().as_str(),
            principals.admission_ref().as_str(),
        ],
    );
    let session = scoped_object(
        "session",
        &[
            principals.tenant_ref().as_str(),
            principals.session_ref().as_str(),
            principals.task_ref().as_str(),
            principals.workload_ref().as_str(),
            principals.run_ref().as_str(),
            principals.admission_ref().as_str(),
        ],
    );
    let capability = capability_object(capability);
    let resource = resource_object(resource);
    json!({
        "tuple_keys": [
            tuple(&actor, "actor", &task),
            tuple(&task, "assigned_task", &agent),
            tuple(&task, "task", &session),
            tuple(&actor, "allowed_actor", &capability),
            tuple(&format!("{agent}#assigned_task"), "allowed_task", &capability),
            tuple(&capability, "capability", &resource),
        ]
    })
}

fn tuple(user: &str, relation: &str, object: &str) -> Value {
    json!({
        "user": user,
        "relation": relation,
        "object": object,
    })
}

fn actor_user(principals: &AuthenticatedInvocationContext) -> String {
    typed_object("actor", principals.actor_ref().as_str())
}

fn scoped_object(object_type: &str, parts: &[&str]) -> String {
    typed_object(object_type, &parts.join("\0"))
}

fn typed_object(object_type: &str, value: &str) -> String {
    format!("{object_type}:{}", hex(value.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn mapping_error(message: &'static str) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PolicyStale,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        message,
    )
}
