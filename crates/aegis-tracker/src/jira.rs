//! Jira REST API v2: Cloud (`/search/jql`, basic auth with the account's
//! e-mail and API token) or Data Center (`/search`, a personal access
//! token). Also provides shared REST helpers for the JSM connector (#453).

use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{Deployment, JiraConfig, site_url};
use crate::http::{self, client};
use crate::ticket::{self, Log, MAX_LOG_BYTES, MAX_LOGS, checked_key};
use crate::{MAX_TICKETS, Report, Ticket, TrackerError};

const JIRA: &str = "Jira";
const PAGE: usize = 100;

#[derive(Debug, Clone)]
pub(crate) struct JiraRest {
    pub(crate) label: &'static str,
    pub(crate) client: reqwest::Client,
    /// The site, without a trailing `/`.
    pub(crate) site: String,
    pub(crate) deployment: Deployment,
    pub(crate) user: Option<String>,
    pub(crate) token: String,
}

#[derive(Debug, Clone)]
pub struct Jira {
    rest: JiraRest,
    jql: String,
    pr_transition: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Page {
    #[serde(default)]
    issues: Vec<Value>,
    /// Cloud.
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
    /// Data Center.
    #[serde(default)]
    total: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawIssue {
    pub(crate) key: String,
    pub(crate) fields: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Attachment {
    pub(crate) filename: String,
    #[serde(default, rename = "mimeType")]
    pub(crate) mime_type: Option<String>,
    #[serde(default)]
    pub(crate) size: u64,
    pub(crate) content: String,
}

#[derive(Debug, Deserialize)]
struct Transitions {
    transitions: Vec<Transition>,
}

#[derive(Debug, Deserialize)]
struct Transition {
    id: String,
    name: String,
}

/// `errorMessages` and `errors` of a Jira / JSM error response.
pub(crate) fn message(v: &Value) -> Option<String> {
    let mut parts: Vec<String> = v
        .get("errorMessages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect();
    if let Some(error_msg) = v.get("errorMessage").and_then(Value::as_str)
        && !error_msg.is_empty()
    {
        parts.push(error_msg.to_owned());
    }
    if let Some(errors) = v.get("errors").and_then(Value::as_object) {
        parts.extend(
            errors
                .iter()
                .map(|(k, m)| format!("{k}: {}", m.as_str().unwrap_or(""))),
        );
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

impl JiraRest {
    pub(crate) fn new(
        label: &'static str,
        url: &str,
        deployment: Deployment,
        user: Option<&str>,
        token: &str,
    ) -> Result<Self, TrackerError> {
        Ok(Self {
            label,
            client: client(label)?,
            site: site_url(url)?.to_owned(),
            deployment,
            user: user
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned),
            token: token.to_owned(),
        })
    }

    pub(crate) fn api(&self, rest: &str) -> String {
        format!("{}/rest/api/2/{rest}", self.site)
    }

    pub(crate) fn authed(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.user {
            Some(user) => request.basic_auth(user, Some(&self.token)),
            None => request.bearer_auth(&self.token),
        }
    }

    pub(crate) fn get(&self, url: &str) -> RequestBuilder {
        self.authed(self.client.get(url))
            .header("Accept", "application/json")
    }

    pub(crate) fn post(&self, url: &str, body: &Value) -> RequestBuilder {
        self.authed(self.client.post(url)).json(body)
    }

    /// Paginated Jira JQL search returning raw issue JSON objects (`key` + requested `fields`).
    pub(crate) async fn search_issues(
        &self,
        jql: &str,
        fields: &str,
    ) -> Result<Vec<Value>, TrackerError> {
        let mut issues = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page_size = PAGE.to_string();
            let mut query = vec![
                ("jql", jql),
                ("fields", fields),
                ("maxResults", page_size.as_str()),
            ];
            let start = issues.len().to_string();
            let request = match self.deployment {
                Deployment::Cloud => {
                    if let Some(t) = &token {
                        query.push(("nextPageToken", t));
                    }
                    self.get(&self.api("search/jql")).query(&query)
                }
                Deployment::DataCenter => {
                    query.push(("startAt", &start));
                    self.get(&self.api("search")).query(&query)
                }
            };
            let page: Page = http::json(self.label, request, message).await?;
            let got = page.issues.len();
            let before = issues.len();
            for item in page.issues {
                let valid = item
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|k| checked_key(k).is_ok());
                if valid {
                    issues.push(item);
                }
            }
            let more = match self.deployment {
                Deployment::Cloud => {
                    token = page.next_page_token;
                    token.is_some()
                }
                Deployment::DataCenter => page.total.is_some_and(|t| issues.len() < t),
            };
            if !more || got == 0 || issues.len() >= MAX_TICKETS || issues.len() == before {
                break;
            }
        }
        issues.truncate(MAX_TICKETS);
        Ok(issues)
    }

    pub(crate) async fn fetch_issue(
        &self,
        key: &str,
        fields: &str,
    ) -> Result<RawIssue, TrackerError> {
        let url = self.api(&format!("issue/{key}"));
        let request = self.get(&url).query(&[("fields", fields)]);
        http::json(self.label, request, message).await
    }

    pub(crate) async fn download_logs(
        &self,
        attachments_val: Option<&Value>,
    ) -> Result<Vec<Log>, TrackerError> {
        let Some(val) = attachments_val else {
            return Ok(Vec::new());
        };
        let attachments: Vec<Attachment> = serde_json::from_value(val.clone()).unwrap_or_default();
        let mut logs = Vec::new();
        for a in &attachments {
            if logs.len() == MAX_LOGS {
                break;
            }
            if !ticket::is_log(&a.filename, a.mime_type.as_deref(), a.size) {
                continue;
            }
            // The token goes with the download: to the site only.
            if !a.content.starts_with(&format!("{}/", self.site)) {
                continue;
            }
            let response = http::send(self.label, self.get(&a.content), message).await?;
            let (bytes, cut) = http::read(self.label, response, MAX_LOG_BYTES).await?;
            logs.push(ticket::log(&a.filename, &bytes, cut));
        }
        Ok(logs)
    }

    pub(crate) async fn post_remotelink(&self, key: &str, pr: &str) -> Result<(), TrackerError> {
        // The same global ID updates the link instead of adding one.
        let link = json!({
            "globalId": format!("aegis-code-pr={pr}"),
            "object": { "url": pr, "title": "Pull request by aegis-code" },
        });
        let remote = self.api(&format!("issue/{key}/remotelink"));
        http::send(self.label, self.post(&remote, &link), message).await?;
        Ok(())
    }

    pub(crate) async fn transition(&self, key: &str, name: &str) -> Result<(), TrackerError> {
        let url = self.api(&format!("issue/{key}/transitions"));
        let found: Transitions = http::json(self.label, self.get(&url), message).await?;
        let Some(t) = found
            .transitions
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name.trim()))
        else {
            return Err(TrackerError::Config(format!(
                "{}: {key} has no transition `{name}` now",
                self.label.to_ascii_lowercase()
            )));
        };
        let body = json!({ "transition": { "id": t.id } });
        http::send(self.label, self.post(&url, &body), message).await?;
        Ok(())
    }
}

impl Jira {
    pub(crate) fn new(config: &JiraConfig, token: &str) -> Result<Self, TrackerError> {
        Ok(Self {
            rest: JiraRest::new(
                JIRA,
                &config.url,
                config.deployment,
                config.user.as_deref(),
                token,
            )?,
            jql: config.jql.clone(),
            pr_transition: config.pr_transition.clone(),
        })
    }

