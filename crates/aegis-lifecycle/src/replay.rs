//! A dry run of the eval set (#104): a provider that "fixes" each issue by
//! writing its real fix (the `source_files` at `fix_commit`) through the
//! ACI tools, so fetch, gate, hidden tests and the ledger run on the real
//! repos with no model and no cost. A run that fails here fails on the
//! platform, not on the model.

use std::path::Path;
use std::sync::Mutex;

use aegis_provider::{
    ContentBlock, Message, ModelProvider, ProviderError, Request, Response, Role, StopReason, Usage,
};
use serde_json::{Value, json};

use crate::eval::EvalIssue;
use crate::{LifecycleError, job};

/// The tool calls that write one issue's fix, then the final answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    calls: Vec<(String, Value)>,
    answer: String,
}

fn text(repo: &Path, rev: &str, file: &str) -> Result<Option<String>, LifecycleError> {
    job::show(repo, rev, file)?
        .map(|bytes| {
            String::from_utf8(bytes)
                .map_err(|_| LifecycleError::Issue(format!("{file} at {rev} is not UTF-8")))
        })
        .transpose()
}

impl Replay {
    /// `issue`'s fix, read from the clone at `repo`: its source files, and
    /// the tests it changes that exist at the base (an `xfail` the fix
    /// makes pass, an expected value that changes). A real agent has to
    /// change those too, and lists them as allowed test changes, with the
    /// new test files that go with them (a new reference). When the change
    /// includes a binary file (a reference image), which the tools cannot
    /// write, none of it is.
    pub fn of(issue: &EvalIssue, repo: &Path) -> Result<Self, LifecycleError> {
        let mut calls = Vec::new();
        for file in &issue.source_files {
            let fixed = text(repo, &issue.fix_commit, file)?.ok_or_else(|| {
                LifecycleError::Issue(format!(
                    "{}: the fix deletes {file}; the tools cannot delete a file",
                    issue.id
                ))
            })?;
            calls.push(match text(repo, &issue.base_commit, file)? {
                Some(old) => edit_whole(file, &old, fixed),
                None => (
                    "create_file".to_owned(),
                    json!({"file": file, "content": fixed}),
                ),
            });
        }
        let mut allowed = Vec::new();
        let mut edits = Vec::new();
        let mut all_text = true;
        for file in &issue.test_files {
            let fixed = text(repo, &issue.fix_commit, file);
            let old = text(repo, &issue.base_commit, file);
            match (old, fixed) {
                (Ok(Some(old)), Ok(Some(fixed))) if old != fixed => {
                    edits.push((file, Some(old), fixed));
                }
                (Ok(None), Ok(Some(fixed))) => edits.push((file, None, fixed)),
                (Ok(_), Ok(_)) => {}
                // A reference image: the tools write text only.
                _ => all_text = false,
            }
        }
        // Half of a test change (a new test without its new reference)
        // fails where neither half would: all of it or none. New files go
        // in only with a change to an existing one; alone they are the
        // hidden tests, copied in after the gate.
        let changes_existing = edits.iter().any(|(_, old, _)| old.is_some());
        if all_text && changes_existing {
            for (file, old, fixed) in edits {
                match old {
                    Some(old) => {
                        calls.push(edit_whole(file, &old, fixed));
                        allowed
                            .push(json!({"test": file, "reason": "the upstream fix changes it"}));
                    }
                    None => calls.push((
                        "create_file".to_owned(),
                        json!({"file": file, "content": fixed}),
                    )),
                }
            }
        }
        let changes: Vec<Value> = issue
            .source_files
            .iter()
            .map(|f| json!({"location": f, "note": "the upstream fix"}))
            .collect();
        let answer = json!({
            "status": "fixed",
            "cause": format!("Replay of {} (fix {}).", issue.url, issue.fix_commit),
            "changes": changes,
            "repro_test": null,
            "allowed_test_changes": allowed,
            "stop_reason": null
        })
        .to_string();
        Ok(Self { calls, answer })
    }
}

/// An `edit` that replaces all of `file` (`old`) with `fixed`.
fn edit_whole(file: &str, old: &str, fixed: String) -> (String, Value) {
    (
        "edit".to_owned(),
        json!({"file": file, "from": 1, "to": old.lines().count(), "content": fixed}),
    )
}

