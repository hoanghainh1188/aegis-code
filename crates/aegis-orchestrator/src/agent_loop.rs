//! The tool-use loop for one model on one session (ADR 0001).
//!
//! Runs until the model ends its turn, a [`StopCheck`] says stop, or the
//! provider fails for good. Escalation and budget rules live in
//! `aegis-router`, which plugs in through [`StopCheck`].

use std::future::Future;
use std::time::Duration;

use aegis_provider::{
    ContentBlock, Message, ModelProvider, ProviderError, Response, StopReason, ToolDefinition,
    Usage,
};
use serde_json::Value;

use crate::{ModelSpec, Session};

/// Result of running one ACI tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

/// The tools the agent may call. The list must stay fixed for a whole
/// session, or the cached prefix breaks (design §4.4).
pub trait ToolExecutor {
    fn definitions(&self) -> &[ToolDefinition];

    fn call(&self, name: &str, input: &Value) -> impl Future<Output = ToolOutcome> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRecord {
    pub name: String,
    pub is_error: bool,
}

/// One model call, as the router and ledger need to see it.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRecord {
    pub model: String,
    pub usage: Usage,
    pub stop_reason: Option<StopReason>,
    pub tool_calls: Vec<ToolCallRecord>,
    /// Provider attempts for this call, including retries.
    pub attempts: u32,
}

/// Decides, before each model call, whether the loop must stop.
///
/// `turns` holds only the calls of the current [`run_loop`]. Limits that span
/// several loops (the $4 / 20-call cap across Core and Expert, design §5.3)
/// must add the turns of earlier loops themselves. Failed provider calls are
/// not billed and do not appear as turns.
pub trait StopCheck {
    /// Returns the reason to stop, or `None` to continue.
    fn check(&self, turns: &[TurnRecord]) -> Option<String>;
}

/// Stops after a fixed number of model calls in this loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxModelCalls(pub usize);

