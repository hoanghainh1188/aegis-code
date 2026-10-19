use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use aegis_orchestrator::{
    COLLAPSED, LoopConfig, LoopEnd, MaxModelCalls, ModelSpec, RetryPolicy, Session, ToolExecutor,
    ToolOutcome, Warning, run_loop,
};
use aegis_provider::{
    CacheTtl, ContentBlock, Message, ModelProvider, ProviderError, Request, Response, StopReason,
    ToolDefinition, Usage,
};
use serde_json::{Value, json};

struct FakeProvider {
    script: Mutex<VecDeque<Result<Response, ProviderError>>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeProvider {
    fn new(script: Vec<Result<Response, ProviderError>>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            requests: Mutex::new(vec![]),
        }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl ModelProvider for FakeProvider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted")
    }
}

struct FakeTools {
    definitions: Vec<ToolDefinition>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl FakeTools {
    fn new() -> Self {
        Self {
            definitions: vec![ToolDefinition {
                name: "open".into(),
                description: "open a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            calls: Mutex::new(vec![]),
        }
    }
}

impl ToolExecutor for FakeTools {
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn call(&self, name: &str, input: &Value) -> ToolOutcome {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_owned(), input.clone()));
        ToolOutcome {
            content: format!("ran {name}"),
            is_error: name == "edit",
        }
    }
}

fn usage(input: u64, cache_read: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: 10,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cache_read,
        ..Usage::default()
    }
}

fn response(content: Vec<ContentBlock>, stop: StopReason, usage: Usage) -> Response {
    Response {
        id: "msg".into(),
        model: "claude-sonnet-5-5".into(),
        content,
        stop_reason: Some(stop),
        usage,
    }
}

fn tool_use(id: &str, name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input: json!({"file": id}),
    }
}

fn wants_tool(id: &str, cache_read: u64) -> Result<Response, ProviderError> {
    Ok(response(
        vec![ContentBlock::text("looking"), tool_use(id, "open")],
        StopReason::ToolUse,
        usage(100, cache_read),
    ))
}

fn done(cache_read: u64) -> Result<Response, ProviderError> {
    Ok(response(
        vec![ContentBlock::text("fixed")],
        StopReason::EndTurn,
        usage(100, cache_read),
    ))
}

fn model() -> ModelSpec {
    ModelSpec {
        id: "claude-sonnet-5-5".into(),
        effort: None,
        max_tokens: 4096,
        context_window: 1_000_000,
    }
}

fn session() -> Session {
    Session::new(
        vec!["sys".into()],
        Message::user_text("issue"),
        CacheTtl::FiveMinutes,
    )
}

fn config() -> LoopConfig {
    LoopConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        },
        ..LoopConfig::default()
    }
}

#[tokio::test]
async fn runs_tool_then_finishes() {
    let provider = FakeProvider::new(vec![wants_tool("t1", 0), done(500)]);
    let tools = FakeTools::new();

    let outcome = run_loop(
        &provider,
        &tools,
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(
        outcome.end,
        LoopEnd::Finished {
            stop_reason: Some(StopReason::EndTurn)
        }
    ));
    assert_eq!(outcome.turns.len(), 2);
    assert_eq!(outcome.turns[0].tool_calls[0].name, "open");
    assert_eq!(
        tools.calls.lock().unwrap()[0],
        ("open".into(), json!({"file": "t1"}))
    );

    let requests = provider.requests();
    assert_eq!(requests[0].tools, tools.definitions);
    let last = requests[1].messages.last().unwrap();
    assert_eq!(
        last.content,
        vec![ContentBlock::tool_result("t1", "ran open")]
    );
    // issue + assistant(tool_use) + user(tool_result) + assistant(final)
    assert_eq!(outcome.session.messages().len(), 4);
    assert!(outcome.warnings.is_empty());
}

