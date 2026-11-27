//! AWS CloudWatch Logs Insights client for [`super::ApmClient`] (#704).
//!
//! Reads one or more log groups for the ticket's correlation ID (or error
//! code) around the time the ticket arrived. Credentials come from the
//! standard AWS chain (the machine's role, or a named profile): no key is
//! stored in the repository's settings. Only `logs:StartQuery` and
//! `logs:GetQueryResults` are called.

use std::time::Duration;

use aws_sdk_cloudwatchlogs::Client;
use aws_sdk_cloudwatchlogs::error::DisplayErrorContext;
use aws_sdk_cloudwatchlogs::types::{QueryStatus, ResultField};
use time::OffsetDateTime;

use crate::TrackerError;
use crate::apm::{CloudWatchConfig, Telemetry, TelemetryQuery};

const PROVIDER: &str = "cloudwatch";
/// Log events read per query: the newest first.
const QUERY_LIMIT: i32 = 20;
/// How long the window reaches past the ticket's time.
const AFTER: time::Duration = time::Duration::hours(1);
/// Polls of `GetQueryResults` before giving up on a query.
const MAX_POLLS: u32 = 30;
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// JSON fields of a structured log line that hold a stack trace.
const STACK_FIELDS: &[&str] = &[
    "/error/stack",
    "/error/stack_trace",
    "/stack",
    "/stack_trace",
    "/stacktrace",
    "/exception",
];
/// JSON fields that hold the release's commit SHA.
const COMMIT_FIELDS: &[&str] = &[
    "/git/commit/sha",
    "/commit_sha",
    "/commit",
    "/release",
    "/version",
];
/// JSON fields that hold the line's message.
const MESSAGE_FIELDS: &[&str] = &["/error/message", "/message", "/msg"];

#[derive(Debug, Clone)]
pub struct CloudWatch {
    region: String,
    profile: Option<String>,
    log_groups: Vec<String>,
    lookback: time::Duration,
    /// Set by tests; otherwise built from the standard chain on each fetch.
    client: Option<Client>,
    poll: Duration,
}

/// One log event of the query's results.
#[derive(Debug, Default)]
struct Event {
    timestamp: Option<String>,
    stream: Option<String>,
    message: String,
}

impl CloudWatch {
    pub(crate) fn new(config: &CloudWatchConfig) -> Self {
        Self {
            region: config.region.trim().to_owned(),
            profile: config.profile.as_deref().map(str::trim).map(str::to_owned),
            log_groups: config
                .log_groups
                .iter()
                .map(|g| g.trim().to_owned())
                .collect(),
            lookback: time::Duration::minutes(i64::from(config.lookback_minutes)),
            client: None,
            poll: POLL_INTERVAL,
        }
    }

    async fn client(&self) -> Client {
        if let Some(client) = &self.client {
            return client.clone();
        }
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(self.region.clone()));
        if let Some(profile) = &self.profile {
            loader = loader.profile_name(profile);
        }
        Client::new(&loader.load().await)
    }

    pub(crate) async fn fetch(
        &self,
        query: &TelemetryQuery,
    ) -> Result<Option<Telemetry>, TrackerError> {
        let Some(term) = search_term(query) else {
            return Ok(None);
        };
        let around = query.around.unwrap_or_else(OffsetDateTime::now_utc);
        let start = around - self.lookback;
        let end = (around + AFTER).min(OffsetDateTime::now_utc().max(around));
        let client = self.client().await;

        let started = client
            .start_query()
            .set_log_group_names(Some(self.log_groups.clone()))
            .start_time(start.unix_timestamp())
            .end_time(end.unix_timestamp())
            .query_string(insights_query(&term))
            .limit(QUERY_LIMIT)
            .send()
            .await
            .map_err(|e| api_error("StartQuery", &DisplayErrorContext(&e).to_string()))?;
        let id = started
            .query_id()
            .ok_or_else(|| api_error("StartQuery", "no query id"))?
            .to_owned();

        let (rows, matched) = self.results(&client, &id).await?;
        let events: Vec<Event> = rows.iter().map(|row| event(row)).collect();
        if events.is_empty() {
            return Ok(None);
        }
        Ok(Some(telemetry(&events, matched)))
    }

    /// Polls the query until it completes: its rows and the records it matched.
    async fn results(
        &self,
        client: &Client,
        id: &str,
    ) -> Result<(Vec<Vec<ResultField>>, Option<u64>), TrackerError> {
        for _ in 0..MAX_POLLS {
            let out = client
                .get_query_results()
                .query_id(id)
                .send()
                .await
                .map_err(|e| api_error("GetQueryResults", &DisplayErrorContext(&e).to_string()))?;
            match out.status() {
                Some(QueryStatus::Complete) => {
                    // Whole records: a count past 2^53 is not a concern.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let matched = out
                        .statistics()
                        .map(|s| s.records_matched())
                        .filter(|n| n.is_finite() && *n > 0.0)
                        .map(|n| n as u64);
                    return Ok((out.results().to_vec(), matched));
                }
                Some(QueryStatus::Scheduled | QueryStatus::Running) | None => {
                    tokio::time::sleep(self.poll).await;
                }
                Some(other) => {
                    return Err(api_error(
                        "GetQueryResults",
                        &format!("query {id} ended {}", other.as_str()),
                    ));
                }
            }
        }
        Err(api_error(
            "GetQueryResults",
            &format!("query {id} did not finish in {MAX_POLLS} polls"),
        ))
    }
}

