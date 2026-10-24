use std::time::Duration;

use aegis_provider::{
    CachePlan, ClaudeApi, ContentBlock, Message, ModelProvider, ProviderError, Request, StopReason,
};
use aegis_secrets::{MemorySource, Secrets};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "test-key-not-real";

fn request() -> Request {
    Request {
        model: "claude-sonnet-5-5".into(),
        max_tokens: 1024,
        effort: None,
        system: vec!["You fix bugs.".into()],
        tools: vec![],
        messages: vec![Message::user_text("Issue #1")],
        cache: CachePlan::default(),
    }
}

async fn provider_for(server: &MockServer) -> ClaudeApi {
    ClaudeApi::new(KEY)
        .unwrap()
        .with_base_url(&server.uri())
        .unwrap()
}

#[tokio::test]
async fn sends_headers_and_decodes_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_partial_json(json!({
            "model": "claude-sonnet-5-5",
            "cache_control": {"type": "ephemeral"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-5",
            "content": [{"type": "text", "text": "done"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12,
                "output_tokens": 3,
                "cache_creation_input_tokens": 600,
                "cache_read_input_tokens": 0,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 100,
                    "ephemeral_1h_input_tokens": 500
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let response = provider_for(&server).await.send(&request()).await.unwrap();

    assert_eq!(response.content, vec![ContentBlock::text("done")]);
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(response.usage.cache_creation_input_tokens, 600);
    assert_eq!(response.usage.cache_creation.ephemeral_5m_input_tokens, 100);
    assert_eq!(response.usage.cache_creation.ephemeral_1h_input_tokens, 500);
}

#[tokio::test]
async fn maps_rate_limit_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_json(json!({
                    "type": "error",
                    "error": {"type": "rate_limit_error", "message": "slow down"}
                })),
        )
        .mount(&server)
        .await;

    let err = provider_for(&server)
        .await
        .send(&request())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ProviderError::RateLimited {
            retry_after_s: Some(7)
        }
    ));
}

#[tokio::test]
async fn maps_overloaded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).set_body_json(json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Overloaded"}
        })))
        .mount(&server)
        .await;

    let err = provider_for(&server)
        .await
        .send(&request())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::Overloaded));
    assert!(err.is_retryable());
}

#[tokio::test]
async fn maps_api_error_kind_and_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "max_tokens too large"}
        })))
        .mount(&server)
        .await;

    let err = provider_for(&server)
        .await
        .send(&request())
        .await
        .unwrap_err();
    match err {
        ProviderError::Api {
            status,
            kind,
            message,
        } => {
            assert_eq!(status, 400);
            assert_eq!(kind, "invalid_request_error");
            assert_eq!(message, "max_tokens too large");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn non_json_error_body_is_still_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
        .mount(&server)
        .await;

    let err = provider_for(&server)
        .await
        .send(&request())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::Api { status: 502, .. }));
    assert!(err.is_retryable());
}

#[tokio::test]
async fn invalid_request_is_rejected_before_any_http_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let mut req = request();
    req.messages.clear();
    let err = provider_for(&server).await.send(&req).await.unwrap_err();
    assert!(matches!(err, ProviderError::InvalidRequest(_)));
}

#[test]
fn debug_output_redacts_api_key() {
    let rendered = format!("{:?}", ClaudeApi::new(KEY).unwrap());
    assert!(!rendered.contains(KEY));
}

#[test]
fn plain_http_base_url_is_refused_for_remote_hosts() {
    let err = ClaudeApi::new(KEY)
        .unwrap()
        .with_base_url("http://proxy.example.com")
        .unwrap_err();
    assert!(matches!(err, ProviderError::Config(_)));
    let err = ClaudeApi::new(KEY)
        .unwrap()
        .with_base_url("http://localhost.evil.example")
        .unwrap_err();
    assert!(matches!(err, ProviderError::Config(_)));
}

#[test]
fn key_with_control_characters_is_a_config_error() {
    let err = ClaudeApi::new("abc\ndef").unwrap_err();
    assert!(matches!(err, ProviderError::Config(_)));
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", "https://elsewhere.example.com"),
        )
        .mount(&server)
        .await;

    let err = provider_for(&server)
        .await
        .send(&request())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::Api { status: 307, .. }));
}

#[test]
fn from_env_reports_missing_variable_by_name() {
    let err = ClaudeApi::from_env("AEGIS_TEST_KEY_THAT_IS_NOT_SET").unwrap_err();
    assert!(err.to_string().contains("AEGIS_TEST_KEY_THAT_IS_NOT_SET"));
}

fn ok_body() -> serde_json::Value {
    json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "model": "claude-sonnet-5-5",
        "content": [{"type": "text", "text": "done"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn unauthorized() -> ResponseTemplate {
    ResponseTemplate::new(401).set_body_json(json!({
        "type": "error",
        "error": {"type": "authentication_error", "message": "invalid x-api-key"}
    }))
}

#[tokio::test]
async fn a_rotated_secret_key_is_used_from_the_next_call() {
    let server = MockServer::start().await;
    Mock::given(header("x-api-key", "key-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(header("x-api-key", "key-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let source = MemorySource::new([("ANTHROPIC_API_KEY", "key-1")]);
    let secrets = Secrets::new(source.clone());
    secrets.require(&["ANTHROPIC_API_KEY"]).await.unwrap();
    let api = ClaudeApi::from_secret(&secrets, "ANTHROPIC_API_KEY")
        .unwrap()
        .with_base_url(&server.uri())
        .unwrap();
    api.send(&request()).await.unwrap();

    source.set("ANTHROPIC_API_KEY", "key-2");
    secrets.refresh().await;
    api.send(&request()).await.unwrap();
}

#[tokio::test]
async fn a_refused_secret_key_is_read_again_and_the_call_retried_once() {
    let server = MockServer::start().await;
    Mock::given(header("x-api-key", "revoked"))
        .respond_with(unauthorized())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(header("x-api-key", "rotated"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let source = MemorySource::new([("ANTHROPIC_API_KEY", "revoked")]);
    let secrets = Secrets::with_min_forced_gap(source.clone(), Duration::from_secs(3600));
    secrets.require(&["ANTHROPIC_API_KEY"]).await.unwrap();
    let api = ClaudeApi::from_secret(&secrets, "ANTHROPIC_API_KEY")
        .unwrap()
        .with_base_url(&server.uri())
        .unwrap();
    // Rotated in the store, not yet read by the process.
    source.set("ANTHROPIC_API_KEY", "rotated");
    api.send(&request()).await.unwrap();
}

#[tokio::test]
async fn a_refused_key_that_did_not_change_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(unauthorized())
        .expect(1)
        .mount(&server)
        .await;
    let secrets = Secrets::new(MemorySource::new([("ANTHROPIC_API_KEY", "bad")]));
    secrets.require(&["ANTHROPIC_API_KEY"]).await.unwrap();
    let api = ClaudeApi::from_secret(&secrets, "ANTHROPIC_API_KEY")
        .unwrap()
        .with_base_url(&server.uri())
        .unwrap();
    let err = api.send(&request()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::Api { status: 401, .. }),
        "{err:?}"
    );
}

#[test]
fn a_secret_key_must_be_loaded_first() {
    assert!(ClaudeApi::from_secret(&Secrets::none(), "ANTHROPIC_API_KEY").is_err());
}
