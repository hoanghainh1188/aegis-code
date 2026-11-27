//! Opens the pull request on GitHub (REST API). The PR is always opened in
//! the configured repo, from a branch of that same repo: the agent works on
//! a fork and has no way to target another repository. The same client,
//! made for the repository of an issue, comments on that issue (#25).

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::app::{TokenSource, TokenUse};
use crate::{AgentBranch, PrError, Remote};

const API_VERSION: &str = "2022-11-28";

/// A request to GitHub still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct GitHub {
    client: reqwest::Client,
    api: String,
    owner: String,
    repo: String,
    /// Asked for a token at each request (ADR 0017 §3): a fixed one on
    /// the token path, a fresh installation token on the app path.
    token: TokenSource,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHub")
            .field("api", &self.api)
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
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

/// An issue the platform opened (#706): its number and web address.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OpenedIssue {
    pub number: u64,
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
/// goes there, so the host is read as a URL's, not by its first letters
/// (`http://localhost.example` is not this machine, #376), and a URL with
/// a user or password in it is refused.
pub(crate) fn checked_api(api: &str) -> Result<String, PrError> {
    let api = api.trim_end_matches('/');
    let refused = || PrError::InvalidRemote(format!("API root `{api}` must use https"));
    let url = reqwest::Url::parse(api).map_err(|_| refused())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refused());
    }
    let host = url.host_str().unwrap_or_default();
    let local = url.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    if (url.scheme() == "https" && !host.is_empty()) || local {
        Ok(api.to_owned())
    } else {
        Err(refused())
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
        Self::with_source(api, owner, repo, TokenSource::Static(token.to_owned()))
    }

    /// The same, with its token from `source`; an app source must be for
    /// this very repo.
    pub fn with_source(
        api: &str,
        owner: &str,
        repo: &str,
        source: TokenSource,
    ) -> Result<Self, PrError> {
        if matches!(source, TokenSource::Entra(_)) {
            return Err(PrError::App(
                "a service principal reaches Azure DevOps repos only".into(),
            ));
        }
        if let TokenSource::App {
            owner: o, repo: r, ..
        } = &source
            && (o != owner || r != repo)
        {
            return Err(PrError::App(format!(
                "a token source for `{o}/{r}` cannot serve `{owner}/{repo}`"
            )));
        }
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
            token: source,
        })
    }

    /// This client with its token for another use (ADR 0017 §3): on the
    /// app path its requests then carry only that use's permissions.
    pub fn for_use(&self, use_: TokenUse) -> Self {
        Self {
            token: self.token.for_use(use_),
            ..self.clone()
        }
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) fn repo(&self) -> &str {
        &self.repo
    }

    /// Where the agent's branch for this repo is pushed: the web host of
    /// the API root (`api.github.com` → `github.com`, `<host>/api/v3` →
    /// `<host>`), same owner and repo, same token. Only for a fixed token:
    /// on the app path the token expires, so [`GitHub::remote_now`] asks
    /// for one right before the push.
    pub fn remote(&self) -> Result<Remote, PrError> {
        match &self.token {
            TokenSource::Static(token) => self.remote_with(token),
            TokenSource::App { .. } | TokenSource::Entra(_) => Err(PrError::App(
                "an app token is asked for at each push: use `remote_now`".into(),
            )),
        }
    }

    /// The same remote with a token asked for now: what a push on either
    /// path uses.
    pub async fn remote_now(&self) -> Result<Remote, PrError> {
        self.remote_with(&self.token.token().await?)
    }

    fn remote_with(&self, token: &str) -> Result<Remote, PrError> {
        let rest = self
            .api
            .split_once("://")
            .map_or(self.api.as_str(), |(_, r)| r);
        let host = rest.split('/').next().unwrap_or(rest);
        let host = host.strip_prefix("api.").unwrap_or(host);
        Remote::https(host, &self.owner, &self.repo, token)
    }

    pub(crate) async fn get(&self, path: &str) -> Result<serde_json::Value, PrError> {
        let url = format!("{}/repos/{}/{}/{path}", self.api, self.owner, self.repo);
        self.get_json(&url).await
    }

    /// The repo itself, `GET /repos/{owner}/{repo}` (#331).
    pub(crate) async fn get_root<T: serde::de::DeserializeOwned>(&self) -> Result<T, PrError> {
        let url = format!("{}/repos/{}/{}", self.api, self.owner, self.repo);
        self.get_json(&url).await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, PrError> {
        let token = self.token.token().await?;
        let response = self
            .client
            .get(url)
            .bearer_auth(&token)
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

    /// `path` as text (a job's log, #250), following GitHub's redirect to
    /// where it is stored: the token is not sent to another host.
    pub(crate) async fn get_text(&self, path: &str) -> Result<String, PrError> {
        let url = format!("{}/repos/{}/{}/{path}", self.api, self.owner, self.repo);
        let token = self.token.token().await?;
        let response = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        if status.is_success() {
            Ok(text)
        } else {
            Err(PrError::GitHub {
                status: status.as_u16(),
                message: text.chars().take(200).collect(),
            })
        }
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
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    /// `method` on `path` of this repo, with `body` as JSON when there is
    /// one; the reply read as `T`.
    pub(crate) async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, PrError> {
        let url = format!("{}/repos/{}/{}/{path}", self.api, self.owner, self.repo);
        let token = self.token.token().await?;
        let mut request = self
            .client
            .request(method, &url)
            .bearer_auth(&token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
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
                "title": crate::masked(title),
                "head": head.name(),
                "base": base,
                "body": crate::masked(body),
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

    /// Opens an issue of this repo: an error spike's report, for the team to
    /// read and answer with `/aegis fix` (#706). The caller makes `title`
    /// and `body` inert.
    pub async fn open_issue(&self, title: &str, body: &str) -> Result<OpenedIssue, PrError> {
        self.post(
            "issues",
            &json!({ "title": crate::masked(title), "body": crate::masked(body) }),
        )
        .await
    }

    /// Comments on issue `number` of this repo: the job's status for whoever
    /// asked for it there (#25). The caller makes `body` inert.
    pub async fn comment_on_issue(&self, number: u64, body: &str) -> Result<IssueComment, PrError> {
        self.post(
            &format!("issues/{number}/comments"),
            &json!({ "body": crate::masked(body) }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_api_root_is_https_or_this_machine_by_its_host() {
        for ok in [
            "https://api.github.com",
            "https://git.acme.example/api/v3/",
            "http://localhost:8080",
            "http://127.0.0.1:9000/api",
            "http://[::1]:1234",
        ] {
            assert!(checked_api(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.github.com",
            "http://localhost.attacker.example",
            "http://127.0.0.1.attacker.example/",
            "http://localhost@attacker.example",
            "https://user:pass@api.github.com",
            "ftp://localhost",
            "localhost",
        ] {
            assert!(checked_api(bad).is_err(), "{bad}");
        }
    }
}
