//! #55: Bedrock `InvokeModel` in `ap-northeast-1` with a `jp.` profile —
//! confirm whether `usage.cache_creation` splits writes by TTL, and that the
//! provider attributes from [`CachePlan`] when it does not.
//!
//! Costs a few cents. Runs only with `AEGIS_LIVE_BEDROCK=1` and AWS credentials
//! that can call Bedrock in Tokyo:
//!
//! ```text
//! AEGIS_LIVE_BEDROCK=1 cargo test -p aegis-provider --features bedrock \
//!   --test live_bedrock -- --nocapture --test-threads 1
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

use aegis_provider::{
    Bedrock, CachePlan, CacheTtl, Message, ModelPolicy, ModelProvider, Request, Response,
};

/// Light tier on the customer-jp config (`config/customer-jp.toml`).
const LIGHT: &str = "jp.anthropic.claude-haiku-5-5";

async fn provider() -> Option<Bedrock> {
    if std::env::var("AEGIS_LIVE_BEDROCK").as_deref() != Ok("1") {
        eprintln!(
            "skipping: set AEGIS_LIVE_BEDROCK=1 (and AWS credentials for ap-northeast-1) \
             to call Bedrock"
        );
        return None;
    }
    Some(Bedrock::from_region("ap-northeast-1", ModelPolicy::JapanOnly).await)
}

fn long_system() -> String {
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let rule = "Read the issue, find the code involved, write a test that \
                reproduces the bug, make the smallest fix and check it. ";
    format!("Run {run}. {}", rule.repeat(150))
}

fn request(system: &str, question: &str, ttl: CacheTtl) -> Request {
    Request {
        model: LIGHT.into(),
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
async fn bedrock_reports_or_attributes_five_minute_cache_writes() {
    let Some(api) = provider().await else {
        return;
    };
    let system = long_system();

    let first = api
        .send(&request(&system, "Say ok.", CacheTtl::FiveMinutes))
        .await
        .unwrap();
    show("bedrock first 5m", &first);
    assert!(
        first.usage.cache_creation_input_tokens > 0,
        "nothing was cached"
    );
    assert_eq!(
        first.usage.cache_creation.ephemeral_5m_input_tokens
            + first.usage.cache_creation.ephemeral_1h_input_tokens,
        first.usage.cache_creation_input_tokens,
        "writes must be attributed (backend split or CachePlan fallback)"
    );
    assert_eq!(
        first.usage.cache_creation.ephemeral_5m_input_tokens,
        first.usage.cache_creation_input_tokens,
        "a 5-minute plan must price the write as 5m"
    );

    let second = api
        .send(&request(&system, "Say yes.", CacheTtl::FiveMinutes))
        .await
        .unwrap();
    show("bedrock second 5m", &second);
    assert!(
        second.usage.cache_read_input_tokens >= first.usage.cache_creation_input_tokens / 2,
        "the second request did not read the cached prefix"
    );
}

#[tokio::test]
async fn bedrock_one_hour_writes_are_attributed() {
    let Some(api) = provider().await else {
        return;
    };
    let system = long_system();

    let response = api
        .send(&request(&system, "Say ok.", CacheTtl::OneHour))
        .await
        .unwrap();
    show("bedrock 1h", &response);
    assert!(
        response.usage.cache_creation_input_tokens > 0,
        "nothing was cached"
    );
    let split = response.usage.cache_creation;
    assert_eq!(
        split.ephemeral_5m_input_tokens + split.ephemeral_1h_input_tokens,
        response.usage.cache_creation_input_tokens,
        "writes must be attributed (backend split or CachePlan fallback)"
    );
    // Static BP is 1h; BP4 is always 5m. Either the backend reports both, or
    // the fallback attributes the whole write to the static 1h TTL.
    assert!(
        split.ephemeral_1h_input_tokens > 0,
        "no one-hour write was attributed"
    );
}