fn api_error(call: &str, message: &str) -> TrackerError {
    TrackerError::Api {
        tracker: PROVIDER,
        status: 0,
        message: format!("{call}: {message}"),
    }
}

/// The correlation ID, or else the error code: a ticket's title is too
/// vague to scan logs for, and Logs Insights bills by the data it scans.
fn search_term(query: &TelemetryQuery) -> Option<String> {
    let raw = query
        .correlation_id
        .as_deref()
        .or(query.error_code.as_deref())?;
    // Only characters that cannot leave the Insights string literal.
    let term: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        .collect();
    (term.len() >= 4).then_some(term)
}

fn insights_query(term: &str) -> String {
    format!(
        "fields @timestamp, @logStream, @message\n\
         | filter @message like \"{term}\"\n\
         | sort @timestamp desc\n\
         | limit {QUERY_LIMIT}"
    )
}

fn event(row: &[ResultField]) -> Event {
    let mut event = Event::default();
    for field in row {
        let value = field.value().map(str::to_owned);
        match field.field() {
            Some("@timestamp") => event.timestamp = value,
            Some("@logStream") => event.stream = value,
            Some("@message") => event.message = value.unwrap_or_default(),
            _ => {}
        }
    }
    event
}

fn first_string(json: Option<&serde_json::Value>, pointers: &[&str]) -> Option<String> {
    let json = json?;
    pointers
        .iter()
        .find_map(|p| json.pointer(p).and_then(|v| v.as_str()))
        .map(str::to_owned)
}

/// A plain-text line that is itself a stack trace (Java, Node, Python, .NET).
fn looks_like_stack(message: &str) -> bool {
    message.contains("\n\tat ")
        || message.contains("\n    at ")
        || message.contains("Traceback (most recent call last)")
        || message.contains("\n  File \"")
        || message.contains("\n   at ")
}

