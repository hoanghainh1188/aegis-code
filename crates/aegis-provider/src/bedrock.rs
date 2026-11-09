//! Bedrock backend for customer code (design §1, §7): `InvokeModel` in
//! `ap-northeast-1` with `jp.` inference profiles.

use aws_sdk_bedrockruntime::config::http::HttpResponse;
use aws_sdk_bedrockruntime::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_bedrockruntime::operation::invoke_model::InvokeModelError;
use aws_sdk_bedrockruntime::primitives::Blob;

use crate::body::{Target, build_body};
use crate::{CachePlan, ModelPolicy, ModelProvider, ProviderError, Request, Response};

#[derive(Debug, Clone)]
pub struct Bedrock {
    client: aws_sdk_bedrockruntime::Client,
    policy: ModelPolicy,
}

impl Bedrock {
    pub fn new(client: aws_sdk_bedrockruntime::Client, policy: ModelPolicy) -> Self {
        Self { client, policy }
    }

    /// Loads credentials from the standard AWS chain for `region`.
    pub async fn from_region(region: impl Into<String>, policy: ModelPolicy) -> Self {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.into()))
            .load()
            .await;
        Self::new(aws_sdk_bedrockruntime::Client::new(&config), policy)
    }
}

impl ModelProvider for Bedrock {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.policy.check(&request.model)?;
        let body = build_body(request, Target::Bedrock)?;
        let bytes =
            serde_json::to_vec(&body).map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;

        let output = self
            .client
            .invoke_model()
            .model_id(&request.model)
            .content_type("application/json")
            .accept("application/json")
            .body(Blob::new(bytes))
            .send()
            .await
            .map_err(error_from)?;

        decode_response(output.body().as_ref(), request.cache)
    }
}

/// Decode an `InvokeModel` body and attribute unsplit cache writes (#55).
fn decode_response(body: &[u8], plan: CachePlan) -> Result<Response, ProviderError> {
    let mut response: Response =
        serde_json::from_slice(body).map_err(|e| ProviderError::Decode(e.to_string()))?;
    // Bedrock usually returns usage.cache_creation by TTL; when it does
    // not, price from the request's static TTL instead of the ledger's
    // 1h default (#55).
    response.usage.attribute_writes_from_plan(plan);
    Ok(response)
}

fn error_from(error: SdkError<InvokeModelError, HttpResponse>) -> ProviderError {
    let SdkError::ServiceError(context) = error else {
        return match error {
            // Request could not be built: retrying the same input won't help.
            SdkError::ConstructionFailure(_) => ProviderError::Config(error.to_string()),
            other => ProviderError::Transport(other.to_string()),
        };
    };
    let status = context.raw().status().as_u16();
    let err = context.err();
    match err.code() {
        Some("ThrottlingException") => ProviderError::RateLimited {
            retry_after_s: None,
        },
        Some("ServiceUnavailableException" | "ModelNotReadyException") => ProviderError::Overloaded,
        // Bedrock returns 408 here; treat it like a server-side failure.
        Some("ModelTimeoutException") => ProviderError::Api {
            status: 504,
            kind: "ModelTimeoutException".into(),
            message: err.message().unwrap_or_default().to_owned(),
        },
        code => ProviderError::Api {
            status,
            kind: code.unwrap_or("unknown").to_owned(),
            message: err.message().unwrap_or_default().to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CachePlan, CacheTtl, Message};
    use serde_json::json;

    fn offline_client() -> aws_sdk_bedrockruntime::Client {
        let config = aws_sdk_bedrockruntime::Config::builder()
            .behavior_version(aws_sdk_bedrockruntime::config::BehaviorVersion::latest())
            .region(aws_sdk_bedrockruntime::config::Region::new(
                "ap-northeast-1",
            ))
            .build();
        aws_sdk_bedrockruntime::Client::from_conf(config)
    }

    #[tokio::test]
    async fn japan_only_refuses_global_model_before_any_call() {
        let provider = Bedrock::new(offline_client(), ModelPolicy::JapanOnly);
        let request = Request {
            model: "global.anthropic.claude-sonnet-5-5".into(),
            max_tokens: 16,
            effort: None,
            system: vec![],
            tools: vec![],
            messages: vec![Message::user_text("hi")],
            cache: CachePlan::default(),
        };
        let err = provider.send(&request).await.unwrap_err();
        assert!(matches!(err, ProviderError::PolicyViolation { .. }));
    }

    #[test]
    fn decode_attributes_unsplit_writes_from_the_plan() {
        let body = serde_json::to_vec(&json!({
            "id": "msg_bdrk_1",
            "type": "message",
            "role": "assistant",
            "model": "jp.anthropic.claude-haiku-5-5",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 4,
                "output_tokens": 1,
                "cache_creation_input_tokens": 900,
                "cache_read_input_tokens": 0
            }
        }))
        .unwrap();
        let response = decode_response(
            &body,
            CachePlan {
                static_ttl: CacheTtl::FiveMinutes,
                issue_message: None,
            },
        )
        .unwrap();
        assert_eq!(response.usage.cache_creation.ephemeral_5m_input_tokens, 900);
        assert_eq!(response.usage.cache_creation.ephemeral_1h_input_tokens, 0);
    }

    #[test]
    fn decode_keeps_a_backend_ttl_split() {
        let body = serde_json::to_vec(&json!({
            "id": "msg_bdrk_2",
            "type": "message",
            "role": "assistant",
            "model": "jp.anthropic.claude-haiku-5-5",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 4,
                "output_tokens": 1,
                "cache_creation_input_tokens": 362,
                "cache_read_input_tokens": 0,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 288,
                    "ephemeral_1h_input_tokens": 74
                }
            }
        }))
        .unwrap();
        let response = decode_response(
            &body,
            CachePlan {
                static_ttl: CacheTtl::FiveMinutes,
                issue_message: None,
            },
        )
        .unwrap();
        assert_eq!(response.usage.cache_creation.ephemeral_5m_input_tokens, 288);
        assert_eq!(response.usage.cache_creation.ephemeral_1h_input_tokens, 74);
    }
}
