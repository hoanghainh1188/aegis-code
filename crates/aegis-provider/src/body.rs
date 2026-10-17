//! Builds the JSON request body and places the cache breakpoints (design §4.1).

use serde_json::{Map, Value, json};

use crate::{CacheTtl, ContentBlock, Message, ProviderError, Request};

/// Bedrock `InvokeModel` body version for Anthropic models.
pub(crate) const BEDROCK_ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// Claude API: BP4 is the request-level automatic breakpoint.
    ClaudeApi,
    /// Bedrock: model goes in the URL, BP4 is placed by hand on the last block.
    #[cfg_attr(not(feature = "bedrock"), allow(dead_code))]
    Bedrock,
}

pub(crate) fn build_body(request: &Request, target: Target) -> Result<Value, ProviderError> {
    validate(request)?;
    let static_marker = cache_marker(request.cache.static_ttl);

    let mut body = Map::new();
    body.insert("max_tokens".into(), json!(request.max_tokens));

    if !request.tools.is_empty() {
        let tools = to_array(&request.tools)?;
        body.insert("tools".into(), mark_last(tools, &static_marker));
    }

    if !request.system.is_empty() {
        let system = request
            .system
            .iter()
            .map(|text| json!({"type": "text", "text": text}))
            .collect();
        body.insert("system".into(), mark_last(system, &static_marker));
    }

    let messages = request
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let marker = (request.cache.issue_message == Some(index)).then_some(&static_marker);
            message_value(message, marker)
        })
        .collect::<Result<Vec<_>, _>>()?;

    if let Some(effort) = request.effort {
        body.insert("output_config".into(), json!({"effort": effort}));
    }

    let rolling_marker = cache_marker(CacheTtl::FiveMinutes);
    match target {
        Target::ClaudeApi => {
            body.insert("model".into(), json!(request.model));
            body.insert("cache_control".into(), rolling_marker);
            body.insert("messages".into(), Value::Array(messages));
        }
        Target::Bedrock => {
            body.insert("anthropic_version".into(), json!(BEDROCK_ANTHROPIC_VERSION));
            body.insert(
                "messages".into(),
                Value::Array(mark_final_block(messages, rolling_marker)),
            );
        }
    }

    Ok(Value::Object(body))
}

fn validate(request: &Request) -> Result<(), ProviderError> {
    if request.messages.is_empty() {
        return Err(ProviderError::InvalidRequest(
            "request has no messages".into(),
        ));
    }
    if request.system.iter().any(|text| text.trim().is_empty()) {
        return Err(ProviderError::InvalidRequest(
            "system blocks must not be empty".into(),
        ));
    }
    match request.cache.issue_message {
        Some(index) if index >= request.messages.len() => {
            Err(ProviderError::InvalidRequest(format!(
                "issue breakpoint at message {index}, but only {} messages",
                request.messages.len()
            )))
        }
        _ => Ok(()),
    }
}

fn cache_marker(ttl: CacheTtl) -> Value {
    match ttl {
        CacheTtl::FiveMinutes => json!({"type": "ephemeral"}),
        CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
    }
}

fn to_array<T: serde::Serialize>(items: &[T]) -> Result<Vec<Value>, ProviderError> {
    items
        .iter()
        .map(|item| {
            serde_json::to_value(item).map_err(|e| ProviderError::InvalidRequest(e.to_string()))
        })
        .collect()
}

/// Returns `items` with `cache_control` set on the last element.
fn mark_last(items: Vec<Value>, marker: &Value) -> Value {
    let last = items.len().saturating_sub(1);
    Value::Array(
        items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                if index == last {
                    with_marker(item, marker.clone())
                } else {
                    item
                }
            })
            .collect(),
    )
}

fn with_marker(item: Value, marker: Value) -> Value {
    match item {
        Value::Object(mut map) => {
            map.insert("cache_control".into(), marker);
            Value::Object(map)
        }
        other => other,
    }
}

fn message_value(message: &Message, marker: Option<&Value>) -> Result<Value, ProviderError> {
    let sendable: Vec<&ContentBlock> = message
        .content
        .iter()
        .filter(|block| !matches!(block, ContentBlock::Unsupported))
        .collect();
    if sendable.is_empty() {
        return Err(ProviderError::InvalidRequest(
            "message has no sendable content blocks".into(),
        ));
    }
    let blocks = to_array(&sendable)?;
    let content = match marker {
        Some(marker) => mark_last(blocks, marker),
        None => Value::Array(blocks),
    };
    Ok(json!({"role": message.role, "content": content}))
}