#[tokio::test]
async fn answers_every_tool_use_in_one_message_in_order() {
    let provider = FakeProvider::new(vec![
        Ok(response(
            vec![tool_use("a", "open"), tool_use("b", "edit")],
            StopReason::ToolUse,
            usage(100, 0),
        )),
        done(100),
    ]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    let requests = provider.requests();
    let results = &requests[1].messages.last().unwrap().content;
    assert_eq!(
        results,
        &vec![
            ContentBlock::tool_result("a", "ran open"),
            ContentBlock::tool_error("b", "ran edit"),
        ]
    );
    assert!(outcome.turns[0].tool_calls[1].is_error);
}

#[tokio::test]
async fn stop_check_halts_before_the_next_call() {
    let provider = FakeProvider::new(vec![wants_tool("t1", 0), wants_tool("t2", 50)]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(1),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(outcome.end, LoopEnd::Stopped { .. }));
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn retries_retryable_errors_with_backoff() {
    let provider = FakeProvider::new(vec![
        Err(ProviderError::Overloaded),
        Err(ProviderError::RateLimited {
            retry_after_s: Some(7),
        }),
        done(0),
    ]);
    let started = tokio::time::Instant::now();
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(outcome.end, LoopEnd::Finished { .. }));
    assert_eq!(outcome.turns[0].attempts, 3);
    // 1s exponential backoff, then the server's 7s retry-after.
    assert!(started.elapsed() >= Duration::from_secs(8));
}

#[tokio::test(start_paused = true)]
async fn gives_up_after_max_attempts() {
    let provider = FakeProvider::new(vec![
        Err(ProviderError::Overloaded),
        Err(ProviderError::Overloaded),
        Err(ProviderError::Overloaded),
    ]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(
        outcome.end,
        LoopEnd::ProviderFailed(ProviderError::Overloaded)
    ));
    assert_eq!(provider.requests().len(), 3);
    assert!(outcome.turns.is_empty());
}

#[tokio::test]
async fn does_not_retry_permanent_errors() {
    let provider = FakeProvider::new(vec![Err(ProviderError::InvalidRequest("bad".into()))]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(
        outcome.end,
        LoopEnd::ProviderFailed(ProviderError::InvalidRequest(_))
    ));
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn warns_on_cache_miss_from_second_call() {
    let provider = FakeProvider::new(vec![wants_tool("t1", 0), wants_tool("t2", 0), done(300)]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert_eq!(outcome.warnings, vec![Warning::CacheMiss { turn: 2 }]);
}

#[tokio::test]
async fn collapses_old_observations_past_threshold() {
    // Window 1000 → threshold 600. The third response reports 700 prompt tokens.
    let small = ModelSpec {
        context_window: 1000,
        ..model()
    };
    let big = Ok(response(
        vec![tool_use("t3", "open")],
        StopReason::ToolUse,
        usage(200, 500),
    ));
    let provider = FakeProvider::new(vec![
        wants_tool("t1", 0),
        wants_tool("t2", 50),
        big,
        done(50),
    ]);
    let config = LoopConfig {
        keep_recent_observations: 1,
        ..config()
    };
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &small,
        session(),
        &config,
    )
    .await;

    assert_eq!(
        outcome.warnings,
        vec![Warning::Collapsed {
            turn: 3,
            prompt_tokens: 700
        }]
    );
    let requests = provider.requests();
    let outputs: Vec<&str> = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, vec![COLLAPSED, COLLAPSED, "ran open"]);
}

#[tokio::test]
async fn pause_turn_continues_without_tool_results() {
    let provider = FakeProvider::new(vec![
        Ok(response(
            vec![ContentBlock::text("thinking...")],
            StopReason::PauseTurn,
            usage(100, 0),
        )),
        done(100),
    ]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(outcome.end, LoopEnd::Finished { .. }));
    let requests = provider.requests();
    let second = &requests[1];
    assert_eq!(
        second.messages.last().unwrap(),
        &Message::assistant(vec![ContentBlock::text("thinking...")])
    );
}

#[tokio::test]
async fn max_tokens_ends_the_loop() {
    let provider = FakeProvider::new(vec![Ok(response(
        vec![ContentBlock::text("partial")],
        StopReason::MaxTokens,
        usage(100, 0),
    ))]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(
        outcome.end,
        LoopEnd::Finished {
            stop_reason: Some(StopReason::MaxTokens)
        }
    ));
}

#[tokio::test]
async fn truncated_tool_use_is_answered_with_an_error_not_executed() {
    let provider = FakeProvider::new(vec![Ok(response(
        vec![ContentBlock::text("let me"), tool_use("t1", "open")],
        StopReason::MaxTokens,
        usage(100, 0),
    ))]);
    let tools = FakeTools::new();
    let outcome = run_loop(
        &provider,
        &tools,
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(tools.calls.lock().unwrap().is_empty());
    assert!(matches!(
        outcome.end,
        LoopEnd::Finished {
            stop_reason: Some(StopReason::MaxTokens)
        }
    ));
    match &outcome.session.messages().last().unwrap().content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            is_error,
            ..
        } => {
            assert_eq!(tool_use_id, "t1");
            assert!(is_error);
        }
        other => panic!("expected an error tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_response_is_not_stored() {
    let provider = FakeProvider::new(vec![Ok(response(
        vec![],
        StopReason::EndTurn,
        usage(100, 0),
    ))]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert_eq!(outcome.session.messages().len(), 1);
    assert_eq!(outcome.turns.len(), 1);
}

#[tokio::test]
async fn collapses_in_batches_and_skips_the_expected_cache_miss() {
    // Every response is above the threshold; keep 2 recent observations.
    let small = ModelSpec {
        context_window: 1000,
        ..model()
    };
    let over = |id: &str, cache_read: u64| {
        Ok(response(
            vec![tool_use(id, "open")],
            StopReason::ToolUse,
            usage(700, cache_read),
        ))
    };
    let provider = FakeProvider::new(vec![
        over("t1", 0),
        over("t2", 10),
        over("t3", 10), // 3 observations → collapse 1
        over("t4", 0),  // cache rewrite after collapse: no warning
        over("t5", 10), // only 2 new observations since the batch → no collapse
        over("t6", 10), // 2 new → next batch: collapse
        done(10),
    ]);
    let config = LoopConfig {
        keep_recent_observations: 2,
        ..config()
    };
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(20),
        &small,
        session(),
        &config,
    )
    .await;

    let collapses: Vec<usize> = outcome
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            Warning::Collapsed { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect();
    assert_eq!(collapses, vec![3, 5]);
    assert!(
        !outcome
            .warnings
            .iter()
            .any(|warning| matches!(warning, Warning::CacheMiss { .. })),
        "{:?}",
        outcome.warnings
    );
}

#[tokio::test(start_paused = true)]
async fn gives_up_when_retry_after_exceeds_max_delay() {
    let provider = FakeProvider::new(vec![Err(ProviderError::RateLimited {
        retry_after_s: Some(120),
    })]);
    let outcome = run_loop(
        &provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &model(),
        session(),
        &config(),
    )
    .await;

    assert!(matches!(
        outcome.end,
        LoopEnd::ProviderFailed(ProviderError::RateLimited { .. })
    ));
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn escalation_drops_thinking_signed_by_the_previous_model() {
    let thinking = ContentBlock::Thinking {
        thinking: "core reasoning".into(),
        signature: "core-sig".into(),
    };
    let core_provider = FakeProvider::new(vec![Ok(response(
        vec![thinking.clone(), tool_use("t1", "open")],
        StopReason::ToolUse,
        usage(100, 0),
    ))]);
    let core = run_loop(
        &core_provider,
        &FakeTools::new(),
        &MaxModelCalls(1),
        &model(),
        session(),
        &config(),
    )
    .await;

    let expert = ModelSpec {
        id: "claude-opus-5-5".into(),
        ..model()
    };
    let expert_provider = FakeProvider::new(vec![done(0)]);
    run_loop(
        &expert_provider,
        &FakeTools::new(),
        &MaxModelCalls(10),
        &expert,
        core.session,
        &config(),
    )
    .await;

    let requests = expert_provider.requests();
    let sent = &requests[0].messages;
    assert_eq!(sent.len(), 3, "full history carried over");
    assert!(!sent.iter().flat_map(|m| &m.content).any(|b| *b == thinking));
}