/// Answers as Light and Core would, from the [`Replay`] of the issue being
/// run. Jobs run one after the other; [`ReplayProvider::start`] sets the
/// next one.
#[derive(Debug, Default)]
pub struct ReplayProvider {
    current: Mutex<Option<Replay>>,
}

const STOPPED: &str = "replay: the upstream fix did not pass the checks";

/// The stopped answer, with the tool errors of the run: an edit the tools
/// refused is a platform problem the dry run is there to find.
fn stopped(messages: &[Message]) -> Vec<ContentBlock> {
    let errors: Vec<String> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                content,
                is_error: true,
                ..
            } => Some(content.lines().take(5).collect::<Vec<_>>().join(" / ")),
            _ => None,
        })
        .collect();
    let reason = if errors.is_empty() {
        STOPPED.to_owned()
    } else {
        format!("{STOPPED}; tool errors: {}", errors.join(" | "))
    };
    vec![ContentBlock::text(
        json!({"status": "stopped", "stop_reason": reason}).to_string(),
    )]
}

impl ReplayProvider {
    pub fn start(&self, replay: Replay) {
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(replay);
    }

    fn core(&self, request: &Request) -> Vec<ContentBlock> {
        let Some(replay) = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return stopped(&request.messages);
        };
        let turns = request
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count();
        // A text message after the first turn is the gate's feedback (or a
        // retry request): the fix is all there is.
        let feedback = turns > 0
            && request.messages.last().is_some_and(|m| {
                m.role == Role::User
                    && m.content
                        .iter()
                        .all(|b| matches!(b, ContentBlock::Text { .. }))
            });
        if feedback {
            return stopped(&request.messages);
        }
        match replay.calls.get(turns) {
            Some((name, input)) => vec![ContentBlock::ToolUse {
                id: format!("replay-{turns}"),
                name: name.clone(),
                input: input.clone(),
            }],
            None => vec![ContentBlock::text(replay.answer)],
        }
    }
}

impl ModelProvider for ReplayProvider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let content = if !request.tools.is_empty() {
            self.core(request)
        } else if request.system.concat().contains("You triage") {
            vec![ContentBlock::text(
                r#"{"kind": "bug", "has_repro_info": false, "suspected_crates": [], "summary": "Replay of the upstream fix."}"#,
            )]
        } else {
            // A gate summary or a commit message: callers fall back to
            // their own text.
            vec![ContentBlock::text("")]
        };
        let stop = if content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        Ok(Response {
            id: "replay".into(),
            model: request.model.clone(),
            content,
            stop_reason: Some(stop),
            usage: Usage::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use aegis_provider::{CachePlan, ToolDefinition};

    use super::*;

    fn request(messages: Vec<Message>) -> Request {
        Request {
            model: "core".into(),
            max_tokens: 100,
            effort: None,
            system: vec![],
            tools: vec![ToolDefinition {
                name: "edit".into(),
                description: String::new(),
                input_schema: json!({}),
            }],
            messages,
            cache: CachePlan::default(),
        }
    }

    #[tokio::test]
    async fn core_writes_the_fix_then_answers_and_stops_on_feedback() {
        let provider = ReplayProvider::default();
        provider.start(Replay {
            calls: vec![("edit".into(), json!({"file": "a.py"}))],
            answer: "{\"status\": \"fixed\"}".into(),
        });
        let issue = Message::user_text("issue");
        let first = provider.send(&request(vec![issue.clone()])).await.unwrap();
        assert!(matches!(&first.content[0], ContentBlock::ToolUse { name, .. } if name == "edit"));
        let result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "replay-0".into(),
                content: "ok".into(),
                is_error: false,
            }],
        };
        let turn = Message {
            role: Role::Assistant,
            content: first.content.clone(),
        };
        let second = provider
            .send(&request(vec![issue.clone(), turn.clone(), result.clone()]))
            .await
            .unwrap();
        assert_eq!(
            second.content,
            vec![ContentBlock::text("{\"status\": \"fixed\"}")]
        );
        let answered = Message {
            role: Role::Assistant,
            content: second.content,
        };
        let third = provider
            .send(&request(vec![
                issue,
                turn,
                result,
                answered,
                Message::user_text("The checks failed"),
            ]))
            .await
            .unwrap();
        let ContentBlock::Text { text } = &third.content[0] else {
            panic!("{third:?}")
        };
        assert!(text.contains(r#""status":"stopped""#), "{text}");
        assert!(text.contains(STOPPED), "{text}");
        assert_eq!(third.usage, Usage::default());
    }
}
