//! Backlog API v2 with an API key. The key is a query parameter, as the
//! API asks; errors are built without the URL so it never shows.

use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::Value;

use crate::config::{BacklogConfig, site_url};
use crate::http::{self, client};
use crate::ticket::{self, MAX_LOG_BYTES, MAX_LOGS, checked_key};
use crate::{MAX_TICKETS, Report, Ticket, TrackerError};

const BACKLOG: &str = "Backlog";
/// Backlog's largest `count`.
const PAGE: usize = 100;

#[derive(Debug, Clone)]
pub struct Backlog {
    client: reqwest::Client,
    site: String,
    key: String,
    /// The filter, as query parameters.
    filter: Vec<(&'static str, String)>,
    pr_status_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Keyed {
    #[serde(rename = "issueKey")]
    issue_key: String,
}

#[derive(Debug, Deserialize)]
struct Issue {
    #[serde(rename = "issueKey")]
    issue_key: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Debug, Deserialize)]
struct Attachment {
    id: u64,
    name: String,
    #[serde(default)]
    size: u64,
}

/// `errors[].message` of a Backlog error.
fn message(v: &Value) -> Option<String> {
    let parts: Vec<&str> = v
        .get("errors")?
        .as_array()?
        .iter()
        .filter_map(|e| e.get("message")?.as_str())
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

impl Backlog {
    pub(crate) fn new(config: &BacklogConfig, key: &str) -> Result<Self, TrackerError> {
        let mut filter = Vec::new();
        for (name, ids) in [
            ("projectId[]", &config.project_ids),
            ("statusId[]", &config.status_ids),
            ("issueTypeId[]", &config.issue_type_ids),
            ("categoryId[]", &config.category_ids),
        ] {
            filter.extend(ids.iter().map(|id| (name, id.to_string())));
        }
        if let Some(word) = config.keyword.as_ref().filter(|w| !w.trim().is_empty()) {
            filter.push(("keyword", word.clone()));
        }
        Ok(Self {
            client: client(BACKLOG)?,
            site: site_url(&config.url)?.to_owned(),
            key: key.to_owned(),
            filter,
            pr_status_id: config.pr_status_id,
        })
    }

    fn api(&self, rest: &str) -> String {
        format!("{}/api/v2/{rest}", self.site)
    }

    fn keyed(&self, request: RequestBuilder) -> RequestBuilder {
        request.query(&[("apiKey", &self.key)])
    }

    pub(crate) async fn search(&self) -> Result<Vec<String>, TrackerError> {
        let mut keys = Vec::new();
        loop {
            let offset = keys.len().to_string();
            let paging = [
                ("count", PAGE.to_string()),
                ("offset", offset),
                ("sort", "created".into()),
                ("order", "asc".into()),
            ];
            let request = self
                .keyed(self.client.get(self.api("issues")))
                .query(&self.filter)
                .query(&paging);
            let page: Vec<Keyed> = http::json(BACKLOG, request, message).await?;
            let got = page.len();
            keys.extend(page.into_iter().map(|i| i.issue_key));
            if got < PAGE || keys.len() >= MAX_TICKETS {
                break;
            }
        }
        keys.truncate(MAX_TICKETS);
        keys.retain(|k| checked_key(k).is_ok());
        Ok(keys)
    }

    pub(crate) async fn ticket(&self, key: &str) -> Result<Ticket, TrackerError> {
        let request = self.keyed(self.client.get(self.api(&format!("issues/{key}"))));
        let issue: Issue = http::json(BACKLOG, request, message).await?;
        let mut logs = Vec::new();
        for a in &issue.attachments {
            if logs.len() == MAX_LOGS {
                break;
            }
            if !ticket::is_log(&a.name, None, a.size) {
                continue;
            }
            let url = self.api(&format!("issues/{key}/attachments/{}", a.id));
            let response = http::send(BACKLOG, self.keyed(self.client.get(url)), message).await?;
            let (bytes, cut) = http::read(BACKLOG, response, MAX_LOG_BYTES).await?;
            logs.push(ticket::log(&a.name, &bytes, cut));
        }
        Ok(Ticket {
            url: format!("{}/view/{}", self.site, issue.issue_key),
            key: issue.issue_key,
            title: issue.summary,
            body: issue.description.unwrap_or_default(),
            logs,
        })
    }

    /// The comment holds the PR's URL: Backlog has no remote links.
    pub(crate) async fn report(&self, key: &str, report: &Report<'_>) -> Result<(), TrackerError> {
        let comments = self.api(&format!("issues/{key}/comments"));
        let request = self
            .keyed(self.client.post(comments))
            .form(&[("content", report.comment)]);
        http::send(BACKLOG, request, message).await?;
        if let (Some(_), Some(status)) = (report.pr_url, self.pr_status_id) {
            let request = self
                .keyed(self.client.patch(self.api(&format!("issues/{key}"))))
                .form(&[("statusId", status.to_string())]);
            http::send(BACKLOG, request, message).await?;
        }
        Ok(())
    }
}