/// Bedrock BP4: mark the last block of the last message, unless it already
/// carries a (longer-lived) static breakpoint.
fn mark_final_block(messages: Vec<Value>, marker: Value) -> Vec<Value> {
    let last = messages.len().saturating_sub(1);
    messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            if index != last {
                return message;
            }
            let Value::Object(mut map) = message else {
                return message;
            };
            if let Some(Value::Array(blocks)) = map.remove("content") {
                let already_marked = blocks
                    .last()
                    .is_some_and(|block| block.get("cache_control").is_some());
                let content = if already_marked {
                    Value::Array(blocks)
                } else {
                    mark_last(blocks, &marker)
                };
                map.insert("content".into(), content);
            }
            Value::Object(map)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CachePlan, Effort, ToolDefinition};

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object", "properties": {"z": {}, "a": {}}}),
        }
    }

    fn request(cache: CachePlan) -> Request {
        Request {
            model: "claude-sonnet-5-5".into(),
            max_tokens: 4096,
            effort: None,
            system: vec!["You fix bugs.".into(), "Rust conventions.".into()],
            tools: vec![tool("open"), tool("edit")],
            messages: vec![
                Message::user_text("Issue #12: panic in parser"),
                Message::assistant(vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "open".into(),
                    input: json!({"file": "src/parser.rs"}),
                }]),
                Message::user(vec![ContentBlock::tool_result("tu_1", "fn parse() {}")]),
            ],
            cache,
        }
    }

    fn one_hour_plan() -> CachePlan {
        CachePlan {
            static_ttl: CacheTtl::OneHour,
            issue_message: Some(0),
        }
    }

    fn count_breakpoints(value: &Value) -> usize {
        match value {
            Value::Object(map) => {
                usize::from(map.contains_key("cache_control"))
                    + map.values().map(count_breakpoints).sum::<usize>()
            }
            Value::Array(items) => items.iter().map(count_breakpoints).sum(),
            _ => 0,
        }
    }

    #[test]
    fn claude_api_places_static_breakpoints_and_automatic_bp4() {
        let body = build_body(&request(one_hour_plan()), Target::ClaudeApi).unwrap();
        let one_hour = json!({"type": "ephemeral", "ttl": "1h"});

        assert_eq!(body["model"], json!("claude-sonnet-5-5"));
        assert_eq!(body["max_tokens"], json!(4096));
        assert_eq!(body["tools"][1]["cache_control"], one_hour, "BP1");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["system"][1]["cache_control"], one_hour, "BP2");
        assert_eq!(
            body["system"][0],
            json!({"type": "text", "text": "You fix bugs."})
        );
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"], one_hour,
            "BP3"
        );
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}), "BP4");
        assert!(body.get("anthropic_version").is_none());
        assert_eq!(count_breakpoints(&body), 4);
    }

    #[test]
    fn five_minute_plan_omits_ttl() {
        let plan = CachePlan {
            static_ttl: CacheTtl::FiveMinutes,
            issue_message: Some(0),
        };
        let body = build_body(&request(plan), Target::ClaudeApi).unwrap();
        assert_eq!(
            body["tools"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn bedrock_moves_model_out_and_places_bp4_on_last_block() {
        let body = build_body(&request(one_hour_plan()), Target::Bedrock).unwrap();

        assert!(body.get("model").is_none());
        assert_eq!(body["anthropic_version"], json!(BEDROCK_ANTHROPIC_VERSION));
        assert!(body.get("cache_control").is_none());
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"],
            json!({"type": "ephemeral"}),
            "BP4"
        );
        assert_eq!(count_breakpoints(&body), 4);
    }

    #[test]
    fn bedrock_does_not_double_mark_when_issue_is_last_message() {
        let mut req = request(one_hour_plan());
        req.messages.truncate(1);
        let body = build_body(&req, Target::Bedrock).unwrap();
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
        assert_eq!(count_breakpoints(&body), 3);
    }

    #[test]
    fn empty_tools_and_system_are_omitted() {
        let mut req = request(CachePlan::default());
        req.tools.clear();
        req.system.clear();
        let body = build_body(&req, Target::ClaudeApi).unwrap();
        assert!(body.get("tools").is_none());
        assert!(body.get("system").is_none());
        assert_eq!(count_breakpoints(&body), 1);
    }

    #[test]
    fn issue_index_out_of_range_is_rejected() {
        let plan = CachePlan {
            static_ttl: CacheTtl::FiveMinutes,
            issue_message: Some(9),
        };
        let err = build_body(&request(plan), Target::ClaudeApi).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }

    #[test]
    fn empty_messages_are_rejected() {
        let mut req = request(CachePlan::default());
        req.messages.clear();
        let err = build_body(&req, Target::Bedrock).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }

    #[test]
    fn unsupported_blocks_are_not_sent() {
        let mut req = request(CachePlan::default());
        req.messages[1].content.insert(0, ContentBlock::Unsupported);
        let body = build_body(&req, Target::ClaudeApi).unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], json!("tool_use"));
    }

    #[test]
    fn thinking_blocks_are_replayed_verbatim() {
        let mut req = request(CachePlan::default());
        req.messages[1].content.insert(
            0,
            ContentBlock::Thinking {
                thinking: "look at parser".into(),
                signature: "sig".into(),
            },
        );
        let body = build_body(&req, Target::ClaudeApi).unwrap();
        assert_eq!(
            body["messages"][1]["content"][0],
            json!({"type": "thinking", "thinking": "look at parser", "signature": "sig"})
        );
    }

    #[test]
    fn empty_system_block_is_rejected() {
        let mut req = request(CachePlan::default());
        req.system.push("  ".into());
        let err = build_body(&req, Target::ClaudeApi).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }

    #[test]
    fn effort_goes_into_output_config() {
        let mut req = request(CachePlan::default());
        req.effort = Some(Effort::Low);
        let body = build_body(&req, Target::ClaudeApi).unwrap();
        assert_eq!(body["output_config"], json!({"effort": "low"}));

        let without = build_body(&request(CachePlan::default()), Target::ClaudeApi).unwrap();
        assert!(without.get("output_config").is_none());
    }

    #[test]
    fn serialization_is_byte_stable_with_sorted_keys() {
        let req = request(one_hour_plan());
        let first = serde_json::to_string(&build_body(&req, Target::ClaudeApi).unwrap()).unwrap();
        let second = serde_json::to_string(&build_body(&req, Target::ClaudeApi).unwrap()).unwrap();
        assert_eq!(first, second);
        assert!(first.contains(r#""properties":{"a":{},"z":{}}"#));
    }
}
