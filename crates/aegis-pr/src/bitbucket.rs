//! Pull requests on Bitbucket Cloud (REST API 2.0, #634): the agent's PR
//! opened from a branch of the same repository, comments on it, and what
//! a review round reads of it. Reached with a repository or workspace
//! access token, sent as a bearer token; git pushes with it as
//! [`crate::BITBUCKET_USER`].
//!
//! Only comments of people who may write to the repository are read
//! (design §7); the token needs the repository admin scope to tell.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::github::{checked_api, plain_name};
use crate::review::{MAX_NOTES, NoteAt, PullState, REPLY_MARKER, ReviewNote};
use crate::{AgentBranch, PrError, Remote, RepoAccess};

/// A request to Bitbucket still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Pages of a listing read, at most (100 items each).
const MAX_PAGES: usize = 5;

/// The API root of Bitbucket Cloud.
pub const BITBUCKET_API: &str = "https://api.bitbucket.org/2.0";

#[derive(Debug, Clone)]
pub struct Bitbucket {
    client: reqwest::Client,
    api: String,
    workspace: String,
    repo: String,
    token: String,
}

/// A pull request opened by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitbucketPull {
    pub id: u64,
    pub url: String,
}

/// What a person may do in a repository, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BitbucketPermission {
    None,
    Read,
    Write,
    Admin,
}

