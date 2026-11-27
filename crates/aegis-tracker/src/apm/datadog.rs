//! Datadog Logs API v2 client for [`super::ApmClient`].

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::TrackerError;
use crate::apm::sentry::rfc3339;
use crate::apm::{DatadogConfig, ErrorCount, Telemetry, TelemetryQuery};
use crate::config::site_url;
use crate::http;

#[derive(Debug, Clone)]
pub struct Datadog {
    base: String,
    api_key: String,
    app_key: String,
    service: Option<String>,
    env: Option<String>,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct SearchRequest<'a> {
    filter: SearchFilter<'a>,
    sort: &'a str,
    page: SearchPage,
}

#[derive(Serialize)]
struct SearchFilter<'a> {
    query: String,
    from: &'a str,
    to: &'a str,
}

#[derive(Serialize)]
struct SearchPage {
    limit: u32,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<LogEvent>,
}

#[derive(Deserialize)]
struct LogEvent {
    #[serde(default)]
    attributes: LogAttributes,
}

#[derive(Default, Deserialize)]
struct LogAttributes {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    service: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    attributes: serde_json::Value,
}

/// The facet error logs are counted by (Datadog's standard attribute).
const ERROR_FACET: &str = "@error.kind";
/// Error types read per window: the most frequent first.
const MAX_ERROR_TYPES: u32 = 100;

#[derive(Deserialize)]
struct AggregateAnswer {
    #[serde(default)]
    data: AggregateData,
}

#[derive(Default, Deserialize)]
struct AggregateData {
    #[serde(default)]
    buckets: Vec<Bucket>,
}

#[derive(Deserialize)]
struct Bucket {
    #[serde(default)]
    by: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    computes: serde_json::Map<String, serde_json::Value>,
}

fn error_message(v: &serde_json::Value) -> Option<String> {
    v.get("errors")
        .and_then(|e| e.as_array())
        .and_then(|arr| arr.first())
        .and_then(|s| s.as_str())
        .map(str::to_owned)
}

impl Datadog {
    pub(crate) fn new(
        config: &DatadogConfig,
        api_key: &str,
        app_key: &str,
    ) -> Result<Self, TrackerError> {
        Ok(Self {
            base: site_url(&config.url)?.to_owned(),
            api_key: api_key.to_owned(),
            app_key: app_key.to_owned(),
            service: config.service.as_deref().map(str::trim).map(str::to_owned),
            env: config.env.as_deref().map(str::trim).map(str::to_owned),
            client: http::client("datadog")?,
        })
    }

    /// The `service:`/`env:` filters of this connection.
    fn scope(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(svc) = &self.service {
            parts.push(format!("service:{svc}"));
        }
        if let Some(env) = &self.env {
            parts.push(format!("env:{env}"));
        }
        parts
    }

    /// Error logs of `[from, to)` per `@error.kind`, from the logs
    /// aggregation API.
    pub(crate) async fn error_counts(
        &self,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<Vec<ErrorCount>, TrackerError> {
        let mut query = self.scope();
        query.push("status:error".into());
        let body = serde_json::json!({
            "compute": [{"aggregation": "count", "type": "total"}],
            "filter": {"query": query.join(" "), "from": rfc3339(from)?, "to": rfc3339(to)?},
            "group_by": [{
                "facet": ERROR_FACET,
                "limit": MAX_ERROR_TYPES,
                "sort": {"aggregation": "count", "order": "desc", "type": "measure"}
            }]
        });
        let url = format!("{}/api/v2/logs/analytics/aggregate", self.base);
        let req = self
            .client
            .post(url)
            .header("DD-API-KEY", &self.api_key)
            .header("DD-APPLICATION-KEY", &self.app_key)
            .json(&body);
        let answer = http::json::<AggregateAnswer>("datadog", req, error_message).await?;
        Ok(answer
            .data
            .buckets
            .into_iter()
            .filter_map(|bucket| {
                let kind = bucket.by.get(ERROR_FACET)?.as_str()?.trim().to_owned();
                if kind.is_empty() {
                    return None;
                }
                let count = bucket
                    .computes
                    .values()
                    .find_map(serde_json::Value::as_u64)?;
                Some(ErrorCount {
                    fingerprint: format!("datadog:{kind}"),
                    title: kind,
                    count,
                })
            })
            .collect())
    }

    pub(crate) async fn fetch(
        &self,
        query: &TelemetryQuery,
    ) -> Result<Option<Telemetry>, TrackerError> {
        let term = query.search_term();
        if term.is_empty() {
            return Ok(None);
        }
        let mut parts = self.scope();
        parts.push(format!("\"{}\"", term.replace('"', "")));

        let url = format!("{}/api/v2/logs/events/search", self.base);
        let body = SearchRequest {
            filter: SearchFilter {
                query: parts.join(" "),
                from: "now-7d",
                to: "now",
            },
            sort: "-timestamp",
            page: SearchPage { limit: 15 },
        };
        let req = self
            .client
            .post(url)
            .header("DD-API-KEY", &self.api_key)
            .header("DD-APPLICATION-KEY", &self.app_key)
            .json(&body);

        let resp = http::json::<SearchResponse>("datadog", req, error_message).await?;
        if resp.data.is_empty() {
            return Ok(None);
        }

        let count = resp.data.len();
        let mut stacktrace = None;
        let mut release_commit = None;
        let mut summary = String::new();
        let mut breadcrumbs = Vec::new();

        for event in resp.data {
            let attrs = event.attributes;
            if summary.is_empty()
                && let Some(msg) = &attrs.message
            {
                summary = msg.lines().next().unwrap_or(msg).to_owned();
            }
            if stacktrace.is_none() {
                stacktrace = attrs
                    .attributes
                    .pointer("/error/stack")
                    .or_else(|| attrs.attributes.get("stacktrace"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
            }
            if release_commit.is_none() {
                release_commit = attrs
                    .attributes
                    .pointer("/git/commit/sha")
                    .or_else(|| attrs.attributes.get("version"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
            }
            let ts = attrs.timestamp.as_deref().unwrap_or("-");
            let st = attrs.status.as_deref().unwrap_or("info");
            let svc = attrs.service.as_deref().unwrap_or("app");
            let msg = attrs.message.as_deref().unwrap_or("");
            breadcrumbs.push(format!("[{ts} {svc}/{st}] {msg}"));
        }

        Ok(Some(Telemetry {
            provider: "datadog",
            summary: if summary.is_empty() {
                format!("{count} matching log events")
            } else {
                summary
            },
            release_commit,
            frequency: Some(format!("{count} recent matching log events")),
            stacktrace,
            breadcrumbs,
            url: None,
        }))
    }
}
