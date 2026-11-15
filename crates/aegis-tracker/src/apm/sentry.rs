//! Sentry REST API v0 client for [`super::ApmClient`].

use serde::Deserialize;

use crate::TrackerError;
use crate::apm::{SentryConfig, Telemetry, TelemetryQuery};
use crate::config::site_url;
use crate::http;

#[derive(Debug, Clone)]
pub struct Sentry {
    base: String,
    organization: String,
    project: Option<String>,
    token: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct SentryIssue {
    id: String,
    title: String,
    #[serde(default)]
    culprit: Option<String>,
    #[serde(default)]
    count: Option<String>,
    #[serde(default, rename = "userCount")]
    user_count: Option<u64>,
    #[serde(default)]
    permalink: Option<String>,
}

#[derive(Deserialize)]
struct SentryEvent {
    #[serde(default, rename = "release")]
    release: Option<SentryRelease>,
    #[serde(default)]
    entries: Vec<SentryEntry>,
    #[serde(default)]
    tags: Vec<SentryTag>,
}

#[derive(Deserialize)]
struct SentryRelease {
    #[serde(default)]
    version: Option<String>,
}

#[derive(Deserialize)]
struct SentryTag {
    key: String,
    value: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum SentryEntry {
    Exception {
        data: ExceptionData,
    },
    Breadcrumbs {
        data: BreadcrumbsData,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ExceptionData {
    #[serde(default)]
    values: Vec<ExceptionValue>,
}

#[derive(Deserialize)]
struct ExceptionValue {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    stacktrace: Option<StacktraceData>,
}

#[derive(Deserialize)]
struct StacktraceData {
    #[serde(default)]
    frames: Vec<StackFrame>,
}

#[derive(Deserialize)]
struct StackFrame {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    function: Option<String>,
    #[serde(default, rename = "lineNo")]
    line_no: Option<u64>,
}

#[derive(Deserialize)]
struct BreadcrumbsData {
    #[serde(default)]
    values: Vec<Breadcrumb>,
}

#[derive(Deserialize)]
struct Breadcrumb {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

fn error_message(v: &serde_json::Value) -> Option<String> {
    v.get("detail").and_then(|d| d.as_str()).map(str::to_owned)
}

impl Sentry {
    pub(crate) fn new(config: &SentryConfig, token: &str) -> Result<Self, TrackerError> {
        Ok(Self {
            base: site_url(&config.url)?.to_owned(),
            organization: config.organization.trim().to_owned(),
            project: config.project.as_deref().map(str::trim).map(str::to_owned),
            token: token.to_owned(),
            client: http::client("sentry")?,
        })
    }

    pub(crate) async fn fetch(
        &self,
        query: &TelemetryQuery,
    ) -> Result<Option<Telemetry>, TrackerError> {
        let issue = if let Some(id) = &query.sentry_issue_id {
            let url = format!("{}/api/0/issues/{id}/", self.base);
            let req = self.client.get(url).bearer_auth(&self.token);
            http::json::<SentryIssue>("sentry", req, error_message).await?
        } else {
            let term = query.search_term();
            if term.is_empty() {
                return Ok(None);
            }
            let url = format!(
                "{}/api/0/organizations/{}/issues/",
                self.base, self.organization
            );
            let mut req = self
                .client
                .get(url)
                .bearer_auth(&self.token)
                .query(&[("query", term), ("statsPeriod", "14d")]);
            if let Some(proj) = &self.project {
                req = req.query(&[("project", proj.as_str())]);
            }
            let issues = http::json::<Vec<SentryIssue>>("sentry", req, error_message).await?;
            let Some(first) = issues.into_iter().next() else {
                return Ok(None);
            };
            first
        };

        let event_url = format!("{}/api/0/issues/{}/events/latest/", self.base, issue.id);
        let event = http::json::<SentryEvent>(
            "sentry",
            self.client.get(event_url).bearer_auth(&self.token),
            error_message,
        )
        .await?;

        Ok(Some(build_telemetry(issue, event)))
    }
}

fn build_telemetry(issue: SentryIssue, event: SentryEvent) -> Telemetry {
    let summary = match &issue.culprit {
        Some(c) if !c.is_empty() => format!("{} ({c})", issue.title),
        _ => issue.title,
    };
    let frequency = match (issue.count.as_deref(), issue.user_count) {
        (Some(c), Some(u)) => Some(format!("{c} events, {u} users")),
        (Some(c), None) => Some(format!("{c} events")),
        _ => None,
    };
    let release_commit = event
        .tags
        .iter()
        .find(|t| matches!(t.key.as_str(), "commit" | "release"))
        .map(|t| t.value.clone())
        .or_else(|| event.release.and_then(|r| r.version));

    let mut stack_lines = Vec::new();
    let mut breadcrumbs = Vec::new();
    for entry in event.entries {
        match entry {
            SentryEntry::Exception { data } => {
                for exc in data.values {
                    if let (Some(k), Some(v)) = (exc.kind, exc.value) {
                        stack_lines.push(format!("{k}: {v}"));
                    }
                    if let Some(st) = exc.stacktrace {
                        for frame in st.frames.into_iter().rev().take(25) {
                            let file = frame.filename.as_deref().unwrap_or("<unknown>");
                            let func = frame.function.as_deref().unwrap_or("<anonymous>");
                            let line = frame.line_no.map_or_else(String::new, |n| format!(":{n}"));
                            stack_lines.push(format!("  at {func} ({file}{line})"));
                        }
                    }
                }
            }
            SentryEntry::Breadcrumbs { data } => {
                for b in data.values.into_iter().rev().take(10).rev() {
                    if let Some(msg) = b.message.filter(|m| !m.trim().is_empty()) {
                        let cat = b.category.as_deref().unwrap_or("crumb");
                        let lvl = b.level.as_deref().unwrap_or("info");
                        breadcrumbs.push(format!("[{cat}/{lvl}] {msg}"));
                    }
                }
            }
            SentryEntry::Other => {}
        }
    }

    Telemetry {
        provider: "sentry",
        summary,
        release_commit,
        frequency,
        stacktrace: (!stack_lines.is_empty()).then(|| stack_lines.join("\n")),
        breadcrumbs,
        url: issue.permalink,
    }
}