impl BitbucketPermission {
    fn parse(text: &str) -> Self {
        match text {
            "admin" => Self::Admin,
            "write" => Self::Write,
            "read" => Self::Read,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Link {
    href: String,
}

#[derive(Debug, Deserialize)]
struct Links {
    html: Link,
}

#[derive(Debug, Deserialize)]
struct Branch {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Commit {
    hash: String,
}

#[derive(Debug, Deserialize)]
struct FullName {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct End {
    branch: Branch,
    #[serde(default)]
    commit: Option<Commit>,
    #[serde(default)]
    repository: Option<FullName>,
}

#[derive(Debug, Deserialize)]
struct Pull {
    id: u64,
    state: String,
    source: End,
    destination: End,
    links: Links,
}

#[derive(Debug, Deserialize)]
struct User {
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    nickname: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct Content {
    #[serde(default)]
    raw: String,
}

#[derive(Debug, Deserialize)]
struct Inline {
    path: String,
    #[serde(default)]
    to: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Comment {
    #[serde(default)]
    user: Option<User>,
    content: Content,
    #[serde(default)]
    inline: Option<Inline>,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    created_on: String,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    values: Vec<T>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Permission {
    permission: String,
}

#[derive(Debug, Deserialize)]
struct Repository {
    full_name: String,
    links: Links,
    #[serde(default)]
    has_issues: bool,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    error: ApiMessage,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    message: String,
}

impl Bitbucket {
    /// `api` is [`BITBUCKET_API`]; `workspace/repo` the repository the
    /// agent works on.
    pub fn new(api: &str, workspace: &str, repo: &str, token: &str) -> Result<Self, PrError> {
        if !plain_name(workspace) || !plain_name(repo) {
            return Err(PrError::Bitbucket {
                status: 0,
                message: format!("`{workspace}/{repo}` is not a repository path"),
            });
        }
        let api = checked_api(api)?;
        let client = reqwest::Client::builder()
            .user_agent("aegis-code")
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| PrError::Bitbucket {
                status: 0,
                message: e.to_string(),
            })?;
        Ok(Self {
            client,
            api,
            workspace: workspace.to_owned(),
            repo: repo.to_owned(),
            token: token.to_owned(),
        })
    }

    /// Where the agent's branch is pushed: the web host of the API root
    /// (`api.bitbucket.org` → `bitbucket.org`), same repository, same
    /// token.
    pub fn remote(&self) -> Result<Remote, PrError> {
        let rest = self
            .api
            .split_once("://")
            .map_or(self.api.as_str(), |(_, r)| r);
        let host = rest.split('/').next().unwrap_or(rest);
        let host = host.strip_prefix("api.").unwrap_or(host);
        Remote::bitbucket(host, &self.workspace, &self.repo, &self.token)
    }

    /// Opens a PR from `head` into `base`, a draft when `draft`.
    pub async fn open_pull_request(
        &self,
        head: &AgentBranch,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<BitbucketPull, PrError> {
        #[derive(Deserialize)]
        struct Opened {
            id: u64,
            links: Links,
        }
        let request = json!({
            "title": crate::masked(title),
            "description": crate::masked(body),
            "source": { "branch": { "name": head.name() } },
            "destination": { "branch": { "name": base } },
            "close_source_branch": false,
            "draft": draft,
        });
        let opened: Opened = self
            .call(reqwest::Method::POST, "pullrequests", Some(&request))
            .await?;
        Ok(BitbucketPull {
            id: opened.id,
            url: opened.links.html.href,
        })
    }

    /// Comments on PR `id`. The caller makes `body` inert.
    pub async fn comment_on_pull_request(&self, id: u64, body: &str) -> Result<(), PrError> {
        let _: serde_json::Value = self
            .call(
                reqwest::Method::POST,
                &format!("pullrequests/{id}/comments"),
                Some(&json!({ "content": { "raw": crate::masked(body) } })),
            )
            .await?;
        Ok(())
    }

    /// PR `id`, refused unless it is open and from an `agent/*` branch of
    /// this same repository; its head as a full commit hash.
    pub async fn agent_pull(&self, id: u64) -> Result<PullState, PrError> {
        let pull: Pull = self
            .call(reqwest::Method::GET, &format!("pullrequests/{id}"), None)
            .await?;
        let refuse = |why: &str| PrError::Bitbucket {
            status: 0,
            message: format!("PR #{id} {why}"),
        };
        if pull.state != "OPEN" {
            return Err(refuse("is not open"));
        }
        if !pull.source.branch.name.starts_with(crate::branch::PREFIX) {
            return Err(refuse("is not from an agent branch"));
        }
        let own = format!("{}/{}", self.workspace, self.repo);
        if pull
            .source
            .repository
            .as_ref()
            .is_none_or(|r| !r.full_name.eq_ignore_ascii_case(&own))
        {
            return Err(refuse("is from another repository"));
        }
        let short = pull
            .source
            .commit
            .map(|c| c.hash)
            .ok_or_else(|| refuse("has no head commit"))?;
        Ok(PullState {
            number: pull.id,
            url: pull.links.html.href,
            head_ref: pull.source.branch.name,
            head_sha: self.full_hash(&short).await?,
            base_ref: pull.destination.branch.name,
        })
    }

    /// Bitbucket names a PR's commits by a short hash; a push on top of
    /// one needs all of it.
    async fn full_hash(&self, hash: &str) -> Result<String, PrError> {
        if hash.len() == 40 {
            return Ok(hash.to_owned());
        }
        if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(PrError::Bitbucket {
                status: 0,
                message: format!("`{hash}` is not a commit"),
            });
        }
        let commit: Commit = self
            .call(reqwest::Method::GET, &format!("commit/{hash}"), None)
            .await?;
        Ok(commit.hash)
    }

    /// What the person `user` (their `{uuid}` or account id) may do in
    /// this repository.
    pub async fn permission(&self, user: &str) -> Result<BitbucketPermission, PrError> {
        if user.is_empty()
            || !user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "{}-:".contains(c))
        {
            return Ok(BitbucketPermission::None);
        }
        let path = format!(
            "permissions-config/users/{}",
            user.replace('{', "%7B").replace('}', "%7D")
        );
        match self
            .call::<Permission>(reqwest::Method::GET, &path, None)
            .await
        {
            Ok(p) => Ok(BitbucketPermission::parse(&p.permission)),
            Err(PrError::Bitbucket { status: 404, .. }) => Ok(BitbucketPermission::None),
            Err(e) => Err(e),
        }
    }

    /// The comments on PR `id`, oldest first, of people who may write to
    /// the repository (see the module): an inline comment is a line note.
    pub async fn review_notes(&self, id: u64) -> Result<Vec<ReviewNote>, PrError> {
        let comments: Vec<Comment> = self
            .pages(&format!("pullrequests/{id}/comments?pagelen=100"))
            .await?;
        let mut access = BTreeMap::new();
        let mut kept = Vec::new();
        for comment in comments {
            let Some(user) = comment.user else { continue };
            let readable = !comment.deleted
                && user.kind == "user"
                && !comment.content.raw.trim().is_empty()
                && !comment.content.raw.contains(REPLY_MARKER);
            if !readable {
                continue;
            }
            let level = match access.get(&user.uuid) {
                Some(level) => *level,
                None => {
                    let level = self.permission(&user.uuid).await?;
                    access.insert(user.uuid.clone(), level);
                    level
                }
            };
            if level < BitbucketPermission::Write {
                continue;
            }
            let at = match comment.inline {
                Some(inline) => NoteAt::Line {
                    path: inline.path,
                    line: inline.to,
                },
                None => NoteAt::Conversation,
            };
            kept.push(ReviewNote {
                author: user.nickname.or(user.display_name).unwrap_or(user.uuid),
                at,
                body: comment.content.raw,
                created_at: comment.created_on,
            });
        }
        kept.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let drop = kept.len().saturating_sub(MAX_NOTES);
        Ok(kept.split_off(drop))
    }

    /// This repository as the token sees it. Bitbucket does not say what
    /// the token may do: pushing and commenting are taken as allowed, and
    /// a token without them fails on its first push.
    pub async fn access(&self) -> Result<RepoAccess, PrError> {
        let repo: Repository = self.call(reqwest::Method::GET, "", None).await?;
        Ok(RepoAccess {
            path: repo.full_name,
            web_url: repo.links.html.href,
            push: true,
            comment: true,
            issues_enabled: repo.has_issues,
            archived: false,
        })
    }

    /// Every page of the listing at `path`, up to [`MAX_PAGES`]; a `next`
    /// link off this API is not followed.
    async fn pages<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, PrError> {
        let mut url = self.url(path);
        let mut values = Vec::new();
        for _ in 0..MAX_PAGES {
            let (status, text) = self.send(reqwest::Method::GET, &url, None).await?;
            let page: Page<T> = parsed(status, &text)?;
            values.extend(page.values);
            match page.next {
                Some(next) if next.starts_with(&format!("{}/", self.api)) => url = next,
                _ => break,
            }
        }
        Ok(values)
    }

    fn url(&self, path: &str) -> String {
        let root = format!("{}/repositories/{}/{}", self.api, self.workspace, self.repo);
        if path.is_empty() {
            root
        } else {
            format!("{root}/{path}")
        }
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, PrError> {
        let (status, text) = self.send(method, &self.url(path), body).await?;
        parsed(status, &text)
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, String), PrError> {
        let unreached = |e: reqwest::Error| PrError::Bitbucket {
            status: 0,
            message: e.without_url().to_string(),
        };
        let mut request = self.client.request(method, url).bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(unreached)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(unreached)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<ApiError>(&text)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| text.chars().take(200).collect());
            return Err(PrError::Bitbucket { status, message });
        }
        Ok((status, text))
    }
}

fn parsed<T: serde::de::DeserializeOwned>(status: u16, text: &str) -> Result<T, PrError> {
    serde_json::from_str(text).map_err(|e| PrError::Bitbucket {
        status,
        message: format!("unexpected reply: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agents_branch_goes_to_the_web_host_of_the_api_root() {
        let bitbucket = Bitbucket::new(BITBUCKET_API, "acme", "app", "t").unwrap();
        assert_eq!(
            bitbucket.remote().unwrap().url(),
            "https://bitbucket.org/acme/app.git"
        );
        for (workspace, repo) in [("acme", ".."), ("a/b", "app"), ("", "app"), ("acme", "a b")] {
            assert!(
                Bitbucket::new(BITBUCKET_API, workspace, repo, "t").is_err(),
                "{workspace}/{repo}"
            );
        }
        assert!(Bitbucket::new("http://bitbucket.example/2.0", "acme", "app", "t").is_err());
    }

    #[test]
    fn permissions_are_ordered_least_first() {
        assert!(BitbucketPermission::parse("admin") > BitbucketPermission::Write);
        assert!(BitbucketPermission::parse("read") < BitbucketPermission::Write);
        assert_eq!(
            BitbucketPermission::parse("none"),
            BitbucketPermission::None
        );
    }
}
