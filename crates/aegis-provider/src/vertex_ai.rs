//! Google Cloud Vertex AI backend (ADR 0018, #595).
//!
//! Routes automatically by model family:
//! - `claude-*` models call Vertex AI `:rawPredict` using the Anthropic
//!   Messages format (`anthropic_version = "vertex-2023-10-16"`) with full
//!   4-breakpoint prompt caching (`BP1`–`BP4`).
//! - Gemini models call Vertex AI `:generateContent`, preserving byte-stable
//!   `systemInstruction` and `functionDeclarations` order for implicit prefix
//!   caching and mapping `cachedContentTokenCount` to `Usage`.

use std::fmt;
use std::time::Duration;

use aegis_secrets::Secrets;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{StatusCode, redirect};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::body::{Target, build_body};
use crate::local::send_error;
use crate::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, Role, StopReason, Usage,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const STATUS_OVERLOADED: u16 = 529;

#[derive(Clone)]
enum AccessToken {
    Fixed(String),
    Secret { secrets: Secrets, name: String },
}

impl AccessToken {
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

/// Google Cloud Vertex AI provider supporting Gemini and Claude-on-Vertex.
#[derive(Clone)]
pub struct VertexAiProvider {
    http: reqwest::Client,
    project: String,
    region: String,
    base_url: String,
    token: AccessToken,
}

impl fmt::Debug for VertexAiProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VertexAiProvider")
            .field("project", &self.project)
            .field("region", &self.region)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl VertexAiProvider {
    pub fn new(
        project: impl Into<String>,
        region: impl Into<String>,
        access_token: &str,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let token = access_token.trim();
        if token.is_empty() {
            return Err(ProviderError::Config(
                "Vertex AI access token is empty".into(),
            ));
        }
        Self::with_token(
            project.into(),
            region.into(),
            AccessToken::Fixed(token.to_owned()),
            base_url,
        )
    }

    pub fn from_env(
        project: impl Into<String>,
        region: impl Into<String>,
        env_var: &str,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let token = std::env::var(env_var).map_err(|_| {
            ProviderError::Config(format!("environment variable {env_var} is not set"))
        })?;
        Self::new(project, region, &token, base_url)
    }

    pub fn from_secret(
        project: impl Into<String>,
        region: impl Into<String>,
        secrets: &Secrets,
        name: &str,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let token = AccessToken::Secret {
            secrets: secrets.clone(),
            name: name.to_owned(),
        };
        let _ = token.current()?;
        Self::with_token(project.into(), region.into(), token, base_url)
    }

    fn with_token(
        project: String,
        region: String,
        token: AccessToken,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        if project.trim().is_empty() {
            return Err(ProviderError::Config(
                "vertex_ai: `project` is empty".into(),
            ));
        }
        if region.trim().is_empty() {
            return Err(ProviderError::Config("vertex_ai: `region` is empty".into()));
        }
        let default_base = if region == "global" {
            "https://aiplatform.googleapis.com".to_owned()
        } else {
            format!("https://{region}-aiplatform.googleapis.com")
        };
        let base_url = base_url
            .unwrap_or(default_base)
            .trim()
            .trim_end_matches('/')
            .to_owned();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(ProviderError::Config(format!(
                "vertex_ai base_url `{base_url}` must start with https:// or http://"
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
            project,
            region,
            base_url,
            token,
        })
    }

    fn is_claude_model(model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        lower.starts_with("claude-") || lower.contains(".claude-")
    }

    fn endpoint_url(&self, model: &str) -> String {
        if Self::is_claude_model(model) {
            format!(
                "{}/v1/projects/{}/locations/{}/publishers/anthropic/models/{model}:rawPredict",
                self.base_url, self.project, self.region
            )
        } else {
            format!(
                "{}/v1/projects/{}/locations/{}/publishers/google/models/{model}:generateContent",
                self.base_url, self.project, self.region
            )
        }
    }

    fn auth_headers(&self) -> Result<HeaderMap, ProviderError> {
        let token = self.token.current()?;
        let mut headers = HeaderMap::new();
        let mut val = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            ProviderError::Config("Vertex AI token contains invalid characters".into())
        })?;
        val.set_sensitive(true);
        headers.insert(AUTHORIZATION, val);
        Ok(headers)
    }

