//! Claude API backend: direct HTTPS calls to the Messages API (ADR 0001).

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderValue, RETRY_AFTER};
use reqwest::{StatusCode, redirect};
use serde::Deserialize;

use crate::body::{Target, build_body};
use crate::{ModelProvider, ProviderError, Request, Response};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2023-06-01";
pub const DEFAULT_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Overloaded is a non-standard status the API uses under load.
const STATUS_OVERLOADED: u16 = 529;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Non-streaming requests with large `max_tokens` can run for minutes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct ClaudeApi {
    http: reqwest::Client,
    base_url: String,
    api_key: HeaderValue,
}

impl ClaudeApi {
    pub fn new(api_key: &str) -> Result<Self, ProviderError> {
        let mut api_key = HeaderValue::from_str(api_key.trim())
            .map_err(|_| ProviderError::Config("API key contains invalid characters".into()))?;
        api_key.set_sensitive(true);
        // No redirects: reqwest does not strip `x-api-key` on cross-host redirects.
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ProviderError::Config(format!("HTTP client: {e}")))?;
        Ok(Self {
            http,
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key,
        })
    }

    /// Reads the API key from `var`, failing fast when it is missing.
    pub fn from_env(var: &str) -> Result<Self, ProviderError> {
        let key = std::env::var(var)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                ProviderError::Config(format!("environment variable {var} is not set"))
            })?;
        Self::new(&key)
    }

    /// Overrides the API origin. Must be https, except for loopback hosts.
    pub fn with_base_url(self, base_url: &str) -> Result<Self, ProviderError> {
        let base_url = base_url.trim_end_matches('/');
        let url = reqwest::Url::parse(base_url)
            .map_err(|e| ProviderError::Config(format!("invalid base_url {base_url}: {e}")))?;
        let loopback = match url.host() {
            Some(url::Host::Domain(domain)) => domain == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        let allowed = url.scheme() == "https" || (url.scheme() == "http" && loopback);
        if !allowed {
            return Err(ProviderError::Config(format!(
                "base_url must use https: {base_url}"
            )));
        }
        Ok(Self {
            base_url: base_url.to_owned(),
            ..self
        })
    }
}

impl fmt::Debug for ClaudeApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeApi")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ModelProvider for ClaudeApi {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let body = build_body(request, Target::ClaudeApi)?;
        let response = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(send_error)?;

        let status = response.status();
        let retry_after_s = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok());
        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.without_url().to_string()))?;

        if status.is_success() {
            serde_json::from_str(&text).map_err(|e| ProviderError::Decode(e.to_string()))
        } else {
            Err(error_from(status, retry_after_s, &text))
        }
    }
}

fn send_error(error: reqwest::Error) -> ProviderError {
    if error.is_builder() {
        ProviderError::Config(error.without_url().to_string())
    } else {
        ProviderError::Transport(error.without_url().to_string())
    }
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

fn error_from(status: StatusCode, retry_after_s: Option<u64>, body: &str) -> ProviderError {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return ProviderError::RateLimited { retry_after_s };
    }
    if status.as_u16() == STATUS_OVERLOADED {
        return ProviderError::Overloaded;
    }
    let (kind, message) = match serde_json::from_str::<ErrorEnvelope>(body) {
        Ok(envelope) => (envelope.error.kind, envelope.error.message),
        Err(_) => (
            "unknown".to_owned(),
            status.canonical_reason().unwrap_or("").to_owned(),
        ),
    };
    ProviderError::Api {
        status: status.as_u16(),
        kind,
        message,
    }
}