fn telemetry(events: &[Event], matched: Option<u64>) -> Telemetry {
    let mut summary = None;
    let mut stacktrace = None;
    let mut release_commit = None;
    let mut breadcrumbs = Vec::new();

    for event in events {
        let json = serde_json::from_str::<serde_json::Value>(&event.message)
            .ok()
            .filter(serde_json::Value::is_object);
        let message =
            first_string(json.as_ref(), MESSAGE_FIELDS).unwrap_or_else(|| event.message.clone());
        let line = message.lines().next().unwrap_or("").trim().to_owned();
        if summary.is_none() && !line.is_empty() {
            summary = Some(line.clone());
        }
        if stacktrace.is_none() {
            stacktrace = first_string(json.as_ref(), STACK_FIELDS).or_else(|| {
                (json.is_none() && looks_like_stack(&event.message)).then(|| event.message.clone())
            });
        }
        if release_commit.is_none() {
            release_commit = first_string(json.as_ref(), COMMIT_FIELDS);
        }
        let ts = event.timestamp.as_deref().unwrap_or("-");
        let stream = event.stream.as_deref().unwrap_or("log");
        breadcrumbs.push(format!("[{ts} {stream}] {line}"));
    }

    let count = matched.unwrap_or(events.len() as u64);
    Telemetry {
        provider: PROVIDER,
        summary: summary.unwrap_or_else(|| format!("{count} matching log events")),
        release_commit,
        frequency: Some(format!("{count} matching log events")),
        stacktrace,
        breadcrumbs,
        url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudwatchlogs::config::{BehaviorVersion, Credentials, Region};
    use wiremock::matchers::{body_string_contains, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const JSON: &str = "application/x-amz-json-1.1";

    fn config() -> CloudWatchConfig {
        CloudWatchConfig {
            region: "ap-northeast-1".into(),
            log_groups: vec!["/ecs/checkout".into()],
            profile: None,
            lookback_minutes: 60,
        }
    }

    fn client(server: &MockServer) -> CloudWatch {
        let sdk = aws_sdk_cloudwatchlogs::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("ap-northeast-1"))
            .credentials_provider(Credentials::new("test", "test", None, None, "test"))
            .endpoint_url(server.uri())
            .build();
        CloudWatch {
            client: Some(Client::from_conf(sdk)),
            poll: Duration::ZERO,
            ..CloudWatch::new(&config())
        }
    }

    fn target(call: &str) -> wiremock::matchers::HeaderExactMatcher {
        header("x-amz-target", format!("Logs_20140328.{call}").as_str())
    }

    fn row(ts: &str, stream: &str, message: &str) -> serde_json::Value {
        serde_json::json!([
            {"field": "@timestamp", "value": ts},
            {"field": "@logStream", "value": stream},
            {"field": "@message", "value": message},
            {"field": "@ptr", "value": "CmAKJgoi"}
        ])
    }

    async fn start(server: &MockServer, term: &str) {
        Mock::given(method("POST"))
            .and(target("StartQuery"))
            .and(body_string_contains("/ecs/checkout"))
            .and(body_string_contains(
                format!("like \\\"{term}\\\"").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(r#"{"queryId":"q-1"}"#, JSON))
            .expect(1)
            .mount(server)
            .await;
    }

    fn results(status: &str, rows: &[serde_json::Value], matched: f64) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(
            serde_json::json!({
                "status": status,
                "results": rows,
                "statistics": {"recordsMatched": matched, "recordsScanned": 900.0, "bytesScanned": 1.0e5}
            })
            .to_string(),
            JSON,
        )
    }

    #[tokio::test]
    async fn a_correlation_id_brings_the_stack_trace_and_the_commit_into_the_ticket() {
        let server = MockServer::start().await;
        start(&server, "req-998877").await;
        let structured = serde_json::json!({
            "level": "error",
            "message": "TypeError: cannot read properties of undefined (reading 'total')",
            "error": {"stack": "TypeError: cannot read properties of undefined\n    at lockCart (src/cart.ts:44:13)\n    at checkout (src/checkout.ts:12:5)"},
            "git": {"commit": {"sha": "cafe1234"}},
            "correlation_id": "req-998877"
        })
        .to_string();
        // Still running once, then complete.
        Mock::given(method("POST"))
            .and(target("GetQueryResults"))
            .and(body_string_contains("q-1"))
            .respond_with(results("Running", &[], 0.0))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(target("GetQueryResults"))
            .and(body_string_contains("q-1"))
            .respond_with(results(
                "Complete",
                &[
                    row("2026-11-20 09:00:01.000", "checkout/web/abc", &structured),
                    row(
                        "2026-11-20 08:59:58.000",
                        "checkout/web/abc",
                        "POST /checkout 500 req-998877",
                    ),
                ],
                37.0,
            ))
            .mount(&server)
            .await;

        let query = TelemetryQuery::from_ticket("Checkout 500", "correlation_id: req-998877");
        let telemetry = client(&server).fetch(&query).await.unwrap().unwrap();
        assert_eq!(telemetry.provider, "cloudwatch");
        assert_eq!(
            telemetry.summary,
            "TypeError: cannot read properties of undefined (reading 'total')"
        );
        assert_eq!(telemetry.release_commit.as_deref(), Some("cafe1234"));
        assert_eq!(
            telemetry.frequency.as_deref(),
            Some("37 matching log events")
        );
        let md = telemetry.to_markdown();
        assert!(md.contains("## APM telemetry (cloudwatch)"));
        assert!(md.contains("at lockCart (src/cart.ts:44:13)"));
        assert!(md.contains("[2026-11-20 08:59:58.000 checkout/web/abc] POST /checkout 500"));
    }

    #[tokio::test]
    async fn a_plain_text_trace_is_the_stack_and_no_match_is_none() {
        let server = MockServer::start().await;
        start(&server, "ERR_CART_LOCK").await;
        Mock::given(method("POST"))
            .and(target("GetQueryResults"))
            .respond_with(results(
                "Complete",
                &[row(
                    "2026-11-20 09:00:01.000",
                    "app",
                    "java.lang.IllegalStateException: ERR_CART_LOCK\n\tat com.acme.Cart.lock(Cart.java:44)",
                )],
                1.0,
            ))
            .mount(&server)
            .await;
        let query = TelemetryQuery::from_ticket("Cart lock", "code: ERR_CART_LOCK");
        let telemetry = client(&server).fetch(&query).await.unwrap().unwrap();
        assert_eq!(
            telemetry.stacktrace.as_deref(),
            Some(
                "java.lang.IllegalStateException: ERR_CART_LOCK\n\tat com.acme.Cart.lock(Cart.java:44)"
            )
        );
        assert_eq!(telemetry.release_commit, None);

        let empty = MockServer::start().await;
        start(&empty, "req-445566").await;
        Mock::given(method("POST"))
            .and(target("GetQueryResults"))
            .respond_with(results("Complete", &[], 0.0))
            .mount(&empty)
            .await;
        let query = TelemetryQuery::from_ticket("500", "request_id=req-445566");
        assert_eq!(client(&empty).fetch(&query).await.unwrap(), None);
    }

    #[tokio::test]
    async fn without_an_id_or_code_no_query_runs() {
        let server = MockServer::start().await;
        let query = TelemetryQuery::from_ticket("Checkout is slow", "since this morning");
        assert_eq!(client(&server).fetch(&query).await.unwrap(), None);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_query_or_denied_access_is_an_error() {
        let server = MockServer::start().await;
        start(&server, "req-998877").await;
        Mock::given(method("POST"))
            .and(target("GetQueryResults"))
            .respond_with(results("Failed", &[], 0.0))
            .mount(&server)
            .await;
        let query = TelemetryQuery::from_ticket("500", "trace_id: req-998877");
        let err = client(&server).fetch(&query).await.unwrap_err();
        assert!(err.to_string().contains("ended Failed"), "{err}");

        let denied = MockServer::start().await;
        Mock::given(method("POST"))
            .and(target("StartQuery"))
            .respond_with(ResponseTemplate::new(400).set_body_raw(
                r#"{"__type":"AccessDeniedException","message":"not authorized to perform: logs:StartQuery"}"#,
                JSON,
            ))
            .mount(&denied)
            .await;
        let err = client(&denied).fetch(&query).await.unwrap_err();
        assert!(err.to_string().contains("logs:StartQuery"), "{err}");
    }

    #[tokio::test]
    async fn the_window_reaches_back_from_the_ticket_and_an_hour_past_it() {
        let server = MockServer::start().await;
        start(&server, "req-998877").await;
        Mock::given(method("POST"))
            .and(target("GetQueryResults"))
            .respond_with(results("Complete", &[], 0.0))
            .mount(&server)
            .await;
        let around = time::macros::datetime!(2026-11-20 09:30 UTC);
        let query = TelemetryQuery {
            around: Some(around),
            ..TelemetryQuery::from_ticket("500", "correlation_id: req-998877")
        };
        client(&server).fetch(&query).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["startTime"], (around.unix_timestamp() - 3_600));
        assert_eq!(body["endTime"], (around.unix_timestamp() + 3_600));
        assert_eq!(body["limit"], QUERY_LIMIT);
    }

    #[test]
    fn the_search_term_cannot_leave_its_literal() {
        let query = TelemetryQuery {
            correlation_id: Some("req\"-1) | delete".into()),
            ..TelemetryQuery::default()
        };
        assert_eq!(search_term(&query).as_deref(), Some("req-1delete"));
        assert!(insights_query("req-1").contains("filter @message like \"req-1\""));
    }
}