    pub(crate) async fn search(&self) -> Result<Vec<String>, TrackerError> {
        let issues = self.rest.search_issues(&self.jql, "summary").await?;
        Ok(issues
            .into_iter()
            .filter_map(|i| i.get("key").and_then(Value::as_str).map(str::to_owned))
            .collect())
    }

    pub(crate) async fn ticket(&self, key: &str) -> Result<Ticket, TrackerError> {
        let issue = self
            .rest
            .fetch_issue(key, "summary,description,attachment")
            .await?;
        let logs = self
            .rest
            .download_logs(issue.fields.get("attachment"))
            .await?;
        Ok(Ticket {
            url: format!("{}/browse/{}", self.rest.site, issue.key),
            key: issue.key,
            title: issue
                .fields
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            body: issue
                .fields
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            logs,
            priority: None,
            sla_deadline: None,
        })
    }

    pub(crate) async fn report(&self, key: &str, report: &Report<'_>) -> Result<(), TrackerError> {
        let comment = self.rest.api(&format!("issue/{key}/comment"));
        http::send(
            JIRA,
            self.rest.post(&comment, &json!({ "body": report.comment })),
            message,
        )
        .await?;
        let Some(pr) = report.pr_url else {
            return Ok(());
        };
        self.rest.post_remotelink(key, pr).await?;
        if let Some(name) = &self.pr_transition {
            self.rest.transition(key, name).await?;
        }
        Ok(())
    }
}
