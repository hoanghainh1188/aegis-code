//! Jira REST API v2: Cloud (`/search/jql`, basic auth with the account's
//! e-mail and API token) or Data Center (`/search`, a personal access
//! token).

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
pub struct Jira {
    client: reqwest::Client,
    /// The site, without a trailing `/`.
    site: String,
    deployment: Deployment,
    user: Option<String>,
    token: String,
    jql: String,
    pr_transition: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Page {
    #[serde(default)]
    issues: Vec<Keyed>,
    /// Cloud.
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
    /// Data Center.
    #[serde(default)]
    total: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct Keyed {
    key: String,
}

#[derive(Debug, Deserialize)]
struct Issue {
    key: String,
    fields: Fields,
}

#[derive(Debug, Deserialize)]
struct Fields {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    attachment: Vec<Attachment>,
}

#[derive(Debug, Deserialize)]
struct Attachment {
    filename: String,
    #[serde(default, rename = "mimeType")]
    mime_type: Option<String>,
    #[serde(default)]
    size: u64,
    content: String,
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

/// `errorMessages` and `errors` of a Jira error.
fn message(v: &Value) -> Option<String> {
    let mut parts: Vec<String> = v
        .get("errorMessages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect();
    if let Some(errors) = v.get("errors").and_then(Value::as_object) {
        parts.extend(
            errors
                .iter()
                .map(|(k, m)| format!("{k}: {}", m.as_str().unwrap_or(""))),
        );
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

impl Jira {
    pub(crate) fn new(config: &JiraConfig, token: &str) -> Result<Self, TrackerError> {
        Ok(Self {
            client: client(JIRA)?,
            site: site_url(&config.url)?.to_owned(),
            deployment: config.deployment,
            user: config.user.clone().filter(|u| !u.trim().is_empty()),
            token: token.to_owned(),
            jql: config.jql.clone(),
            pr_transition: config.pr_transition.clone(),
        })
    }

    fn api(&self, rest: &str) -> String {
        format!("{}/rest/api/2/{rest}", self.site)
    }

    fn authed(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.user {
            Some(user) => request.basic_auth(user, Some(&self.token)),
            None => request.bearer_auth(&self.token),
        }
    }

    fn get(&self, url: &str) -> RequestBuilder {
        self.authed(self.client.get(url))
            .header("Accept", "application/json")
    }

    fn post(&self, url: &str, body: &Value) -> RequestBuilder {
        self.authed(self.client.post(url)).json(body)
    }

    pub(crate) async fn search(&self) -> Result<Vec<String>, TrackerError> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page_size = PAGE.to_string();
            let mut query = vec![
                ("jql", self.jql.as_str()),
                ("fields", "summary"),
                ("maxResults", page_size.as_str()),
            ];
            let start = keys.len().to_string();
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
            let page: Page = http::json(JIRA, request, message).await?;
            let got = page.issues.len();
            let before = keys.len();
            keys.extend(page.issues.into_iter().map(|i| i.key));
            let more = match self.deployment {
                Deployment::Cloud => {
                    token = page.next_page_token;
                    token.is_some()
                }
                Deployment::DataCenter => page.total.is_some_and(|t| keys.len() < t),
            };
            if !more || got == 0 || keys.len() >= MAX_TICKETS || keys.len() == before {
                break;
            }
        }
        keys.truncate(MAX_TICKETS);
        keys.retain(|k| checked_key(k).is_ok());
        Ok(keys)
    }

    pub(crate) async fn ticket(&self, key: &str) -> Result<Ticket, TrackerError> {
        let url = self.api(&format!("issue/{key}"));
        let request = self
            .get(&url)
            .query(&[("fields", "summary,description,attachment")]);
        let issue: Issue = http::json(JIRA, request, message).await?;
        let mut logs = Vec::new();
        for a in &issue.fields.attachment {
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
            logs.push(self.log(a).await?);
        }
        Ok(Ticket {
            url: format!("{}/browse/{}", self.site, issue.key),
            key: issue.key,
            title: issue.fields.summary,
            body: issue.fields.description.unwrap_or_default(),
            logs,
        })
    }

    async fn log(&self, a: &Attachment) -> Result<Log, TrackerError> {
        let response = http::send(JIRA, self.get(&a.content), message).await?;
        let (bytes, cut) = http::read(JIRA, response, MAX_LOG_BYTES).await?;
        Ok(ticket::log(&a.filename, &bytes, cut))
    }

    pub(crate) async fn report(&self, key: &str, report: &Report<'_>) -> Result<(), TrackerError> {
        let comment = self.api(&format!("issue/{key}/comment"));
        http::send(
            JIRA,
            self.post(&comment, &json!({ "body": report.comment })),
            message,
        )
        .await?;
        let Some(pr) = report.pr_url else {
            return Ok(());
        };
        // The same global ID updates the link instead of adding one.
        let link = json!({
            "globalId": format!("aegis-code-pr={pr}"),
            "object": { "url": pr, "title": "Pull request by aegis-code" },
        });
        let remote = self.api(&format!("issue/{key}/remotelink"));
        http::send(JIRA, self.post(&remote, &link), message).await?;
        if let Some(name) = &self.pr_transition {
            self.transition(key, name).await?;
        }
        Ok(())
    }

    async fn transition(&self, key: &str, name: &str) -> Result<(), TrackerError> {
        let url = self.api(&format!("issue/{key}/transitions"));
        let found: Transitions = http::json(JIRA, self.get(&url), message).await?;
        let Some(t) = found
            .transitions
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name.trim()))
        else {
            return Err(TrackerError::Config(format!(
                "jira: {key} has no transition `{name}` now"
            )));
        };
        let body = json!({ "transition": { "id": t.id } });
        http::send(JIRA, self.post(&url, &body), message).await?;
        Ok(())
    }
}
