//! Residency-safe failover chain across multiple providers (ADR 0018, #595).
//!
//! Tries each configured backend in order. Never fails over on a
//! [`ProviderError::PolicyViolation`], and by default (`retryable_only = true`)
//! fails over only on retryable errors (`429`, `503`/`529`, `5xx`, transport).

use crate::config::LeafBackend;
use crate::{ModelProvider, ProviderError, Request, Response};

/// Ordered provider chain that falls back on transient failures.
#[derive(Debug, Clone)]
pub struct FailoverProvider {
    chain: Vec<LeafBackend>,
    retryable_only: bool,
}

impl FailoverProvider {
    pub(crate) fn new(
        chain: Vec<LeafBackend>,
        retryable_only: bool,
    ) -> Result<Self, ProviderError> {
        if chain.is_empty() {
            return Err(ProviderError::Config(
                "failover provider requires at least one backend in `chain`".into(),
            ));
        }
        Ok(Self {
            chain,
            retryable_only,
        })
    }

    /// True only if every backend in the chain is a local provider.
    pub fn all_local(&self) -> bool {
        self.chain.iter().all(LeafBackend::is_local)
    }

    pub fn len(&self) -> usize {
        self.chain.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }
}

impl ModelProvider for FailoverProvider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let mut last_err = None;
        for (idx, backend) in self.chain.iter().enumerate() {
            match backend.send(request).await {
                Ok(response) => return Ok(response),
                Err(err) => {
                    let is_last = idx + 1 == self.chain.len();
                    let stop = matches!(err, ProviderError::PolicyViolation { .. })
                        || (self.retryable_only && !err.is_retryable());
                    if stop || is_last {
                        return Err(err);
                    }
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| ProviderError::Config("empty failover chain".into())))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::{CachePlan, ContentBlock, Message, Provider, ProviderConfig};

    fn sample_request() -> Request {
        Request {
            model: "gpt-4o".into(),
            max_tokens: 32,
            effort: None,
            system: Vec::new(),
            tools: Vec::new(),
            messages: vec![Message::user_text("ping")],
            cache: CachePlan::default(),
        }
    }

    #[tokio::test]
    async fn fails_over_to_secondary_on_rate_limit_429() {
        let primary = MockServer::start().await;
        let secondary = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "5"))
            .mount(&primary)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ok-secondary",
                "model": "gpt-4o",
                "choices": [{ "message": { "content": "from-secondary" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 4 }
            })))
            .mount(&secondary)
            .await;

        let config = ProviderConfig::Failover {
            retryable_only: true,
            chain: vec![
                ProviderConfig::Local {
                    base_url: primary.uri(),
                    api_key_env: None,
                    protocol: crate::LocalProtocol::OpenAi,
                },
                ProviderConfig::Local {
                    base_url: secondary.uri(),
                    api_key_env: None,
                    protocol: crate::LocalProtocol::OpenAi,
                },
            ],
        };
        let provider = Provider::from_config(&config).await.unwrap();
        let resp = provider.send(&sample_request()).await.unwrap();
        assert_eq!(
            resp.content,
            vec![ContentBlock::Text {
                text: "from-secondary".into()
            }]
        );
    }

    #[tokio::test]
    async fn does_not_fail_over_on_non_retryable_400_when_retryable_only() {
        let primary = MockServer::start().await;
        let secondary = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": { "type": "invalid_request_error", "message": "bad prompt" }
            })))
            .mount(&primary)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&secondary)
            .await;

        let config = ProviderConfig::Failover {
            retryable_only: true,
            chain: vec![
                ProviderConfig::Local {
                    base_url: primary.uri(),
                    api_key_env: None,
                    protocol: crate::LocalProtocol::OpenAi,
                },
                ProviderConfig::Local {
                    base_url: secondary.uri(),
                    api_key_env: None,
                    protocol: crate::LocalProtocol::OpenAi,
                },
            ],
        };
        let provider = Provider::from_config(&config).await.unwrap();
        let err = provider.send(&sample_request()).await.unwrap_err();
        assert!(
            matches!(err, ProviderError::Api { status: 400, .. }),
            "{err}"
        );
    }
}
