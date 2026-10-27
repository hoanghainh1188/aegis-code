use aegis_provider::{
    CachePlan, ContentBlock, LocalModelProvider, LocalProtocol, Message, ModelProvider, Request,
    StopReason,
};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_request() -> Request {
    Request {
        model: "qwen2.5-coder-32b".into(),
        max_tokens: 2048,
        effort: None,
        system: vec!["You are an expert coder.".into()],
        tools: vec![],
        messages: vec![Message::user_text("Fix issue in parser.rs")],
        cache: CachePlan::default(),
    }
}

#[tokio::test]
async fn openai_protocol_sends_chat_completions_and_decodes_response() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-local-key"))
        .and(body_partial_json(json!({
            "model": "qwen2.5-coder-32b",
            "messages": [
                {
                    "role": "system",
                    "content": "You are an expert coder."
                },
                {
                    "role": "user",
                    "content": "Fix issue in parser.rs"
                }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-local-42",
            "object": "chat.completion",
            "model": "qwen2.5-coder-32b",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Here is the fix for parser.rs"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 120,
                "completion_tokens": 45,
                "total_tokens": 165
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = LocalModelProvider::new(
        server.uri(),
        Some("test-local-key".into()),
        LocalProtocol::OpenAi,
    )
    .unwrap();

    let response = provider.send(&test_request()).await.unwrap();

    assert_eq!(response.id, "chatcmpl-local-42");
    assert_eq!(response.model, "qwen2.5-coder-32b");
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        response.content,
        vec![ContentBlock::text("Here is the fix for parser.rs")]
    );
    assert_eq!(response.usage.input_tokens, 120);
    assert_eq!(response.usage.output_tokens, 45);
}

#[tokio::test]
async fn openai_protocol_handles_tool_use_and_tool_results() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-tool-call",
            "object": "chat.completion",
            "model": "qwen2.5-coder-32b",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_123",
                                "type": "function",
                                "function": {
                                    "name": "search_symbol",
                                    "arguments": "{\"query\":\"Parser\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }
            ],
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 30
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = LocalModelProvider::new(server.uri(), None, LocalProtocol::OpenAi).unwrap();
    let response = provider.send(&test_request()).await.unwrap();

    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(
        response.content,
        vec![ContentBlock::ToolUse {
            id: "call_123".into(),
            name: "search_symbol".into(),
            input: json!({"query": "Parser"}),
        }]
    );
}

#[tokio::test]
async fn anthropic_protocol_delegates_to_claude_messages_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "local-anthropic-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_local_1",
            "type": "message",
            "role": "assistant",
            "model": "qwen2.5-coder-32b",
            "content": [
                {
                    "type": "text",
                    "text": "Fixed via Anthropic Messages endpoint."
                }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = LocalModelProvider::new(
        server.uri(),
        Some("local-anthropic-key".into()),
        LocalProtocol::Anthropic,
    )
    .unwrap();

    let response = provider.send(&test_request()).await.unwrap();
    assert_eq!(response.id, "msg_local_1");
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        response.content,
        vec![ContentBlock::text("Fixed via Anthropic Messages endpoint.")]
    );
}
