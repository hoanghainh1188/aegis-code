//! Comments on a GitLab issue (REST API v4, Notes): the job's status for
//! whoever asked for it there (#168). GitLab.com or a self-hosted instance,
//! by its API root.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::PrError;
use crate::github::{checked_api, plain_name};

/// A request to GitLab still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// GitLab subgroups nest up to 20 deep, under one top-level group.
pub(crate) const MAX_PATH_PARTS: usize = 21;

#[derive(Debug, Clone)]
pub struct GitLab {
    client: reqwest::Client,
    api: String,
    /// `group/sub/project`, percent-encoded as one path segment.
    project: String,
    /// `group/sub/project` as it is.
    path: String,
    token: String,
}

/// An issue of the project (#252).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitLabIssue {
    pub iid: u64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub web_url: String,
}

/// A merge request opened by the agent (#252).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MergeRequest {
    pub iid: u64,
    pub web_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IssueNote {
    pub id: u64,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(alias = "error")]
    message: serde_json::Value,
}

impl GitLab {
    /// `api` is `https://gitlab.com/api/v4` or a self-hosted instance's;
    /// `project` is its `path_with_namespace`.
    pub fn new(api: &str, project: &str, token: &str) -> Result<Self, PrError> {
        let parts: Vec<&str> = project.split('/').collect();
        if parts.len() < 2 || parts.len() > MAX_PATH_PARTS || !parts.iter().all(|p| plain_name(p)) {
            return Err(PrError::GitLab {
                status: 0,
                message: format!("`{project}` is not a project path"),
            });
        }
        let api = checked_api(api)?;
        let client = reqwest::Client::builder()
            .user_agent("aegis-code")
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| PrError::GitLab {
                status: 0,
                message: e.to_string(),
            })?;
        Ok(Self {
            client,
            api,
            project: parts.join("%2F"),
            path: project.to_owned(),
            token: token.to_owned(),
        })
    }

    /// Where the agent's branch for this project is pushed: the web host of
    /// the API root (`https://gitlab.com/api/v4` → `gitlab.com`), same
    /// project, same token (#252).
    pub fn remote(&self) -> Result<crate::Remote, PrError> {
        let rest = self
            .api
            .split_once("://")
            .map_or(self.api.as_str(), |(_, r)| r);
        let host = rest.split('/').next().unwrap_or(rest);
        crate::Remote::gitlab(host, &self.path, &self.token)
    }

    /// Issue `iid` of this project.
    pub async fn issue(&self, iid: u64) -> Result<GitLabIssue, PrError> {
        self.call(reqwest::Method::GET, &format!("issues/{iid}"), None)
            .await
    }

    /// Opens a merge request from `head` into `base`, a draft when `draft`
    /// (GitLab marks it by the title).
    pub async fn open_merge_request(
        &self,
        head: &crate::AgentBranch,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<MergeRequest, PrError> {
        let title = if draft {
            format!("Draft: {title}")
        } else {
            title.to_owned()
        };
        let request = json!({
            "source_branch": head.name(),
            "target_branch": base,
            "title": title,
            "description": body,
            "remove_source_branch": false,
        });
        self.call(reqwest::Method::POST, "merge_requests", Some(&request))
            .await
    }

    /// Comments on issue `iid` of this project. The caller makes `body`
    /// inert.
    pub async fn comment_on_issue(&self, iid: u64, body: &str) -> Result<IssueNote, PrError> {
        self.call(
            reqwest::Method::POST,
            &format!("issues/{iid}/notes"),
            Some(&json!({ "body": body })),
        )
        .await
    }

    /// `method` on `projects/<project>/<path>`, with `body` as JSON.
    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, PrError> {
        let url = format!("{}/projects/{}/{path}", self.api, self.project);
        let unreached = |e: reqwest::Error| PrError::GitLab {
            status: 0,
            message: e.to_string(),
        };
        let mut request = self
            .client
            .request(method, &url)
            .header("PRIVATE-TOKEN", &self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(unreached)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(unreached)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<ApiError>(&text)
                .map(|e| match e.message {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                })
                .unwrap_or_else(|_| text.chars().take(200).collect());
            return Err(PrError::GitLab { status, message });
        }
        serde_json::from_str(&text).map_err(|e| PrError::GitLab {
            status,
            message: format!("unexpected reply: {e}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agents_branch_goes_to_the_web_host_of_the_api_root() {
        for (api, url) in [
            (
                "https://gitlab.com/api/v4",
                "https://gitlab.com/acme/app.git",
            ),
            (
                "https://git.acme.example/api/v4/",
                "https://git.acme.example/acme/app.git",
            ),
        ] {
            let gitlab = GitLab::new(api, "acme/app", "t").unwrap();
            assert_eq!(gitlab.remote().unwrap().url(), url, "{api}");
        }
    }
}
