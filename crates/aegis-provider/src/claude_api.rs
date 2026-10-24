//! Claude API backend: direct HTTPS calls to the Messages API (ADR 0001).

use std::fmt;
use std::time::Duration;

use aegis_secrets::Secrets;
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

/// The key of every call: fixed, or the secret's current value, read
/// again when the API refuses it (rotation, #30).
#[derive(Clone)]
enum ApiKey {
    Fixed(HeaderValue),
    Secret { secrets: Secrets, name: String },
}

fn key_header(api_key: &str) -> Result<HeaderValue, ProviderError> {
    let mut value = HeaderValue::from_str(api_key.trim())
        .map_err(|_| ProviderError::Config("API key contains invalid characters".into()))?;
    value.set_sensitive(true);
    Ok(value)
}

impl ApiKey {
    fn header(&self) -> Result<HeaderValue, ProviderError> {
        match self {
            Self::Fixed(value) => Ok(value.clone()),
            Self::Secret { secrets, name } => {
                let secret = secrets
                    .get(name)
                    .ok_or_else(|| ProviderError::Config(format!("secret {name} is not loaded")))?;
                key_header(secret.expose())
            }
        }
    }
}

#[derive(Clone)]
pub struct ClaudeApi {
    http: reqwest::Client,
    base_url: String,
    api_key: ApiKey,
}

impl ClaudeApi {
    pub fn new(api_key: &str) -> Result<Self, ProviderError> {
        Self::with_key(ApiKey::Fixed(key_header(api_key)?))
    }

    /// The key is secret `name` of `secrets`, already loaded: each call
    /// takes its value then, so a rotated key is used from the next call.
    pub fn from_secret(secrets: &Secrets, name: &str) -> Result<Self, ProviderError> {
        let key = ApiKey::Secret {
            secrets: secrets.clone(),
            name: name.to_owned(),
        };
        key.header()?;
        Self::with_key(key)
    }

    fn with_key(api_key: ApiKey) -> Result<Self, ProviderError> {
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
        let mut response = self.post(&body, self.api_key.header()?).await?;
        // A refused secret key may have been rotated: read it again and
        // try once more with the new one.
        if response.status() == StatusCode::UNAUTHORIZED
            && let ApiKey::Secret { secrets, name } = &self.api_key
            && let Some(rotated) = secrets.refresh_after_auth_failure(name).await
        {
            response = self.post(&body, key_header(rotated.expose())?).await?;
        }

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

impl ClaudeApi {
    async fn post(
        &self,
        body: &serde_json::Value,
        key: HeaderValue,
    ) -> Result<reqwest::Response, ProviderError> {
        self.http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(send_error)
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
