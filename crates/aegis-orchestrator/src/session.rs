//! Conversation state for one issue, independent of the model, so the full
//! history carries over on escalation from Core to Expert (design §5.3).

use aegis_provider::{
    CachePlan, CacheTtl, ContentBlock, Effort, Message, Request, Role, ToolDefinition,
};

/// Replaces a collapsed tool result (design §3.3).
pub const COLLAPSED: &str = "[Observation collapsed]";

/// Which model to call and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    pub effort: Option<Effort>,
    pub max_tokens: u32,
    /// Context window in tokens; drives history collapsing.
    pub context_window: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Session {
    system: Vec<String>,
    messages: Vec<Message>,
    static_ttl: CacheTtl,
    /// Model whose signed thinking blocks are in the history, if any.
    thinking_model: Option<String>,
}

impl Session {
    /// Starts a session. `issue` is the first user turn (issue text, initial
    /// log, repro test) and carries cache breakpoint BP3.
    pub fn new(system: Vec<String>, issue: Message, static_ttl: CacheTtl) -> Self {
        Self {
            system,
            messages: vec![issue],
            static_ttl,
            thinking_model: None,
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn last_message(&self) -> Option<&Message> {
        self.messages.last()
    }

    pub fn with_message(self, message: Message) -> Self {
        let mut messages = self.messages;
        messages.push(message);
        Self { messages, ..self }
    }

    /// Adds `text` for the model to read next: to the last message when it
    /// is the user's (the results of the tools it just called), so turns
    /// keep alternating; as a new user message otherwise.
    pub fn with_user_text(self, text: impl Into<String>) -> Self {
        let mut messages = self.messages;
        match messages.last_mut() {
            Some(last) if last.role == Role::User => last.content.push(ContentBlock::text(text)),
            _ => messages.push(Message::user_text(text)),
        }
        Self { messages, ..self }
    }

    /// Appends an assistant turn produced by `model`.
    pub fn with_assistant(self, model: &str, message: Message) -> Self {
        let has_thinking = message.content.iter().any(is_thinking);
        let thinking_model = if has_thinking {
            Some(model.to_owned())
        } else {
            self.thinking_model.clone()
        };
        Self {
            thinking_model,
            ..self.with_message(message)
        }
    }

    /// Prepares the history for `model`. Thinking blocks are signed by the
    /// model that wrote them, so they are dropped when another model takes
    /// over (escalation Core → Expert).
    pub fn prepared_for(self, model: &str) -> Self {
        match &self.thinking_model {
            Some(owner) if owner != model => {
                let messages = self.messages.into_iter().map(strip_thinking).collect();
                Self {
                    messages,
                    thinking_model: None,
                    ..self
                }
            }
            _ => self,
        }
    }

    /// Number of user turns carrying tool results (after the issue turn).
    pub fn observation_count(&self) -> usize {
        self.observation_indices().len()
    }

    fn observation_indices(&self) -> Vec<usize> {
        self.messages
            .iter()
            .enumerate()
            .skip(ISSUE_INDEX + 1)
            .filter(|(_, message)| is_observation(message))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn request(&self, model: &ModelSpec, tools: &[ToolDefinition]) -> Request {
        Request {
            model: model.id.clone(),
            max_tokens: model.max_tokens,
            effort: model.effort,
            system: self.system.clone(),
            tools: tools.to_vec(),
            messages: self.messages.clone(),
            cache: CachePlan {
                static_ttl: self.static_ttl,
                issue_message: Some(ISSUE_INDEX),
            },
        }
    }

    /// Replaces the content of every tool result older than the
    /// `keep_recent` most recent tool-result turns with [`COLLAPSED`].
    /// Agent reasoning and actions are never touched, nor is the issue turn.
    /// Returns the new session and how many turns changed.
    pub fn collapse_observations(self, keep_recent: usize) -> (Self, usize) {
        let observation_turns = self.observation_indices();
        let cutoff = observation_turns.len().saturating_sub(keep_recent);
        let to_collapse: Vec<usize> = observation_turns[..cutoff]
            .iter()
            .copied()
            .filter(|&index| !is_collapsed(&self.messages[index]))
            .collect();
        let changed = to_collapse.len();

        let messages = self
            .messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| {
                if to_collapse.contains(&index) {
                    collapse_message(message)
                } else {
                    message
                }
            })
            .collect();
        (Self { messages, ..self }, changed)
    }
}

fn is_thinking(block: &ContentBlock) -> bool {
    matches!(
        block,
        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
    )
}

fn strip_thinking(message: Message) -> Message {
    if message.role != Role::Assistant || !message.content.iter().any(is_thinking) {
        return message;
    }
    let content: Vec<ContentBlock> = message
        .content
        .into_iter()
        .filter(|block| !is_thinking(block))
        .collect();
    // An assistant turn may not be empty; keep a marker if only thinking was there.
    let content = if content.is_empty() {
        vec![ContentBlock::text("(reasoning omitted)")]
    } else {
        content
    };
    Message { content, ..message }
}

fn is_collapsed(message: &Message) -> bool {
    message.content.iter().all(|block| match block {
        ContentBlock::ToolResult { content, .. } => content == COLLAPSED,
        _ => true,
    })
}

fn is_observation(message: &Message) -> bool {
    message.role == Role::User
        && message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

fn collapse_message(message: Message) -> Message {
    let content = message
        .content
        .into_iter()
        .map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => ContentBlock::ToolResult {
                tool_use_id,
                content: COLLAPSED.to_owned(),
                is_error,
            },
            other => other,
        })
        .collect();
    Message { content, ..message }
}

const ISSUE_INDEX: usize = 0;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_text_joins_the_last_user_message_and_starts_one_after_the_model() {
        let session = Session::new(vec![], Message::user_text("issue"), CacheTtl::FiveMinutes);
        let [call, result] = tool_turn("a.rs", "fn a() {}");
        let session = session
            .with_assistant("m", call)
            .with_message(result)
            .with_user_text("run your test");
        let roles: Vec<_> = session.messages().iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::User]);
        let last = session.last_message().unwrap();
        assert!(matches!(last.content[0], ContentBlock::ToolResult { .. }));
        assert_eq!(
            last.content.last(),
            Some(&ContentBlock::text("run your test"))
        );

        let session = session
            .with_assistant("m", Message::assistant(vec![ContentBlock::text("ok")]))
            .with_user_text("again");
        assert_eq!(session.messages().len(), 5);
        assert_eq!(session.last_message(), Some(&Message::user_text("again")));
    }

    fn tool_turn(id: &str, output: &str) -> [Message; 2] {
        [
            Message::assistant(vec![
                ContentBlock::text(format!("open {id}")),
                ContentBlock::ToolUse {
                    id: id.into(),
                    name: "open".into(),
                    input: json!({"file": id}),
                },
            ]),
            Message::user(vec![ContentBlock::tool_result(id, output)]),
        ]
    }

    fn session_with_turns(count: usize) -> Session {
        (0..count).fold(
            Session::new(
                vec!["sys".into()],
                Message::user_text("issue"),
                CacheTtl::OneHour,
            ),
            |session, i| {
                let [assistant, user] = tool_turn(&format!("t{i}"), &format!("output {i}"));
                session.with_message(assistant).with_message(user)
            },
        )
    }

    fn tool_output(message: &Message) -> &str {
        match &message.content[0] {
            ContentBlock::ToolResult { content, .. } => content,
            other => panic!("expected tool result, got {other:?}"),
        }
    }

    #[test]
    fn request_carries_model_and_issue_breakpoint() {
        let model = ModelSpec {
            id: "claude-sonnet-5-5".into(),
            effort: Some(Effort::Low),
            max_tokens: 8000,
            context_window: 200_000,
        };
        let request = session_with_turns(1).request(&model, &[]);
        assert_eq!(request.model, "claude-sonnet-5-5");
        assert_eq!(request.effort, Some(Effort::Low));
        assert_eq!(request.messages.len(), 3);
        assert_eq!(
            request.cache,
            CachePlan {
                static_ttl: CacheTtl::OneHour,
                issue_message: Some(0)
            }
        );
    }

    #[test]
    fn collapse_keeps_recent_turns_and_reasoning() {
        let (collapsed, changed) = session_with_turns(7).collapse_observations(5);
        assert_eq!(changed, 2);
        let messages = collapsed.messages();

        assert_eq!(messages[0], Message::user_text("issue"));
        assert_eq!(tool_output(&messages[2]), COLLAPSED);
        assert_eq!(tool_output(&messages[4]), COLLAPSED);
        assert_eq!(tool_output(&messages[6]), "output 2");
        assert_eq!(tool_output(&messages[14]), "output 6");
        // Assistant turns are untouched.
        assert_eq!(messages[1], tool_turn("t0", "")[0]);
        assert_eq!(messages.len(), 15);
    }

    #[test]
    fn collapse_preserves_error_flag_and_ids() {
        let session = Session::new(vec![], Message::user_text("issue"), CacheTtl::FiveMinutes)
            .with_message(tool_turn("a", "")[0].clone())
            .with_message(Message::user(vec![ContentBlock::tool_error("a", "boom")]))
            .with_message(tool_turn("b", "")[0].clone())
            .with_message(Message::user(vec![ContentBlock::tool_result("b", "ok")]));
        let (collapsed, _) = session.collapse_observations(1);
        assert_eq!(
            collapsed.messages()[2].content[0],
            ContentBlock::tool_error("a", COLLAPSED)
        );
    }

    #[test]
    fn collapse_is_idempotent_and_noop_when_few_turns() {
        let few = session_with_turns(3);
        assert_eq!(few.clone().collapse_observations(5), (few, 0));

        let (once, changed) = session_with_turns(8).collapse_observations(5);
        assert_eq!(changed, 3);
        assert_eq!(once.clone().collapse_observations(5), (once, 0));
    }

    #[test]
    fn collapse_never_touches_the_issue_turn() {
        let session = Session::new(
            vec![],
            Message::user(vec![
                ContentBlock::text("issue"),
                ContentBlock::tool_result("seed", "initial log"),
            ]),
            CacheTtl::FiveMinutes,
        );
        let (collapsed, changed) = session.clone().collapse_observations(0);
        assert_eq!((collapsed, changed), (session, 0));
    }

    fn thinking_turn() -> Message {
        Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: "sig".into(),
            },
            ContentBlock::text("plan"),
        ])
    }

    #[test]
    fn thinking_is_kept_for_the_same_model() {
        let session = Session::new(vec![], Message::user_text("issue"), CacheTtl::FiveMinutes)
            .with_assistant("core", thinking_turn());
        assert_eq!(session.clone().prepared_for("core"), session);
    }

    #[test]
    fn thinking_is_dropped_when_another_model_takes_over() {
        let session = Session::new(vec![], Message::user_text("issue"), CacheTtl::FiveMinutes)
            .with_assistant("core", thinking_turn())
            .with_assistant(
                "core",
                Message::assistant(vec![ContentBlock::RedactedThinking { data: "x".into() }]),
            );
        let prepared = session.prepared_for("expert");
        assert_eq!(
            prepared.messages()[1],
            Message::assistant(vec![ContentBlock::text("plan")])
        );
        assert_eq!(
            prepared.messages()[2],
            Message::assistant(vec![ContentBlock::text("(reasoning omitted)")])
        );
        assert_eq!(prepared.messages()[0], Message::user_text("issue"));
    }
}
