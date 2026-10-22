//! #6: the provider against the real Claude API, to check what the mock
//! tests only assume: prompt caching (5 min and 1 h, with the TTL split in
//! `usage`), the request-level automatic breakpoint, and the shape of the
//! effort parameter. Costs a few cents, so it runs only with
//! `AEGIS_LIVE_API=1` and `ANTHROPIC_API_KEY` set:
//!
//! ```text
//! AEGIS_LIVE_API=1 cargo test -p aegis-provider --test live_api -- --nocapture --test-threads 1
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

use aegis_provider::{
    CachePlan, CacheTtl, ClaudeApi, Effort, Message, ModelProvider, Request, Response,
};

const LIGHT: &str = "claude-haiku-5-5";
const CORE: &str = "claude-sonnet-5-5";
const EXPERT: &str = "claude-opus-5-5";

fn provider() -> Option<ClaudeApi> {
    if std::env::var("AEGIS_LIVE_API").as_deref() != Ok("1") {
        eprintln!("skipping: set AEGIS_LIVE_API=1 (and ANTHROPIC_API_KEY) to call the real API");
        return None;
    }
    Some(ClaudeApi::from_env("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set"))
}

/// A system prompt long enough to be cached (thousands of tokens), and
/// unique to this run so the first request writes the cache rather than
/// reading an entry an earlier run left.
fn long_system() -> String {
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let rule = "Read the issue, find the code involved, write a test that \
                reproduces the bug, make the smallest fix and check it. ";
    format!("Run {run}. {}", rule.repeat(150))
}

fn request(model: &str, system: &str, question: &str, ttl: CacheTtl) -> Request {
    Request {
        model: model.into(),
        max_tokens: 32,
        effort: None,
        system: vec![system.to_owned()],
        tools: vec![],
        messages: vec![Message::user_text(question)],
        cache: CachePlan {
            static_ttl: ttl,
            issue_message: None,
        },
    }
}

fn show(label: &str, r: &Response) {
    let u = &r.usage;
    eprintln!(
        "{label}: input {} output {} cache_write {} (5m {}, 1h {}) cache_read {}",
        u.input_tokens,
        u.output_tokens,
        u.cache_creation_input_tokens,
        u.cache_creation.ephemeral_5m_input_tokens,
        u.cache_creation.ephemeral_1h_input_tokens,
        u.cache_read_input_tokens
    );
}

#[tokio::test]
async fn a_repeated_prefix_is_written_once_then_read() {
    let Some(api) = provider() else { return };
    let system = long_system();

    let first = api
        .send(&request(LIGHT, &system, "Say ok.", CacheTtl::FiveMinutes))
        .await
        .unwrap();
    show("first", &first);
    assert!(
        first.usage.cache_creation_input_tokens > 0,
        "nothing was cached"
    );
    assert_eq!(first.usage.cache_read_input_tokens, 0);
    assert_eq!(
        first.usage.cache_creation.ephemeral_5m_input_tokens,
        first.usage.cache_creation_input_tokens,
        "5-minute writes are reported apart"
    );

    let second = api
        .send(&request(LIGHT, &system, "Say yes.", CacheTtl::FiveMinutes))
        .await
        .unwrap();
    show("second", &second);
    assert!(
        second.usage.cache_read_input_tokens >= first.usage.cache_creation_input_tokens / 2,
        "the second request did not read the cached prefix"
    );
}

#[tokio::test]
async fn one_hour_breakpoints_are_written_and_reported_as_such() {
    let Some(api) = provider() else { return };
    let system = long_system();

    let response = api
        .send(&request(LIGHT, &system, "Say ok.", CacheTtl::OneHour))
        .await
        .unwrap();
    show("1h", &response);
    // BP2 (the system prompt) for an hour; BP4 at the end, five minutes.
    assert!(
        response.usage.cache_creation.ephemeral_1h_input_tokens > 0,
        "no one-hour write was reported"
    );
}

#[tokio::test]
async fn every_effort_level_is_accepted_where_the_config_uses_it() {
    let Some(api) = provider() else { return };
    for (model, effort) in [
        (CORE, Effort::Low),
        (CORE, Effort::Medium),
        (EXPERT, Effort::High),
    ] {
        let mut r = request(
            model,
            "Answer in one word.",
            "Say ok.",
            CacheTtl::FiveMinutes,
        );
        r.effort = Some(effort);
        r.max_tokens = 256;
        let response = api.send(&r).await.unwrap_or_else(|e| {
            panic!("{model} with effort {effort:?} was refused: {e}");
        });
        show(&format!("{model} {effort:?}"), &response);
    }
}