    async fn post_json(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<(StatusCode, Option<u64>, String), ProviderError> {
        let headers = self.auth_headers()?;
        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(body)
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

impl ModelProvider for VertexAiProvider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let is_claude = Self::is_claude_model(&request.model);
        let url = self.endpoint_url(&request.model);
        let body = if is_claude {
            build_body(request, Target::VertexClaude)?
        } else {
            build_gemini_request(request)?
        };

        let (mut status, mut retry_after_s, mut text) = self.post_json(&url, &body).await?;
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && self.token.refresh().await
        {
            (status, retry_after_s, text) = self.post_json(&url, &body).await?;
        }

        if status.is_success() {
            if is_claude {
                serde_json::from_str::<Response>(&text)
                    .map_err(|e| ProviderError::Decode(format!("{e}: {text}")))
            } else {
                let raw: GeminiResponse = serde_json::from_str(&text)
                    .map_err(|e| ProviderError::Decode(format!("{e}: {text}")))?;
                convert_gemini_response(raw, &request.model)
            }
        } else {
            Err(vertex_error(status, retry_after_s, &text))
        }
    }
}

fn build_gemini_request(request: &Request) -> Result<Value, ProviderError> {
    if request.messages.is_empty() {
        return Err(ProviderError::InvalidRequest(
            "request has no messages".into(),
        ));
    }

    let mut body = serde_json::Map::new();

    if !request.system.is_empty() {
        let parts: Vec<Value> = request
            .system
            .iter()
            .map(|text| json!({ "text": text }))
            .collect();
        body.insert("systemInstruction".into(), json!({ "parts": parts }));
    }

    if !request.tools.is_empty() {
        let decls: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        body.insert("tools".into(), json!([{ "functionDeclarations": decls }]));
    }

    let mut contents = Vec::with_capacity(request.messages.len());
    for msg in &request.messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "model",
        };
        let mut parts = Vec::new();
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({ "text": text })),
                ContentBlock::ToolUse { name, input, .. } => {
                    parts.push(json!({
                        "functionCall": {
                            "name": name,
                            "args": input,
                        }
                    }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    parts.push(json!({
                        "functionResponse": {
                            "name": tool_use_id,
                            "response": {
                                "content": content,
                                "is_error": is_error,
                            }
                        }
                    }));
                }
                ContentBlock::Thinking { .. }
                | ContentBlock::RedactedThinking { .. }
                | ContentBlock::Unsupported => {}
            }
        }
        if !parts.is_empty() {
            contents.push(json!({ "role": role, "parts": parts }));
        }
    }

    body.insert("contents".into(), Value::Array(contents));
    body.insert(
        "generationConfig".into(),
        json!({ "maxOutputTokens": request.max_tokens }),
    );
    Ok(Value::Object(body))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    #[serde(default)]
    response_id: Option<String>,
    #[serde(default)]
    model_version: Option<String>,
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct GeminiContent {
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    function_call: Option<GeminiFunctionCall>,
}

#[derive(Deserialize)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GeminiUsage {
    #[serde(default)]
    prompt_token_count: u64,
    #[serde(default)]
    candidates_token_count: u64,
    #[serde(default)]
    cached_content_token_count: u64,
}

fn convert_gemini_response(
    raw: GeminiResponse,
    fallback_model: &str,
) -> Result<Response, ProviderError> {
    let candidate = raw.candidates.into_iter().next().ok_or_else(|| {
        ProviderError::Decode("Vertex AI response contained no candidates".into())
    })?;

    let mut content = Vec::new();
    let mut has_tool_use = false;
    if let Some(body) = candidate.content {
        for (idx, part) in body.parts.into_iter().enumerate() {
            if let Some(text) = part.text
                && !text.is_empty()
            {
                content.push(ContentBlock::Text { text });
            }
            if let Some(call) = part.function_call {
                has_tool_use = true;
                let input = if call.args.is_null() {
                    json!({})
                } else {
                    call.args
                };
                content.push(ContentBlock::ToolUse {
                    id: format!("vertex-call-{idx}-{}", call.name),
                    name: call.name,
                    input,
                });
            }
        }
    }

    let stop_reason = if has_tool_use {
        Some(StopReason::ToolUse)
    } else {
        match candidate.finish_reason.as_deref() {
            Some("MAX_TOKENS") => Some(StopReason::MaxTokens),
            Some("STOP") | None => Some(StopReason::EndTurn),
            Some(_) => Some(StopReason::Other),
        }
    };

    let usage = raw.usage_metadata.unwrap_or_default();
    // Gemini `promptTokenCount` includes cached tokens; subtract them so ledger pricing does not double-count.
    let cache_read = usage.cached_content_token_count;
    let uncached_input = usage.prompt_token_count.saturating_sub(cache_read);

    Ok(Response {
        id: raw.response_id.unwrap_or_default(),
        model: raw
            .model_version
            .unwrap_or_else(|| fallback_model.to_owned()),
        content,
        stop_reason,
        usage: Usage {
            input_tokens: uncached_input,
            output_tokens: usage.candidates_token_count,
            cache_read_input_tokens: cache_read,
            ..Default::default()
        },
    })
}