impl StopCheck for MaxModelCalls {
    fn check(&self, turns: &[TurnRecord]) -> Option<String> {
        (turns.len() >= self.0).then(|| format!("reached {} model calls", self.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopConfig {
    pub retry: RetryPolicy,
    /// Collapse old observations when the prompt exceeds this share of the
    /// context window (design §3.3).
    pub collapse_threshold: f64,
    /// Observation turns kept intact when collapsing.
    pub keep_recent_observations: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            retry: RetryPolicy {
                max_attempts: 3,
                base_delay: Duration::from_secs(2),
                max_delay: Duration::from_secs(60),
            },
            collapse_threshold: 0.6,
            keep_recent_observations: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// From the second call on, nothing was read from cache: something in
    /// the prefix changed (design §4.1).
    CacheMiss { turn: usize },
    /// Old observations were collapsed; the next call rewrites the cache once.
    Collapsed { turn: usize, prompt_tokens: u64 },
}

#[derive(Debug)]
pub enum LoopEnd {
    /// The model stopped without asking for a tool.
    Finished { stop_reason: Option<StopReason> },
    /// The [`StopCheck`] ended the loop before the next call.
    Stopped { reason: String },
    /// The provider failed and retries did not help.
    ProviderFailed(ProviderError),
}

#[derive(Debug)]
pub struct LoopOutcome {
    pub session: Session,
    pub turns: Vec<TurnRecord>,
    pub warnings: Vec<Warning>,
    pub end: LoopEnd,
}

pub async fn run_loop<P, T, S>(
    provider: &P,
    tools: &T,
    stop: &S,
    model: &ModelSpec,
    session: Session,
    config: &LoopConfig,
) -> LoopOutcome
where
    P: ModelProvider + Sync,
    T: ToolExecutor + Sync,
    S: StopCheck,
{
    let mut state = LoopState::new(session.prepared_for(&model.id));
    let end = loop {
        if let Some(reason) = stop.check(&state.turns) {
            break LoopEnd::Stopped { reason };
        }
        let request = state.session.request(model, tools.definitions());
        let (response, attempts) = match send_with_retry(provider, &request, &config.retry).await {
            Ok(sent) => sent,
            Err(error) => break LoopEnd::ProviderFailed(error),
        };
        state.check_cache(&response.usage);
        let prompt_tokens = prompt_tokens(&response.usage);
        if let Some(end) = state.absorb(response, attempts, model, tools).await {
            break end;
        }
        state.maybe_collapse(prompt_tokens, model, config);
    };
    state.finish(end)
}

struct LoopState {
    session: Session,
    turns: Vec<TurnRecord>,
    warnings: Vec<Warning>,
    /// Observation count right after the last collapse (batching, §3.3).
    collapsed_at: Option<usize>,
    /// The next call is expected to rewrite the cache (after a collapse).
    rewrite_expected: bool,
}

impl LoopState {
    fn new(session: Session) -> Self {
        Self {
            session,
            turns: Vec::new(),
            warnings: Vec::new(),
            collapsed_at: None,
            rewrite_expected: false,
        }
    }

    fn check_cache(&mut self, usage: &Usage) {
        let turn = self.turns.len() + 1;
        if turn > 1 && !self.rewrite_expected && usage.cache_read_input_tokens == 0 {
            self.warnings.push(Warning::CacheMiss { turn });
        }
        self.rewrite_expected = false;
    }

    /// Records the response and runs the tools it asks for. Returns the
    /// loop end when the model did not ask for (completed) tool calls.
    async fn absorb<T>(
        &mut self,
        response: Response,
        attempts: u32,
        model: &ModelSpec,
        tools: &T,
    ) -> Option<LoopEnd>
    where
        T: ToolExecutor + Sync,
    {
        let Response {
            content,
            stop_reason,
            usage,
            ..
        } = response;
        let requested = tool_uses(&content);
        let executable = stop_reason == Some(StopReason::ToolUse) && !requested.is_empty();
        let stored = !content.is_empty();
        let (results, tool_calls) = if !stored {
            (Vec::new(), Vec::new())
        } else {
            let session = std::mem::take(&mut self.session);
            self.session = session.with_assistant(&model.id, Message::assistant(content));
            if executable {
                execute_tools(tools, &requested).await
            } else {
                (unanswered(&requested, stop_reason), Vec::new())
            }
        };
        self.turns.push(TurnRecord {
            model: model.id.clone(),
            usage,
            stop_reason,
            tool_calls,
            attempts,
        });
        if !results.is_empty() {
            let session = std::mem::take(&mut self.session);
            self.session = session.with_message(Message::user(results));
        }
        // An empty pause_turn reply was not stored; resending would repeat it.
        let continues = executable || (stop_reason == Some(StopReason::PauseTurn) && stored);
        (!continues).then_some(LoopEnd::Finished { stop_reason })
    }

    /// Collapses old observations in batches once the prompt passes the
    /// threshold: after one collapse, waits for `keep_recent_observations`
    /// new observations before the next, so the cache is rewritten once
    /// per batch rather than every turn.
    fn maybe_collapse(&mut self, prompt_tokens: u64, model: &ModelSpec, config: &LoopConfig) {
        let keep = config.keep_recent_observations;
        let observations = self.session.observation_count();
        let batch_ready = self
            .collapsed_at
            .is_none_or(|at| observations.saturating_sub(at) >= keep);
        if !batch_ready
            || !exceeds_threshold(
                prompt_tokens,
                model.context_window,
                config.collapse_threshold,
            )
        {
            return;
        }
        let session = std::mem::take(&mut self.session);
        let (collapsed, changed) = session.collapse_observations(keep);
        self.session = collapsed;
        if changed > 0 {
            self.collapsed_at = Some(observations);
            self.rewrite_expected = true;
            self.warnings.push(Warning::Collapsed {
                turn: self.turns.len(),
                prompt_tokens,
            });
        }
    }

    fn finish(self, end: LoopEnd) -> LoopOutcome {
        LoopOutcome {
            session: self.session,
            turns: self.turns,
            warnings: self.warnings,
            end,
        }
    }
}

/// Error results for tool calls the model started but did not finish
/// (for example cut off by `max_tokens`), so the history stays valid.
fn unanswered(requested: &[ToolUse], stop_reason: Option<StopReason>) -> Vec<ContentBlock> {
    requested
        .iter()
        .map(|call| {
            ContentBlock::tool_error(
                call.id.clone(),
                format!(
                    "Not executed: the response ended ({stop_reason:?}) before this call completed."
                ),
            )
        })
        .collect()
}

async fn send_with_retry<P>(
    provider: &P,
    request: &aegis_provider::Request,
    policy: &RetryPolicy,
) -> Result<(Response, u32), ProviderError>
where
    P: ModelProvider + Sync,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match provider.send(request).await {
            Ok(response) => return Ok((response, attempt)),
            Err(error) if error.is_retryable() && attempt < max_attempts => {
                let Some(delay) = retry_delay(&error, attempt, policy) else {
                    return Err(error);
                };
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Backoff before the next attempt, or `None` when the server asks for a
/// longer wait than the policy allows (retrying early would only fail again).
fn retry_delay(error: &ProviderError, attempt: u32, policy: &RetryPolicy) -> Option<Duration> {
    match error {
        ProviderError::RateLimited {
            retry_after_s: Some(seconds),
        } => {
            let requested = Duration::from_secs(*seconds);
            (requested <= policy.max_delay).then_some(requested)
        }
        _ => Some(
            policy
                .base_delay
                .saturating_mul(2_u32.saturating_pow(attempt - 1))
                .min(policy.max_delay),
        ),
    }
}

fn prompt_tokens(usage: &Usage) -> u64 {
    usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens
}

fn exceeds_threshold(prompt_tokens: u64, context_window: u64, threshold: f64) -> bool {
    // Token counts stay far below 2^53, so the f64 conversion is exact.
    context_window > 0 && prompt_tokens as f64 > context_window as f64 * threshold
}

struct ToolUse {
    id: String,
    name: String,
    input: Value,
}

fn tool_uses(content: &[ContentBlock]) -> Vec<ToolUse> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Runs the requested tools in order; edits must not race each other.
async fn execute_tools<T>(
    tools: &T,
    requested: &[ToolUse],
) -> (Vec<ContentBlock>, Vec<ToolCallRecord>)
where
    T: ToolExecutor + Sync,
{
    let mut results = Vec::with_capacity(requested.len());
    let mut records = Vec::with_capacity(requested.len());
    for call in requested {
        let outcome = tools.call(&call.name, &call.input).await;
        records.push(ToolCallRecord {
            name: call.name.clone(),
            is_error: outcome.is_error,
        });
        results.push(ContentBlock::ToolResult {
            tool_use_id: call.id.clone(),
            content: outcome.content,
            is_error: outcome.is_error,
        });
    }
    (results, records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_model_calls_stops_at_limit() {
        let turn = TurnRecord {
            model: "m".into(),
            usage: Usage::default(),
            stop_reason: None,
            tool_calls: vec![],
            attempts: 1,
        };
        assert!(
            MaxModelCalls(2)
                .check(std::slice::from_ref(&turn))
                .is_none()
        );
        assert!(MaxModelCalls(2).check(&[turn.clone(), turn]).is_some());
    }
}
