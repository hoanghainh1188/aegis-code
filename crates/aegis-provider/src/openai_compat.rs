//! OpenAI-compatible and Azure OpenAI backend (ADR 0018, #595).
//!
//! Supports both standard OpenAI-compatible gateways (`Authorization: Bearer`)
//! and Azure OpenAI deployments (`api-key` header + `api-version` query
//! parameter), with live secret rotation via `aegis_secrets::Secrets`.

use std::fmt;
use std::time::Duration;

use aegis_secrets::Secrets;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use reqwest::{StatusCode, redirect};

use crate::local::{
    OpenAiChatResponse, build_openai_request, convert_openai_response, error_from, send_error,
};
use crate::{ModelProvider, ProviderError, Request, Response};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const AZURE_API_KEY_HEADER: HeaderName = HeaderName::from_static("api-key");

#[derive(Clone)]
enum ApiKey {
    Fixed(String),
    Secret { secrets: Secrets, name: String },
}

impl ApiKey {
    fn current(&self) -> Result<String, ProviderError> {
        match self {
            Self::Fixed(value) => Ok(value.clone()),
            Self::Secret { secrets, name } => secrets
                .get(name)
                .map(|s| s.expose().trim().to_owned())
                .ok_or_else(|| ProviderError::Config(format!("secret {name} is not loaded"))),
        }
    }

    async fn refresh(&self) -> bool {
        match self {
            Self::Fixed(_) => false,
            Self::Secret { secrets, name } => secrets
                .refresh()
                .await
                .iter()
                .any(|changed| changed == name),
        }
    }
}

/// OpenAI-compatible enterprise gateway or Azure OpenAI deployment provider.
#[derive(Clone)]
pub struct OpenAiCompatProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: ApiKey,
    azure_api_version: Option<String>,
}

impl fmt::Debug for OpenAiCompatProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatProvider")
            .field("base_url", &self.base_url)
            .field("azure_api_version", &self.azure_api_version)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: &str,
        azure_api_version: Option<String>,
    ) -> Result<Self, ProviderError> {
        let key = api_key.trim();
        if key.is_empty() {
            return Err(ProviderError::Config("API key is empty".into()));
        }
        Self::with_key(
            base_url.into(),
            ApiKey::Fixed(key.to_owned()),
            azure_api_version,
        )
    }

    pub fn from_env(
        base_url: impl Into<String>,
        env_var: &str,
        azure_api_version: Option<String>,
    ) -> Result<Self, ProviderError> {
        let key = std::env::var(env_var).map_err(|_| {
            ProviderError::Config(format!("environment variable {env_var} is not set"))
        })?;
        Self::new(base_url, &key, azure_api_version)
    }

    pub fn from_secret(
        base_url: impl Into<String>,
        secrets: &Secrets,
        name: &str,
        azure_api_version: Option<String>,
    ) -> Result<Self, ProviderError> {
        let key = ApiKey::Secret {
            secrets: secrets.clone(),
            name: name.to_owned(),
        };
        let _ = key.current()?;
        Self::with_key(base_url.into(), key, azure_api_version)
    }

    fn with_key(
        base_url: String,
        api_key: ApiKey,
        azure_api_version: Option<String>,
    ) -> Result<Self, ProviderError> {
        let trimmed = base_url.trim().trim_end_matches('/').to_owned();
        if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
            return Err(ProviderError::Config(format!(
                "openai_compat base_url `{trimmed}` must start with https:// or http://"
            )));
        }
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ProviderError::Config(format!("HTTP client: {e}")))?;
        Ok(Self {
            http,
            base_url: trimmed,
            api_key,
            azure_api_version,
        })
    }

    fn endpoint_url(&self, model: &str) -> String {
        if let Some(version) = &self.azure_api_version {
            format!(
                "{}/openai/deployments/{model}/chat/completions?api-version={version}",
                self.base_url
            )
        } else if self.base_url.ends_with("/chat/completions") {
            self.base_url.clone()
        } else if self.base_url.ends_with("/v1") {
            format!("{}/chat/completions", self.base_url)
        } else {
            format!("{}/v1/chat/completions", self.base_url)
        }
    }

    fn auth_headers(&self) -> Result<HeaderMap, ProviderError> {
        let key = self.api_key.current()?;
        let mut headers = HeaderMap::new();
        if self.azure_api_version.is_some() {
            let mut val = HeaderValue::from_str(&key)
                .map_err(|_| ProviderError::Config("API key contains invalid characters".into()))?;
            val.set_sensitive(true);
            headers.insert(AZURE_API_KEY_HEADER, val);
        } else {
            let mut val = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| ProviderError::Config("API key contains invalid characters".into()))?;
            val.set_sensitive(true);
            headers.insert(AUTHORIZATION, val);
        }
        Ok(headers)
    }

    async fn send_once(
        &self,
        request: &Request,
    ) -> Result<(StatusCode, Option<u64>, String), ProviderError> {
        let body = build_openai_request(request)?;
        let url = self.endpoint_url(&request.model);
        let headers = self.auth_headers()?;
        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(send_error)?;
        let status = response.status();
        let retry_after_s = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.without_url().to_string()))?;
        Ok((status, retry_after_s, text))
    }
}

impl ModelProvider for OpenAiCompatProvider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let (mut status, mut retry_after_s, mut text) = self.send_once(request).await?;
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && self.api_key.refresh().await
        {
            (status, retry_after_s, text) = self.send_once(request).await?;
        }
        if status.is_success() {
            let parsed: OpenAiChatResponse = serde_json::from_str(&text)
                .map_err(|e| ProviderError::Decode(format!("{e}: {text}")))?;
            convert_openai_response(parsed, &request.model)
        } else {
            Err(error_from(status, retry_after_s, &text))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::{CachePlan, ContentBlock, Message};

    fn sample_request(model: &str) -> Request {
        Request {
            model: model.into(),
            max_tokens: 64,
            effort: None,
            system: vec!["System rules".into()],
            tools: Vec::new(),
            messages: vec![Message::user_text("Fix the bug")],
            cache: CachePlan::default(),
        }
    }

    #[tokio::test]
    async fn azure_openai_uses_deployment_url_api_key_and_maps_cached_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/gpt-4o/chat/completions"))
            .and(query_param("api-version", "2024-10-21"))
            .and(header("api-key", "az-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "model": "gpt-4o",
                "choices": [{
                    "message": { "content": "Done" },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1000,
                    "completion_tokens": 25,
                    "prompt_tokens_details": { "cached_tokens": 800 }
                }
            })))
            .mount(&server)
            .await;

        let provider =
            OpenAiCompatProvider::new(server.uri(), "az-secret", Some("2024-10-21".into()))
                .unwrap();
        let resp = provider.send(&sample_request("gpt-4o")).await.unwrap();
        assert_eq!(
            resp.content,
            vec![ContentBlock::Text {
                text: "Done".into()
            }]
        );
        assert_eq!(resp.usage.input_tokens, 200);
        assert_eq!(resp.usage.cache_read_input_tokens, 800);
        assert_eq!(resp.usage.output_tokens, 25);
    }

    #[tokio::test]
    async fn standard_gateway_uses_bearer_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer gw-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-2",
                "model": "gpt-4.1",
                "choices": [{
                    "message": { "content": "Patched" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 100, "completion_tokens": 10 }
            })))
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "gw-token", None).unwrap();
        let resp = provider.send(&sample_request("gpt-4.1")).await.unwrap();
        assert_eq!(
            resp.content,
            vec![ContentBlock::Text {
                text: "Patched".into()
            }]
        );
    }
}
