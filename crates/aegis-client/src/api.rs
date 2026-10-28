//! `aegis-server`'s `/api/v1`, as the client uses it: every answer in the
//! `{data, error, meta}` envelope, the token as a bearer.

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::Login;

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Me {
    pub name: String,
    pub admin: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Repo {
    pub id: i64,
    pub slug: String,
    pub url: String,
    pub default_branch: String,
    pub language: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Job {
    pub id: i64,
    pub repo_id: i64,
    pub issue_ref: String,
    pub source: String,
    pub status: String,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    pub attempts: u32,
    #[serde(default)]
    pub retry_of: Option<i64>,
    #[serde(default)]
    pub review_of: Option<i64>,
    #[serde(default)]
    pub review_round: u32,
    pub created_at: String,
    pub updated_at: String,
}

impl Job {
    /// Whether it ended: done, failed or cancelled.
    pub fn ended(&self) -> bool {
        matches!(self.status.as_str(), "done" | "failed" | "cancelled")
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Run {
    pub attempt: u32,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    data: Value,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

pub struct Client {
    http: reqwest::blocking::Client,
    login: Login,
}

impl Client {
    pub fn new(login: Login) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("aegis-job/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { http, login })
    }

    pub fn server(&self) -> &str {
        &self.login.server
    }

    fn call<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T, String> {
        let url = format!("{}/api/v1/{path}", self.login.server);
        let mut request = self
            .http
            .request(method, &url)
            .bearer_auth(&self.login.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|e| format!("cannot reach {}: {e}", self.login.server))?;
        let status = response.status();
        let text = response.text().map_err(|e| e.to_string())?;
        let envelope: Envelope = serde_json::from_str(&text).map_err(|_| {
            format!(
                "{status} from the server: {}",
                text.chars().take(200).collect::<String>()
            )
        })?;
        if let Some(e) = envelope.error {
            let hint = match status.as_u16() {
                401 => " (log in again: `aegis-job login`)",
                _ => "",
            };
            return Err(format!("{}: {}{hint}", e.code, e.message));
        }
        serde_json::from_value(envelope.data).map_err(|e| format!("unexpected answer: {e}"))
    }

    pub fn me(&self) -> Result<Me, String> {
        self.call(reqwest::Method::GET, "me", None)
    }

    pub fn repos(&self) -> Result<Vec<Repo>, String> {
        self.call(reqwest::Method::GET, "repos", None)
    }

    /// The repo `slug` names, among those the caller has a role in.
    pub fn repo(&self, slug: &str) -> Result<Repo, String> {
        self.repos()?
            .into_iter()
            .find(|r| r.slug.eq_ignore_ascii_case(slug))
            .ok_or_else(|| format!("no repo `{slug}` you have a role in (`aegis-job repos`)"))
    }

    pub fn submit(&self, repo_id: i64, issue_ref: &str, source: &str) -> Result<Job, String> {
        let body =
            serde_json::json!({ "repo_id": repo_id, "issue_ref": issue_ref, "source": source });
        self.call(reqwest::Method::POST, "jobs", Some(&body))
    }

    pub fn job(&self, id: i64) -> Result<Job, String> {
        self.call(reqwest::Method::GET, &format!("jobs/{id}"), None)
    }

    pub fn runs(&self, id: i64) -> Result<Vec<Run>, String> {
        self.call(reqwest::Method::GET, &format!("jobs/{id}/runs"), None)
    }

    /// The newest jobs first, of `repo_id` and in `status` when given.
    pub fn jobs(
        &self,
        repo_id: Option<i64>,
        status: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Job>, String> {
        let mut query = vec![format!("limit={limit}")];
        if let Some(r) = repo_id {
            query.push(format!("repo_id={r}"));
        }
        if let Some(s) = status {
            query.push(format!("status={s}"));
        }
        self.call(
            reqwest::Method::GET,
            &format!("jobs?{}", query.join("&")),
            None,
        )
    }

    pub fn cancel(&self, id: i64) -> Result<Job, String> {
        self.call(reqwest::Method::POST, &format!("jobs/{id}/cancel"), None)
    }

    pub fn retry(&self, id: i64) -> Result<Job, String> {
        self.call(reqwest::Method::POST, &format!("jobs/{id}/retry"), None)
    }

    /// The next review round of the PR job `id` opened (#243).
    pub fn review(&self, id: i64) -> Result<Job, String> {
        self.call(reqwest::Method::POST, &format!("jobs/{id}/review"), None)
    }
}
