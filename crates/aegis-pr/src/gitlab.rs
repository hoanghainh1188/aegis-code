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
const MAX_PATH_PARTS: usize = 21;

#[derive(Debug, Clone)]
pub struct GitLab {
    client: reqwest::Client,
    api: String,
    /// `group/sub/project`, percent-encoded as one path segment.
    project: String,
    token: String,
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
            token: token.to_owned(),
        })
    }

    /// Comments on issue `iid` of this project. The caller makes `body`
    /// inert.
    pub async fn comment_on_issue(&self, iid: u64, body: &str) -> Result<IssueNote, PrError> {
        let url = format!("{}/projects/{}/issues/{iid}/notes", self.api, self.project);
        let unreached = |e: reqwest::Error| PrError::GitLab {
            status: 0,
            message: e.to_string(),
        };
        let response = self
            .client
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(&json!({ "body": body }))
            .send()
            .await
            .map_err(unreached)?;
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
