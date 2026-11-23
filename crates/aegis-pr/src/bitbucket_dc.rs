//! Pull requests on Bitbucket Data Center and Server (REST API 1.0,
//! #635): the agent's PR opened from a branch of the same repository,
//! comments on it, and what a review round reads of it. Reached with an
//! HTTP access token (personal, project or repository), sent as a bearer
//! token, to git too ([`crate::BEARER_USER`]).
//!
//! The API root is `https://<host>[/<context>]/rest/api/1.0`; a fork's
//! `owner` is its project key and `name` its repository slug. Only
//! comments of people who may write to the repository are read (design
//! §7), by their effective permission, groups included.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::github::{checked_api, plain_name};
use crate::review::{MAX_NOTES, NoteAt, PullState, REPLY_MARKER, ReviewNote};
use crate::{AgentBranch, PrError, Remote, RepoAccess};

/// A request to Bitbucket still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Pages of a listing read, at most.
const MAX_PAGES: usize = 5;
/// Items asked for per page.
const PAGE_LIMIT: u32 = 100;

/// The API root's suffixes, after the web root.
const API_SUFFIXES: [&str; 2] = ["/rest/api/1.0", "/rest/api/latest"];

#[derive(Debug, Clone)]
pub struct BitbucketDataCenter {
    client: reqwest::Client,
    api: String,
    /// `https://<host>[/<context>]`.
    root: String,
    project: String,
    repo: String,
    token: String,
}

/// A pull request opened by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataCenterPull {
    pub id: u64,
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct Href {
    href: String,
}

#[derive(Debug, Deserialize)]
struct SelfLinks {
    #[serde(rename = "self", default)]
    own: Vec<Href>,
}

#[derive(Debug, Deserialize)]
struct Project {
    key: String,
}

#[derive(Debug, Deserialize)]
struct RepoRef {
    slug: String,
    project: Project,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ref {
    display_id: String,
    #[serde(default)]
    latest_commit: Option<String>,
    repository: RepoRef,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    id: u64,
    state: String,
    from_ref: Ref,
    to_ref: Ref,
    links: SelfLinks,
}

#[derive(Debug, Deserialize)]
struct Author {
    #[serde(default)]
    slug: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    #[serde(default)]
    text: String,
    author: Author,
    #[serde(default)]
    created_date: i64,
}

#[derive(Debug, Deserialize)]
struct Anchor {
    path: String,
    #[serde(default)]
    line: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Activity {
    action: String,
    #[serde(default)]
    comment: Option<Comment>,
    #[serde(default)]
    comment_anchor: Option<Anchor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    values: Vec<T>,
    #[serde(default = "yes")]
    is_last_page: bool,
    #[serde(default)]
    next_page_start: Option<u64>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct UserRow {
    slug: String,
}

#[derive(Debug, Deserialize)]
struct CloneLink {
    href: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct RepoLinks {
    #[serde(default)]
    clone: Vec<CloneLink>,
}

#[derive(Debug, Deserialize)]
struct Repository {
    slug: String,
    project: Project,
    links: RepoLinks,
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Deserialize)]
struct ApiErrors {
    errors: Vec<ApiMessage>,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    message: String,
}

impl BitbucketDataCenter {
    /// `api` is the instance's `…/rest/api/1.0`; `project/repo` the
    /// repository the agent works on.
    pub fn new(api: &str, project: &str, repo: &str, token: &str) -> Result<Self, PrError> {
        if !plain_name(project) || !plain_name(repo) {
            return Err(PrError::Bitbucket {
                status: 0,
                message: format!("`{project}/{repo}` is not a project key and repository slug"),
            });
        }
        let api = checked_api(api)?;
        let root = API_SUFFIXES
            .iter()
            .find_map(|s| api.strip_suffix(s))
            .ok_or_else(|| {
                PrError::InvalidRemote(format!("API root `{api}` must end in /rest/api/1.0"))
            })?
            .to_owned();
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
            root,
            // Keys are upper case; clone URLs often write them lower.
            project: project.to_ascii_uppercase(),
            repo: repo.to_owned(),
            token: token.to_owned(),
        })
    }

    /// Where the agent's branch is pushed: `<root>/scm/<project>/<repo>.git`,
    /// same token.
    pub fn remote(&self) -> Result<Remote, PrError> {
        Remote::bitbucket_data_center(&self.root, &self.project, &self.repo, &self.token)
    }

