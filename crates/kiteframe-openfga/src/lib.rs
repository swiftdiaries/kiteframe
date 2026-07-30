#![forbid(unsafe_code)]

mod client;
mod freshness;
mod mapping;

use std::time::Duration;

use async_trait::async_trait;
use client::OpenFgaClient;
use freshness::{current_timestamp, require_fresh_authority};
use kiteframe_contract::{
    AuthorityRevision, AuthorityRevisionSet, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AuthorizationBackend,
    AuthorizationDecision, InvocationAuthorizationRequest, NarrowedAuthorizationConditions,
    SafeDenialReason,
};
use mapping::{
    CheckResponse, ListObjectsResponse, check_request, decision_ref, list_objects_request,
};
use reqwest::Url;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct OpenFgaConfig {
    base_url: Url,
    store_id: String,
    authorization_model_id: String,
    tenant_policy_revision: String,
    deployment_policy_revisions: Vec<AuthorityRevision>,
    bearer_token: Option<String>,
    request_timeout: Duration,
    max_response_bytes: usize,
}

impl OpenFgaConfig {
    pub fn try_new(
        base_url: impl AsRef<str>,
        store_id: impl Into<String>,
        authorization_model_id: impl Into<String>,
        tenant_policy_revision: impl Into<String>,
    ) -> Result<Self, String> {
        let base_url = parse_base_origin(base_url.as_ref())?;
        let store_id = store_id.into();
        let authorization_model_id = authorization_model_id.into();
        let tenant_policy_revision = tenant_policy_revision.into();
        validate_path_id(&store_id, "OpenFGA store ID")?;
        validate_non_empty(&authorization_model_id, "OpenFGA authorization model ID")?;
        validate_non_empty(&tenant_policy_revision, "tenant policy revision")?;
        Ok(Self {
            base_url,
            store_id,
            authorization_model_id,
            tenant_policy_revision,
            deployment_policy_revisions: Vec::new(),
            bearer_token: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        })
    }

    pub fn with_deployment_policy_revision(
        mut self,
        source: impl Into<String>,
        revision: impl Into<String>,
    ) -> Result<Self, String> {
        let entry = AuthorityRevision::try_new(source, revision)?;
        if matches!(
            entry.source(),
            "openfga-model" | "openfga-store" | "tenant-policy"
        ) {
            return Err(
                "deployment policy source collides with a reserved authority source".into(),
            );
        }
        self.deployment_policy_revisions.push(entry);
        // Validate uniqueness immediately instead of deferring a bad deployment
        // configuration until the backend is assembled.
        self.revision_set()?;
        Ok(self)
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Result<Self, String> {
        let token = token.into();
        validate_non_empty(&token, "OpenFGA bearer token")?;
        self.bearer_token = Some(token);
        Ok(self)
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Result<Self, String> {
        if timeout.is_zero() {
            return Err("OpenFGA request timeout must be greater than zero".into());
        }
        self.request_timeout = timeout;
        Ok(self)
    }

    pub fn with_max_response_bytes(mut self, maximum: usize) -> Result<Self, String> {
        if maximum == 0 {
            return Err("OpenFGA maximum response size must be greater than zero".into());
        }
        self.max_response_bytes = maximum;
        Ok(self)
    }

    fn revision_set(&self) -> Result<AuthorityRevisionSet, String> {
        let mut entries = vec![
            AuthorityRevision::try_new("openfga-model", &self.authorization_model_id)?,
            AuthorityRevision::try_new("openfga-store", &self.store_id)?,
            AuthorityRevision::try_new("tenant-policy", &self.tenant_policy_revision)?,
        ];
        entries.extend(self.deployment_policy_revisions.iter().cloned());
        AuthorityRevisionSet::try_new(entries)
    }
}

#[derive(Clone)]
pub struct OpenFgaAuthorizationBackend {
    client: OpenFgaClient,
    authorization_model_id: String,
    revisions: AuthorityRevisionSet,
}

impl OpenFgaAuthorizationBackend {
    pub fn try_new(config: OpenFgaConfig) -> Result<Self, String> {
        let revisions = config.revision_set()?;
        let authorization_model_id = config.authorization_model_id.clone();
        let client = OpenFgaClient::try_new(config)?;
        Ok(Self {
            client,
            authorization_model_id,
            revisions,
        })
    }
}

#[async_trait]
impl AuthorizationBackend for OpenFgaAuthorizationBackend {
    async fn list_admissible(
        &self,
        request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, Diagnostic> {
        let now = current_timestamp()?;
        require_fresh_authority(
            request.principals(),
            request.loaded_authority_revisions(),
            &self.revisions,
            now,
            DiagnosticStage::Admit,
        )?;
        let body = list_objects_request(request, &self.authorization_model_id, now);
        let expected_object = mapping::resource_object(request.selected_resource().as_str());
        let response: ListObjectsResponse = self.client.list_objects(&body).await?;
        let admissible = response
            .objects
            .iter()
            .any(|object| object == &expected_object)
            .then(|| request.capability().clone())
            .into_iter()
            .collect();
        Ok(AdmissionAuthorizationResult::new(admissible))
    }

    async fn check(
        &self,
        request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, Diagnostic> {
        let now = current_timestamp()?;
        require_fresh_authority(
            request.principals(),
            request.loaded_authority_revisions(),
            &self.revisions,
            now,
            DiagnosticStage::Invoke,
        )?;
        let body = check_request(request, &self.authorization_model_id, now);
        let response: CheckResponse = self.client.check(&body).await?;
        let reference = decision_ref(&body, &response, &self.revisions)?;
        if response.allowed {
            let conditions = NarrowedAuthorizationConditions::new(
                vec![request.selected_resource().clone()],
                request.principals().expires_at(),
                Vec::new(),
            )
            .map_err(|_| policy_stale("OpenFGA returned invalid narrowed conditions"))?;
            AuthorizationDecision::allow(reference, self.revisions.clone(), now, conditions)
                .map_err(|_| policy_stale("OpenFGA returned an invalid allow decision"))
        } else {
            AuthorizationDecision::deny(reference, SafeDenialReason::CapabilityDenied)
                .map_err(|_| policy_stale("OpenFGA returned an invalid denial decision"))
        }
    }

    async fn revisions(&self) -> Result<AuthorityRevisionSet, Diagnostic> {
        Ok(self.revisions.clone())
    }
}

fn parse_base_origin(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "OpenFGA base URL must be an absolute URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "OpenFGA base URL must be a credential-free HTTP(S) origin with no path, query, or fragment"
                .into(),
        );
    }
    Ok(url)
}

fn validate_path_id(value: &str, label: &str) -> Result<(), String> {
    validate_non_empty(value, label)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "{label} contains characters unsafe for an endpoint path"
        ));
    }
    Ok(())
}

fn validate_non_empty(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} is required"));
    }
    Ok(())
}

fn policy_stale(message: &'static str) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PolicyStale,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        message,
    )
}
