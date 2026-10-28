//! Opens the pull request on GitHub (REST API). The PR is always opened in
//! the configured repo, from a branch of that same repo: the agent works on
//! a fork and has no way to target another repository. The same client,
//! made for the repository of an issue, comments on that issue (#25).

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::{AgentBranch, PrError, Remote};

const API_VERSION: &str = "2022-11-28";

/// A request to GitHub still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct GitHub {
    client: reqwest::Client,
    api: String,
    owner: String,
    repo: String,
    token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    #[serde(rename = "html_url")]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IssueComment {
    pub id: u64,
    #[serde(rename = "html_url")]
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    message: String,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

impl ApiError {
    /// `message` plus the details GitHub gives for a 422.
    fn text(self) -> String {
        let details: Vec<String> = self
            .errors
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()).map(str::to_owned))
            .collect();
        if details.is_empty() {
            self.message
        } else {
            format!("{} ({})", self.message, details.join("; "))
        }
    }
}

/// `https://…`, or `http://` to this machine (tests). The bearer token
/// goes there.
pub(crate) fn checked_api(api: &str) -> Result<String, PrError> {
    let api = api.trim_end_matches('/');
    let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| api.starts_with(p));
    if api.starts_with("https://") || local {
        Ok(api.to_owned())
    } else {
        Err(PrError::InvalidRemote(format!(
            "API root `{api}` must use https"
        )))
    }
}

pub(crate) fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && s != "."
        && s != ".."
}

impl GitHub {
    /// `api` is `https://api.github.com` (or a GitHub Enterprise API root);
    /// `owner/repo` is the fork the agent works on.
    pub fn new(api: &str, owner: &str, repo: &str, token: &str) -> Result<Self, PrError> {
        if !plain_name(owner) || !plain_name(repo) {
            return Err(PrError::GitHub {
                status: 0,
                message: format!("`{owner}/{repo}` is not a repository name"),
            });
        }
        let api = checked_api(api)?;
        let client = reqwest::Client::builder()
            .user_agent("aegis-code")
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| PrError::Http(e.to_string()))?;
        Ok(Self {
            client,
            api,
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            token: token.to_owned(),
        })
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) fn repo(&self) -> &str {
        &self.repo
    }

    /// Where the agent's branch for this repo is pushed: the web host of
    /// the API root (`api.github.com` → `github.com`, `<host>/api/v3` →
    /// `<host>`), same owner and repo, same token.
    pub fn remote(&self) -> Result<Remote, PrError> {
        let rest = self
            .api
            .split_once("://")
            .map_or(self.api.as_str(), |(_, r)| r);
        let host = rest.split('/').next().unwrap_or(rest);
        let host = host.strip_prefix("api.").unwrap_or(host);
        Remote::https(host, &self.owner, &self.repo, &self.token)
    }

    pub(crate) async fn get(&self, path: &str) -> Result<serde_json::Value, PrError> {
        let url = format!("{}/repos/{}/{}/{path}", self.api, self.owner, self.repo);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        if !status.is_success() {
            let message = serde_json::from_str::<ApiError>(&text)
                .map(ApiError::text)
                .unwrap_or_else(|_| text.chars().take(200).collect());
            return Err(PrError::GitHub {
                status: status.as_u16(),
                message,
            });
        }
        serde_json::from_str(&text).map_err(|e| PrError::GitHub {
            status: status.as_u16(),
            message: format!("unexpected reply: {e}"),
        })
    }

    /// Lines the reviewers changed on top of the agent's commit: additions
    /// plus deletions from the PR's first commit (the agent's) to its last
    /// (design §8: "accepted" allows at most 10).
    pub async fn review_edits(&self, number: u64) -> Result<u32, PrError> {
        let commits = self
            .get(&format!("pulls/{number}/commits?per_page=100"))
            .await?;
        let shas: Vec<&str> = commits
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["sha"].as_str())
            .collect();
        let (Some(first), Some(last)) = (shas.first(), shas.last()) else {
            return Ok(0);
        };
        if first == last {
            return Ok(0);
        }
        let compare = self.get(&format!("compare/{first}...{last}")).await?;
        let lines: u64 = compare["files"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| f["additions"].as_u64().unwrap_or(0) + f["deletions"].as_u64().unwrap_or(0))
            .sum();
        Ok(u32::try_from(lines).unwrap_or(u32::MAX))
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, PrError> {
        let url = format!("{}/repos/{}/{}/{path}", self.api, self.owner, self.repo);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        if status.is_success() {
            serde_json::from_str(&text).map_err(|e| PrError::GitHub {
                status: status.as_u16(),
                message: format!("unexpected reply: {e}"),
            })
        } else {
            let message = serde_json::from_str::<ApiError>(&text)
                .map(ApiError::text)
                .unwrap_or_else(|_| text.chars().take(200).collect());
            Err(PrError::GitHub {
                status: status.as_u16(),
                message,
            })
        }
    }

    /// Opens a PR from `head` into `base`.
    pub async fn open_pull_request(
        &self,
        head: &AgentBranch,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<PullRequest, PrError> {
        self.post(
            "pulls",
            &json!({
                "title": title,
                "head": head.name(),
                "base": base,
                "body": body,
                "draft": draft,
                "maintainer_can_modify": false,
            }),
        )
        .await
    }

    /// What `login` may do in this repo (`admin`, `write`, `read`,
    /// `none`): `GET collaborators/{login}/permission`. Not a
    /// collaborator is `none`.
    pub async fn permission(&self, login: &str) -> Result<String, PrError> {
        if !plain_name(login) {
            return Ok("none".into());
        }
        match self.get(&format!("collaborators/{login}/permission")).await {
            Ok(v) => Ok(v["permission"].as_str().unwrap_or("none").to_owned()),
            Err(PrError::GitHub { status: 404, .. }) => Ok("none".into()),
            Err(e) => Err(e),
        }
    }

    /// Comments on issue `number` of this repo: the job's status for whoever
    /// asked for it there (#25). The caller makes `body` inert.
    pub async fn comment_on_issue(&self, number: u64, body: &str) -> Result<IssueComment, PrError> {
        self.post(
            &format!("issues/{number}/comments"),
            &json!({ "body": body }),
        )
        .await
    }
}
