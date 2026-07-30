use std::time::Duration;

use kiteframe_contract::{Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage};
use reqwest::{Client, Method, redirect::Policy};
use serde::{Serialize, de::DeserializeOwned};

use crate::{OpenFgaConfig, mapping::CheckResponse, mapping::ListObjectsResponse};

#[derive(Clone)]
pub(crate) struct OpenFgaClient {
    client: Client,
    list_objects_url: reqwest::Url,
    check_url: reqwest::Url,
    bearer_token: Option<String>,
    timeout: Duration,
    max_response_bytes: usize,
}

impl OpenFgaClient {
    pub(crate) fn try_new(config: OpenFgaConfig) -> Result<Self, String> {
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|_| "failed to construct the OpenFGA HTTP client")?;
        let list_objects_url = config
            .base_url
            .join(&format!("stores/{}/list-objects", config.store_id))
            .map_err(|_| "failed to construct the OpenFGA ListObjects endpoint")?;
        let check_url = config
            .base_url
            .join(&format!("stores/{}/check", config.store_id))
            .map_err(|_| "failed to construct the OpenFGA Check endpoint")?;
        Ok(Self {
            client,
            list_objects_url,
            check_url,
            bearer_token: config.bearer_token,
            timeout: config.request_timeout,
            max_response_bytes: config.max_response_bytes,
        })
    }

    pub(crate) async fn list_objects<T: Serialize + ?Sized>(
        &self,
        body: &T,
    ) -> Result<ListObjectsResponse, Diagnostic> {
        self.post(&self.list_objects_url, body, DiagnosticStage::Admit)
            .await
    }

    pub(crate) async fn check<T: Serialize + ?Sized>(
        &self,
        body: &T,
    ) -> Result<CheckResponse, Diagnostic> {
        self.post(&self.check_url, body, DiagnosticStage::Invoke)
            .await
    }

    async fn post<T, R>(
        &self,
        url: &reqwest::Url,
        body: &T,
        stage: DiagnosticStage,
    ) -> Result<R, Diagnostic>
    where
        T: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let mut request = self
            .client
            .request(Method::POST, url.clone())
            .json(body)
            .timeout(self.timeout);
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| unavailable(stage, "OpenFGA request failed"))?;
        if !response.status().is_success() {
            return Err(unavailable(stage, "OpenFGA returned a non-success status"));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(unavailable(
                stage,
                "OpenFGA response exceeded the size limit",
            ));
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| unavailable(stage, "OpenFGA response body failed"))?
        {
            let next_length = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| unavailable(stage, "OpenFGA response exceeded the size limit"))?;
            if next_length > self.max_response_bytes {
                return Err(unavailable(
                    stage,
                    "OpenFGA response exceeded the size limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| unavailable(stage, "OpenFGA returned an invalid response"))
    }
}

fn unavailable(stage: DiagnosticStage, message: &'static str) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PolicyStale,
        DiagnosticCategory::Authorization,
        stage,
        message,
    )
}
