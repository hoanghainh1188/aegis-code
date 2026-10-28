//! Local model backend supporting OpenAI Chat Completions and Anthropic Messages protocols (Issue #38).

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{StatusCode, redirect};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude_api::ClaudeApi;
use crate::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, Role, StopReason, Usage,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// Put before the content of a failed tool call, which OpenAI's `tool`
/// message cannot flag.
const TOOL_ERROR_PREFIX: &str = "Error: ";

/// Protocol used to communicate with the local model runner (vLLM, Ollama, LocalAI, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalProtocol {
    #[default]
    #[serde(alias = "openai")]
    OpenAi,
    Anthropic,
}

/// A provider that talks to a locally hosted LLM endpoint.
#[derive(Debug, Clone)]
pub struct LocalModelProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    protocol: LocalProtocol,
    anthropic: Option<ClaudeApi>,
}

impl LocalModelProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        protocol: LocalProtocol,
    ) -> Result<Self, ProviderError> {
        let base_url = base_url.into();
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(redirect::Policy::none())
            .build()
            .map_err(|e| ProviderError::Config(e.to_string()))?;

        let anthropic = if protocol == LocalProtocol::Anthropic {
            let key = api_key.as_deref().unwrap_or("local-key");
            Some(ClaudeApi::new(key)?.with_base_url(&base_url)?)
        } else {
            None
        };

        Ok(Self {
            http,
            base_url,
            api_key,
            protocol,
            anthropic,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn protocol(&self) -> LocalProtocol {
        self.protocol
    }

    fn chat_completions_url(&self) -> String {
        let trimmed = self.base_url.trim_end_matches('/');
        if trimmed.ends_with("/chat/completions") {
            trimmed.to_owned()
        } else if trimmed.ends_with("/v1") {
            format!("{trimmed}/chat/completions")
        } else {
            format!("{trimmed}/v1/chat/completions")
        }
    }
}

impl ModelProvider for LocalModelProvider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        match self.protocol {
            LocalProtocol::Anthropic => {
                if let Some(anthropic) = &self.anthropic {
                    anthropic.send(request).await
                } else {
                    Err(ProviderError::Config(
                        "Anthropic provider not initialized".into(),
                    ))
                }
            }
            LocalProtocol::OpenAi => self.send_openai(request).await,
        }
    }
}

impl LocalModelProvider {
    async fn send_openai(&self, request: &Request) -> Result<Response, ProviderError> {
        let body = build_openai_request(request)?;
        let url = self.chat_completions_url();

        let mut headers = HeaderMap::new();
        if let Some(key) = &self.api_key {
            let val = format!("Bearer {}", key.trim());
            let mut header_val = HeaderValue::from_str(&val)
                .map_err(|_| ProviderError::Config("Invalid API key header".into()))?;
            header_val.set_sensitive(true);
            headers.insert(AUTHORIZATION, header_val);
        }

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

        if status.is_success() {
            let chat_response: OpenAiChatResponse = serde_json::from_str(&text)
                .map_err(|e| ProviderError::Decode(format!("{e}: {text}")))?;
            convert_openai_response(chat_response, &request.model)
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

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool<'a>>,
}

#[derive(Serialize)]
struct OpenAiTool<'a> {
    r#type: &'static str,
    function: OpenAiFunction<'a>,
}

#[derive(Serialize)]
struct OpenAiFunction<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
    parameters: &'a Value,
}

#[derive(Serialize)]
#[serde(tag = "role")]
enum OpenAiMessage {
    #[serde(rename = "system")]
    System { content: String },
    #[serde(rename = "user")]
    User { content: String },
    #[serde(rename = "assistant")]
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<OpenAiToolCall>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
struct OpenAiToolCall {
    id: String,
    r#type: String,
    function: OpenAiFunctionCall,
}

#[derive(Clone, Serialize, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

fn build_openai_request<'a>(request: &'a Request) -> Result<OpenAiChatRequest<'a>, ProviderError> {
    let mut messages = Vec::new();

    if !request.system.is_empty() {
        messages.push(OpenAiMessage::System {
            content: request.system.join("\n\n"),
        });
    }

    for msg in &request.messages {
        match msg.role {
            Role::User => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            messages.push(OpenAiMessage::User {
                                content: text.clone(),
                            });
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            // A `tool` message has no error flag; say it in
                            // the text so the model knows the call failed (#222).
                            let content = if *is_error {
                                format!("{TOOL_ERROR_PREFIX}{content}")
                            } else {
                                content.clone()
                            };
                            messages.push(OpenAiMessage::Tool {
                                tool_call_id: tool_use_id.clone(),
                                content,
                            });
                        }
                        _ => {}
                    }
                }
            }
            Role::Assistant => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(OpenAiToolCall {
                                id: id.clone(),
                                r#type: "function".to_owned(),
                                function: OpenAiFunctionCall {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            });
                        }
                        _ => {}
                    }
                }

                let content = if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join("\n\n"))
                };

                messages.push(OpenAiMessage::Assistant {
                    content,
                    tool_calls,
                });
            }
        }
    }

    let tools = request
        .tools
        .iter()
        .map(|t| OpenAiTool {
            r#type: "function",
            function: OpenAiFunction {
                name: &t.name,
                description: &t.description,
                parameters: &t.input_schema,
            },
        })
        .collect();

    Ok(OpenAiChatRequest {
        model: &request.model,
        messages,
        max_tokens: request.max_tokens,
        tools,
    })
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}

#[derive(Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Deserialize, Default)]
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

fn convert_openai_response(
    raw: OpenAiChatResponse,
    fallback_model: &str,
) -> Result<Response, ProviderError> {
    let choice = raw
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Decode("OpenAI response contained no choices".into()))?;

    let mut content = Vec::new();
    if let Some(text) = choice.message.content
        && !text.is_empty()
    {
        content.push(ContentBlock::Text { text });
    }

    for tc in choice.message.tool_calls {
        let input: Value = serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| {
            serde_json::json!({
                "raw": tc.function.arguments
            })
        });
        content.push(ContentBlock::ToolUse {
            id: tc.id,
            name: tc.function.name,
            input,
        });
    }

    let stop_reason = match choice.finish_reason.as_deref() {
        Some("tool_calls" | "function_call") => Some(StopReason::ToolUse),
        Some("stop") => Some(StopReason::EndTurn),
        Some("length") => Some(StopReason::MaxTokens),
        Some(_) => Some(StopReason::Other),
        None => None,
    };

    let usage = raw.usage.unwrap_or_default();
    let cache_read_input_tokens = usage
        .prompt_tokens_details
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    // OpenAI counts cached tokens in `prompt_tokens`; `input_tokens` is the
    // uncached rest, as with the Messages API (#222).
    let input_tokens = usage.prompt_tokens.saturating_sub(cache_read_input_tokens);

    Ok(Response {
        id: raw.id.unwrap_or_else(|| "local-response".into()),
        model: raw.model.unwrap_or_else(|| fallback_model.to_owned()),
        content,
        stop_reason,
        usage: Usage {
            input_tokens,
            output_tokens: usage.completion_tokens,
            cache_read_input_tokens,
            ..Default::default()
        },
    })
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    message: String,
    #[serde(default, rename = "type")]
    kind: String,
}

fn error_from(status: StatusCode, retry_after_s: Option<u64>, body: &str) -> ProviderError {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return ProviderError::RateLimited { retry_after_s };
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