    fn repository(&self) -> serde_json::Value {
        json!({ "slug": self.repo, "project": { "key": self.project } })
    }

    /// Opens a PR from `head` into `base`, a draft when `draft` (8.18 and
    /// later; an older instance opens it as a PR).
    pub async fn open_pull_request(
        &self,
        head: &AgentBranch,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<DataCenterPull, PrError> {
        #[derive(Deserialize)]
        struct Opened {
            id: u64,
            links: SelfLinks,
        }
        let request = json!({
            "title": crate::masked(title),
            "description": crate::masked(body),
            "draft": draft,
            "fromRef": { "id": format!("refs/heads/{}", head.name()), "repository": self.repository() },
            "toRef": { "id": format!("refs/heads/{base}"), "repository": self.repository() },
        });
        let opened: Opened = self
            .call(reqwest::Method::POST, "pull-requests", Some(&request))
            .await?;
        Ok(DataCenterPull {
            id: opened.id,
            url: web_link(&opened.links).ok_or_else(|| PrError::Bitbucket {
                status: 201,
                message: "the new pull request has no link".into(),
            })?,
        })
    }

    /// Comments on PR `id`. The caller makes `body` inert.
    pub async fn comment_on_pull_request(&self, id: u64, body: &str) -> Result<(), PrError> {
        let _: serde_json::Value = self
            .call(
                reqwest::Method::POST,
                &format!("pull-requests/{id}/comments"),
                Some(&json!({ "text": crate::masked(body) })),
            )
            .await?;
        Ok(())
    }

    /// PR `id`, refused unless it is open and from an `agent/*` branch of
    /// this same repository.
    pub async fn agent_pull(&self, id: u64) -> Result<PullState, PrError> {
        let pull: Pull = self
            .call(reqwest::Method::GET, &format!("pull-requests/{id}"), None)
            .await?;
        let refuse = |why: &str| PrError::Bitbucket {
            status: 0,
            message: format!("PR #{id} {why}"),
        };
        if pull.state != "OPEN" {
            return Err(refuse("is not open"));
        }
        if !pull.from_ref.display_id.starts_with(crate::branch::PREFIX) {
            return Err(refuse("is not from an agent branch"));
        }
        let from = &pull.from_ref.repository;
        if !(from.slug.eq_ignore_ascii_case(&self.repo)
            && from.project.key.eq_ignore_ascii_case(&self.project))
        {
            return Err(refuse("is from another repository"));
        }
        let head = pull
            .from_ref
            .latest_commit
            .filter(|c| c.len() == 40 && c.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| refuse("has no head commit"))?;
        Ok(PullState {
            number: pull.id,
            url: web_link(&pull.links).ok_or_else(|| refuse("has no link"))?,
            head_ref: pull.from_ref.display_id,
            head_sha: head,
            base_ref: pull.to_ref.display_id,
        })
    }

    /// Whether the person `slug` may write to this repository, by their
    /// effective permission (their own, a group's, the project's).
    pub async fn can_write(&self, slug: &str) -> Result<bool, PrError> {
        if !plain_name(slug) {
            return Ok(false);
        }
        let url = reqwest::Url::parse_with_params(
            &format!("{}/users", self.api),
            &[
                ("filter", slug),
                ("permission.1", "REPO_WRITE"),
                ("permission.1.projectKey", self.project.as_str()),
                ("permission.1.repositorySlug", self.repo.as_str()),
            ],
        )
        .map_err(|e| PrError::InvalidRemote(e.to_string()))?;
        let users: Vec<UserRow> = self.pages(url.as_str()).await?;
        Ok(users.iter().any(|u| u.slug.eq_ignore_ascii_case(slug)))
    }

    /// The comments on PR `id`, oldest first, of people who may write to
    /// the repository (see the module): a comment on a file is a line
    /// note. Replies are not read.
    pub async fn review_notes(&self, id: u64) -> Result<Vec<ReviewNote>, PrError> {
        let url = format!("{}/pull-requests/{id}/activities", self.repo_url());
        let activities: Vec<Activity> = self.pages(&url).await?;
        let mut access = BTreeMap::new();
        let mut kept = Vec::new();
        for activity in activities {
            let (true, Some(comment)) = (activity.action == "COMMENTED", activity.comment) else {
                continue;
            };
            let readable = comment.author.kind == "NORMAL"
                && !comment.text.trim().is_empty()
                && !comment.text.contains(REPLY_MARKER);
            if !readable {
                continue;
            }
            let writes = match access.get(&comment.author.slug) {
                Some(writes) => *writes,
                None => {
                    let writes = self.can_write(&comment.author.slug).await?;
                    access.insert(comment.author.slug.clone(), writes);
                    writes
                }
            };
            if !writes {
                continue;
            }
            let at = match activity.comment_anchor {
                Some(anchor) => NoteAt::Line {
                    path: anchor.path,
                    line: anchor.line,
                },
                None => NoteAt::Conversation,
            };
            kept.push(ReviewNote {
                author: comment.author.slug,
                at,
                body: comment.text,
                created_at: rfc3339(comment.created_date),
            });
        }
        kept.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let drop = kept.len().saturating_sub(MAX_NOTES);
        Ok(kept.split_off(drop))
    }

    /// This repository as the token sees it; its web URL is its HTTP clone
    /// URL, the one a repo is registered with. Bitbucket does not say what
    /// the token may do: pushing and commenting are taken as allowed, and
    /// a token without them fails on its first push.
    pub async fn access(&self) -> Result<RepoAccess, PrError> {
        let repo: Repository = self.call(reqwest::Method::GET, "", None).await?;
        let web_url = repo
            .links
            .clone
            .into_iter()
            .find(|l| l.name == "http")
            .map(|l| l.href)
            .unwrap_or_default();
        Ok(RepoAccess {
            path: format!("{}/{}", repo.project.key, repo.slug),
            web_url,
            push: true,
            comment: true,
            issues_enabled: false,
            archived: repo.archived,
        })
    }

    fn repo_url(&self) -> String {
        format!("{}/projects/{}/repos/{}", self.api, self.project, self.repo)
    }

    /// Every page of the listing at `url`, up to [`MAX_PAGES`].
    async fn pages<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<Vec<T>, PrError> {
        let mut values = Vec::new();
        let mut start = 0;
        let join = if url.contains('?') { '&' } else { '?' };
        for _ in 0..MAX_PAGES {
            let page_url = format!("{url}{join}limit={PAGE_LIMIT}&start={start}");
            let (status, text) = self.send(reqwest::Method::GET, &page_url, None).await?;
            let page: Page<T> = parsed(status, &text)?;
            values.extend(page.values);
            match page.next_page_start {
                Some(next) if !page.is_last_page && next > start => start = next,
                _ => break,
            }
        }
        Ok(values)
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, PrError> {
        let url = if path.is_empty() {
            self.repo_url()
        } else {
            format!("{}/{path}", self.repo_url())
        };
        let (status, text) = self.send(method, &url, body).await?;
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
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(unreached)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(unreached)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<ApiErrors>(&text)
                .ok()
                .and_then(|e| e.errors.into_iter().next())
                .map(|e| e.message)
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(PrError::Bitbucket { status, message });
        }
        Ok((status, text))
    }
}

/// A PR's web page: its first `self` link.
fn web_link(links: &SelfLinks) -> Option<String> {
    links.own.first().map(|l| l.href.clone())
}

/// Unix milliseconds as RFC 3339; the epoch when out of range.
fn rfc3339(ms: i64) -> String {
    let at = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
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
    fn the_agents_branch_goes_under_scm_of_the_web_root() {
        for (api, url) in [
            (
                "https://git.acme.example/rest/api/1.0",
                "https://git.acme.example/scm/PAY/cart.git",
            ),
            (
                "https://acme.example/bitbucket/rest/api/latest/",
                "https://acme.example/bitbucket/scm/PAY/cart.git",
            ),
        ] {
            let dc = BitbucketDataCenter::new(api, "PAY", "cart", "t").unwrap();
            assert_eq!(dc.remote().unwrap().url(), url, "{api}");
        }
        assert!(
            BitbucketDataCenter::new("https://git.acme.example/api/v4", "PAY", "cart", "t")
                .is_err()
        );
        assert!(
            BitbucketDataCenter::new("https://git.acme.example/rest/api/1.0", "~me", "cart", "t")
                .is_err()
        );
    }

    #[test]
    fn comment_times_are_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000_000), "2023-11-14T22:13:20Z");
    }
}
