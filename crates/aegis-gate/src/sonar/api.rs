//! The Web API calls of the Sonar step, the same on SonarQube Server and
//! SonarQube Cloud: the background task the scanner queued, the quality
//! gate of its analysis, the new issues. Blocking, like the rest of the
//! gate; every wait has a deadline and watches the cancel token.

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::GateError;
use crate::command::CancelToken;

/// One request may take this long; the wait for the analysis has its own
/// deadline on top.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a cancel is noticed while waiting between polls.
const TICK: Duration = Duration::from_millis(50);
/// Issues read for the report; the rest are counted.
pub(crate) const MAX_ISSUES: usize = 100;
/// Characters of an error response kept in the message.
const MAX_BODY_CHARS: usize = 300;

pub(crate) struct Api {
    client: Client,
    url: String,
    token: String,
    organization: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Task {
    pub status: String,
    #[serde(default)]
    pub analysis_id: Option<String>,
    #[serde(default)]
    pub component_key: Option<String>,
    #[serde(default)]
    pub pull_request: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

#[derive(Deserialize)]
struct TaskResponse {
    task: Task,
}

/// How the wait for the analysis ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Waited {
    Done(Task),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Condition {
    pub status: String,
    pub metric_key: String,
    #[serde(default)]
    pub comparator: Option<String>,
    #[serde(default)]
    pub error_threshold: Option<String>,
    #[serde(default)]
    pub actual_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct QualityGate {
    /// `OK`, `ERROR`, `WARN` (old servers) or `NONE`.
    pub status: String,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GateResponse {
    project_status: QualityGate,
}

/// One issue as `/api/issues/search` returns it; the Sonar batch (#17)
/// reads the same type.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    #[serde(default)]
    pub key: String,
    pub rule: String,
    #[serde(default)]
    pub severity: Option<String>,
    /// `<project key>:<path>`.
    pub component: String,
    #[serde(default)]
    pub line: Option<u64>,
    pub message: String,
    #[serde(default)]
    pub text_range: Option<TextRange>,
    /// Secondary locations; a data-flow issue has one flow per path from
    /// source to sink.
    #[serde(default)]
    pub flows: Vec<Flow>,
}

/// 1-based lines, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextRange {
    pub start_line: u64,
    pub end_line: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Flow {
    #[serde(default)]
    pub locations: Vec<FlowLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowLocation {
    /// `<project key>:<path>`.
    pub component: String,
    #[serde(default)]
    pub text_range: Option<TextRange>,
    #[serde(default)]
    pub msg: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issues {
    pub total: u64,
    pub issues: Vec<Issue>,
}

#[derive(Deserialize)]
struct Paging {
    total: u64,
}

#[derive(Deserialize)]
struct IssuesResponse {
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    paging: Option<Paging>,
    #[serde(default)]
    issues: Vec<Issue>,
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(MAX_BODY_CHARS) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_owned(),
    }
}

impl Api {
    pub(crate) fn new(
        url: &str,
        token: &str,
        organization: Option<&str>,
    ) -> Result<Self, GateError> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| GateError::Sonar(format!("cannot build the HTTP client: {e}")))?;
        Ok(Self {
            client,
            url: url.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            organization: organization.map(str::to_owned),
        })
    }

    fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T, GateError> {
        let mut query: Vec<(&str, &str)> = query.to_vec();
        if let Some(org) = &self.organization {
            query.push(("organization", org));
        }
        let response = self
            .client
            .get(format!("{}{path}", self.url))
            .bearer_auth(&self.token)
            .query(&query)
            .send()
            .map_err(|e| GateError::Sonar(format!("GET {path}: {e}")))?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(GateError::Sonar(format!(
                "GET {path}: the server refused the token (HTTP {})",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(GateError::Sonar(format!(
                "GET {path}: HTTP {}: {}",
                status.as_u16(),
                cut(body.trim())
            )));
        }
        response
            .json()
            .map_err(|e| GateError::Sonar(format!("GET {path}: unexpected response: {e}")))
    }

    pub(crate) fn task(&self, id: &str) -> Result<Task, GateError> {
        let r: TaskResponse = self.get("/api/ce/task", &[("id", id)])?;
        Ok(r.task)
    }

    /// Polls the background task until the server has processed the
    /// report, for at most `wait`.
    pub(crate) fn wait_for(
        &self,
        id: &str,
        wait: Duration,
        poll: Duration,
        cancel: &CancelToken,
    ) -> Result<Waited, GateError> {
        let deadline = Instant::now() + wait;
        loop {
            if cancel.is_cancelled() {
                return Ok(Waited::Cancelled);
            }
            let task = self.task(id)?;
            if !matches!(task.status.as_str(), "PENDING" | "IN_PROGRESS") {
                return Ok(Waited::Done(task));
            }
            let next = Instant::now() + poll;
            if next > deadline {
                return Err(GateError::Sonar(format!(
                    "the server did not finish analysis task {id} within {}s",
                    wait.as_secs()
                )));
            }
            while Instant::now() < next {
                if cancel.is_cancelled() {
                    return Ok(Waited::Cancelled);
                }
                std::thread::sleep(TICK.min(next.saturating_duration_since(Instant::now())));
            }
        }
    }

    pub(crate) fn quality_gate(&self, analysis_id: &str) -> Result<QualityGate, GateError> {
        let r: GateResponse = self.get(
            "/api/qualitygates/project_status",
            &[("analysisId", analysis_id)],
        )?;
        Ok(r.project_status)
    }

    /// Open issues of the analysed code that are new: all of a pull
    /// request's, or those in the project's new-code period.
    pub(crate) fn new_issues(
        &self,
        project_key: &str,
        pull_request: Option<&str>,
    ) -> Result<Issues, GateError> {
        let size = MAX_ISSUES.to_string();
        let mut query = vec![
            ("componentKeys", project_key),
            ("resolved", "false"),
            ("ps", size.as_str()),
        ];
        match pull_request {
            Some(pr) => query.push(("pullRequest", pr)),
            None => query.push(("inNewCodePeriod", "true")),
        }
        let r: IssuesResponse = self.get("/api/issues/search", &query)?;
        let total = r
            .paging
            .map(|p| p.total)
            .or(r.total)
            .unwrap_or(r.issues.len() as u64);
        Ok(Issues {
            total,
            issues: r.issues,
        })
    }

    /// Open issues of the project's main branch, page by page, at most
    /// `limit`.
    pub(crate) fn open_issues(&self, project_key: &str, limit: usize) -> Result<Issues, GateError> {
        let size = MAX_ISSUES.to_string();
        let mut issues = Vec::new();
        let mut total = 0;
        for page in 1.. {
            let page = page.to_string();
            let r: IssuesResponse = self.get(
                "/api/issues/search",
                &[
                    ("componentKeys", project_key),
                    ("resolved", "false"),
                    ("ps", size.as_str()),
                    ("p", page.as_str()),
                ],
            )?;
            total = r.paging.map(|p| p.total).or(r.total).unwrap_or(0);
            let got = r.issues.len();
            issues.extend(r.issues);
            if got == 0 || issues.len() >= limit || issues.len() as u64 >= total {
                break;
            }
        }
        issues.truncate(limit);
        Ok(Issues {
            total: total.max(issues.len() as u64),
            issues,
        })
    }
}
