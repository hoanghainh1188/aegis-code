//! Datadog Logs API v2 client for [`super::ApmClient`].

use serde::{Deserialize, Serialize};

use crate::TrackerError;
use crate::apm::{DatadogConfig, Telemetry, TelemetryQuery};
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

    pub(crate) async fn fetch(
        &self,
        query: &TelemetryQuery,
    ) -> Result<Option<Telemetry>, TrackerError> {
        let term = query.search_term();
        if term.is_empty() {
            return Ok(None);
        }
        let mut parts = Vec::new();
        if let Some(svc) = &self.service {
            parts.push(format!("service:{svc}"));
        }
        if let Some(env) = &self.env {
            parts.push(format!("env:{env}"));
        }
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
