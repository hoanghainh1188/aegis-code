//! Request and response types shared by every backend.
//!
//! The shapes follow the Messages API, which both the Claude API and the
//! Bedrock `InvokeModel` body use.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    /// Must be passed back verbatim in later turns when thinking is active.
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    /// A block type this crate does not model.
    /// Kept so a response still decodes; never sent back to the model.
    #[serde(other)]
    Unsupported,
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn tool_error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![ContentBlock::text(text)])
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema. `serde_json::Map` is a `BTreeMap` (the `preserve_order`
    /// feature is off), so keys serialize sorted and the cached prefix stays
    /// byte-identical across turns (design §4.4).
    pub input_schema: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

/// Lifetime of the cache entries written at BP1–BP3 (design §4.2).
/// BP4 always uses five minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub enum CacheTtl {
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

/// Where the static breakpoints go (design §4.1).
///
/// - BP1: last tool definition.
/// - BP2: last system block.
/// - BP3: last block of `messages[issue_message]` (issue, initial log, repro test).
/// - BP4: moving breakpoint at the end of the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CachePlan {
    pub static_ttl: CacheTtl,
    pub issue_message: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    pub max_tokens: u32,
    pub effort: Option<Effort>,
    pub system: Vec<String>,
    pub tools: Vec<ToolDefinition>,
    pub messages: Vec<Message>,
    pub cache: CachePlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    PauseTurn,
    Refusal,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct Usage {
    #[serde(default, deserialize_with = "null_as_zero")]
    pub input_tokens: u64,
    #[serde(default, deserialize_with = "null_as_zero")]
    pub output_tokens: u64,
    #[serde(default, deserialize_with = "null_as_zero")]
    pub cache_creation_input_tokens: u64,
    #[serde(default, deserialize_with = "null_as_zero")]
    pub cache_read_input_tokens: u64,
    /// Split of `cache_creation_input_tokens` by TTL, priced differently
    /// (design §4.3). Some backends omit it; providers then call
    /// [`Usage::attribute_writes_from_plan`] so the ledger does not price
    /// every write at the one-hour rate (#55). Any remainder the ledger still
    /// sees unattributed is treated as a one-hour write.
    #[serde(default, deserialize_with = "null_as_zero")]
    pub cache_creation: CacheCreation,
}

impl Usage {
    /// When the backend reported cache writes but no TTL split, attribute
    /// them all to [`CachePlan::static_ttl`].
    ///
    /// Bedrock `InvokeModel` usually returns the split (same shape as the
    /// Claude API). When it does not, leaving the tokens unattributed would
    /// make the ledger charge the 1h write multiplier for 5-minute writes
    /// (~60% too high). BP4 is always five minutes and small next to the
    /// static prefix; with a one-hour plan this intentionally prices that
    /// tiny BP4 write at the 1h rate so the budget is never underestimated
    /// (#55). A partial split is left alone: the ledger still prices any
    /// remainder as one-hour writes.
    pub fn attribute_writes_from_plan(&mut self, plan: CachePlan) {
        let attributed = self
            .cache_creation
            .ephemeral_5m_input_tokens
            .saturating_add(self.cache_creation.ephemeral_1h_input_tokens);
        if attributed > 0 || self.cache_creation_input_tokens == 0 {
            return;
        }
        match plan.static_ttl {
            CacheTtl::FiveMinutes => {
                self.cache_creation.ephemeral_5m_input_tokens = self.cache_creation_input_tokens;
            }
            CacheTtl::OneHour => {
                self.cache_creation.ephemeral_1h_input_tokens = self.cache_creation_input_tokens;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct CacheCreation {
    #[serde(default, deserialize_with = "null_as_zero")]
    pub ephemeral_5m_input_tokens: u64,
    #[serde(default, deserialize_with = "null_as_zero")]
    pub ephemeral_1h_input_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Response {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Usage,
}

fn null_as_zero<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The prompt cache relies on `serde_json::Map` being a `BTreeMap`
    /// (design §4.4). `cargo test --workspace` unifies the normal-dependency
    /// features of every member, so any crate turning on `preserve_order`
    /// makes this fail (#20). Build-dependencies are resolved separately and
    /// do not affect it.
    #[test]
    fn serde_json_map_keeps_keys_sorted() {
        let value: Value =
            serde_json::from_str(r#"{"b": 1, "a": 2, "c": {"z": 0, "y": 1}}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"a":2,"b":1,"c":{"y":1,"z":0}}"#
        );
    }

    #[test]
    fn response_decodes_usage_and_skips_unknown_blocks() {
        let raw = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-5",
            "content": [
                {"type": "server_tool_use", "id": "s1", "name": "x", "input": {}},
                {"type": "thinking", "thinking": "...", "signature": "x"},
                {"type": "text", "text": "hi"},
                {"type": "tool_use", "id": "tu_1", "name": "open", "input": {"file": "a.rs"}}
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": 900,
                "cache_creation": null
            }
        });
        let response: Response = serde_json::from_value(raw).unwrap();
        assert_eq!(response.content[0], ContentBlock::Unsupported);
        assert_eq!(
            response.content[1],
            ContentBlock::Thinking {
                thinking: "...".into(),
                signature: "x".into()
            }
        );
        assert_eq!(response.content[2], ContentBlock::text("hi"));
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(
            response.usage,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 900,
                cache_creation: CacheCreation::default(),
            }
        );
    }

    #[test]
    fn tool_result_omits_is_error_when_false() {
        let ok = serde_json::to_value(ContentBlock::tool_result("t", "out")).unwrap();
        assert_eq!(
            ok,
            json!({"type": "tool_result", "tool_use_id": "t", "content": "out"})
        );
        let err = serde_json::to_value(ContentBlock::tool_error("t", "boom")).unwrap();
        assert_eq!(err["is_error"], json!(true));
    }

    #[test]
    fn unsplit_cache_writes_follow_the_plans_static_ttl() {
        let mut five = Usage {
            cache_creation_input_tokens: 900,
            ..Usage::default()
        };
        five.attribute_writes_from_plan(CachePlan {
            static_ttl: CacheTtl::FiveMinutes,
            issue_message: None,
        });
        assert_eq!(five.cache_creation.ephemeral_5m_input_tokens, 900);
        assert_eq!(five.cache_creation.ephemeral_1h_input_tokens, 0);

        let mut hour = Usage {
            cache_creation_input_tokens: 900,
            ..Usage::default()
        };
        hour.attribute_writes_from_plan(CachePlan {
            static_ttl: CacheTtl::OneHour,
            issue_message: None,
        });
        assert_eq!(hour.cache_creation.ephemeral_5m_input_tokens, 0);
        assert_eq!(hour.cache_creation.ephemeral_1h_input_tokens, 900);
    }

    #[test]
    fn attribute_writes_from_plan_leaves_an_existing_split_alone() {
        let mut usage = Usage {
            cache_creation_input_tokens: 700,
            cache_creation: CacheCreation {
                ephemeral_5m_input_tokens: 100,
                ephemeral_1h_input_tokens: 500,
            },
            ..Usage::default()
        };
        usage.attribute_writes_from_plan(CachePlan {
            static_ttl: CacheTtl::FiveMinutes,
            issue_message: None,
        });
        assert_eq!(usage.cache_creation.ephemeral_5m_input_tokens, 100);
        assert_eq!(usage.cache_creation.ephemeral_1h_input_tokens, 500);
    }

    #[test]
    fn attribute_writes_from_plan_is_a_noop_without_writes() {
        let mut usage = Usage::default();
        usage.attribute_writes_from_plan(CachePlan {
            static_ttl: CacheTtl::FiveMinutes,
            issue_message: None,
        });
        assert_eq!(usage, Usage::default());
    }

    #[test]
    fn attribute_writes_from_plan_leaves_a_partial_split_alone() {
        // 200 tokens remain unattributed; the ledger prices those as 1h.
        let mut usage = Usage {
            cache_creation_input_tokens: 700,
            cache_creation: CacheCreation {
                ephemeral_5m_input_tokens: 100,
                ephemeral_1h_input_tokens: 400,
            },
            ..Usage::default()
        };
        usage.attribute_writes_from_plan(CachePlan {
            static_ttl: CacheTtl::FiveMinutes,
            issue_message: None,
        });
        assert_eq!(usage.cache_creation.ephemeral_5m_input_tokens, 100);
        assert_eq!(usage.cache_creation.ephemeral_1h_input_tokens, 400);
    }
}