#[derive(Deserialize)]
struct GoogleErrorEnvelope {
    error: GoogleErrorDetail,
}

#[derive(Deserialize)]
struct GoogleErrorDetail {
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
}

fn vertex_error(status: StatusCode, retry_after_s: Option<u64>, body: &str) -> ProviderError {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return ProviderError::RateLimited { retry_after_s };
    }
    if status == StatusCode::SERVICE_UNAVAILABLE || status.as_u16() == STATUS_OVERLOADED {
        return ProviderError::Overloaded;
    }
    let (kind, message) = match serde_json::from_str::<GoogleErrorEnvelope>(body) {
        Ok(env) => (env.error.status, env.error.message),
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::{CachePlan, Message, ToolDefinition};

    #[tokio::test]
    async fn gemini_maps_function_calls_and_cached_content_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/my-proj/locations/asia-northeast1/publishers/google/models/gemini-2.5-pro:generateContent",
            ))
            .and(header("authorization", "Bearer gcp-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "responseId": "resp-1",
                "modelVersion": "gemini-2.5-pro-001",
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [
                            { "text": "Checking file" },
                            { "functionCall": { "name": "open", "args": { "path": "src/lib.rs" } } }
                        ]
                    },
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 5000,
                    "candidatesTokenCount": 120,
                    "cachedContentTokenCount": 4200
                }
            })))
            .mount(&server)
            .await;

        let provider = VertexAiProvider::new(
            "my-proj",
            "asia-northeast1",
            "gcp-token",
            Some(server.uri()),
        )
        .unwrap();
        let req = Request {
            model: "gemini-2.5-pro".into(),
            max_tokens: 1024,
            effort: None,
            system: vec!["System".into()],
            tools: vec![ToolDefinition {
                name: "open".into(),
                description: "Open file".into(),
                input_schema: json!({"type": "object"}),
            }],
            messages: vec![Message::user_text("Inspect")],
            cache: CachePlan::default(),
        };
        let resp = provider.send(&req).await.unwrap();
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(resp.usage.input_tokens, 800);
        assert_eq!(resp.usage.cache_read_input_tokens, 4200);
        assert_eq!(resp.usage.output_tokens, 120);
        assert_eq!(resp.content.len(), 2);
    }

    #[tokio::test]
    async fn claude_on_vertex_uses_raw_predict_and_preserves_cache_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/my-proj/locations/asia-northeast1/publishers/anthropic/models/claude-sonnet-4-5:rawPredict",
            ))
            .and(header("authorization", "Bearer gcp-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_vertex_1",
                "model": "claude-sonnet-4-5",
                "content": [{ "type": "text", "text": "Fixed" }],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 300,
                    "output_tokens": 40,
                    "cache_read_input_tokens": 2700,
                    "cache_creation_input_tokens": 0
                }
            })))
            .mount(&server)
            .await;

        let provider = VertexAiProvider::new(
            "my-proj",
            "asia-northeast1",
            "gcp-token",
            Some(server.uri()),
        )
        .unwrap();
        let req = Request {
            model: "claude-sonnet-4-5".into(),
            max_tokens: 1024,
            effort: None,
            system: vec!["System".into()],
            tools: Vec::new(),
            messages: vec![Message::user_text("Fix")],
            cache: CachePlan::default(),
        };
        let resp = provider.send(&req).await.unwrap();
        assert_eq!(resp.usage.cache_read_input_tokens, 2700);
        assert_eq!(resp.usage.input_tokens, 300);
    }
}
