//! Pull requests on Azure DevOps (Azure Repos, REST API 7.1, #636): the
//! agent's PR opened from a branch of the same repository, comment
//! threads on it, and what a review round reads of it.
//!
//! The API root is the organization or collection URL
//! (`https://dev.azure.com/<org>`, `https://<host>/tfs/<collection>`); a
//! fork's `owner` is its project and `name` its repository. A personal
//! access token is sent as Basic auth, a Microsoft Entra access token
//! (from a service principal, [`entra_token`]) as a bearer token, to git
//! too ([`crate::AZURE_DEVOPS_USER`]).
//!
//! Only comments of people who may write to the repository are read
//! (design §7): members, groups included, of the project's
//! [`WRITER_GROUPS`].

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::github::{checked_api, plain_name};
use crate::review::{MAX_NOTES, NoteAt, PullState, REPLY_MARKER, ReviewNote};
use crate::{AgentBranch, PrError, Remote, RepoAccess, is_entra_token};

/// A request to Azure DevOps still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

const API_VERSION: &str = "api-version=7.1";

/// Azure DevOps takes a PR description up to this many characters.
const MAX_DESCRIPTION: usize = 4000;

/// The project groups whose members may write to its repositories.
pub const WRITER_GROUPS: [&str; 2] = ["Contributors", "Project Administrators"];

/// Where a tenant's tokens are issued.
pub const ENTRA_AUTHORITY: &str = "https://login.microsoftonline.com";

/// Azure DevOps' application ID, the resource its tokens are for.
const AZURE_DEVOPS_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";

#[derive(Debug, Clone)]
pub struct AzureDevOps {
    client: reqwest::Client,
    /// The organization or collection URL.
    collection: String,
    project: String,
    repo: String,
    token: String,
}

/// A pull request opened by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzurePull {
    pub id: u64,
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct Project {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RepoRef {
    name: String,
    project: Project,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    commit_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    pull_request_id: u64,
    status: String,
    source_ref_name: String,
    target_ref_name: String,
    #[serde(default)]
    last_merge_source_commit: Option<Commit>,
    repository: RepoRef,
    /// Set when the PR is from a fork.
    #[serde(default)]
    fork_source: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Author {
    #[serde(default)]
    id: String,
    #[serde(default)]
    unique_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    #[serde(default)]
    content: String,
    author: Author,
    #[serde(default)]
    published_date: String,
    #[serde(default)]
    comment_type: String,
    #[serde(default)]
    is_deleted: bool,
}

#[derive(Debug, Deserialize)]
struct Position {
    line: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadContext {
    file_path: String,
    #[serde(default)]
    right_file_start: Option<Position>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    is_deleted: bool,
    #[serde(default)]
    thread_context: Option<ThreadContext>,
    #[serde(default)]
    comments: Vec<Comment>,
}

#[derive(Debug, Deserialize)]
struct List<T> {
    value: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    #[serde(default)]
    descriptor: serde_json::Value,
    #[serde(default)]
    member_of: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    name: String,
    project: Project,
    #[serde(default)]
    remote_url: String,
    #[serde(default)]
    is_disabled: bool,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    message: String,
}

impl AzureDevOps {
    /// `collection` is the organization or collection URL; `project/repo`
    /// the repository the agent works on.
    pub fn new(collection: &str, project: &str, repo: &str, token: &str) -> Result<Self, PrError> {
        if !plain_name(project) || !plain_name(repo) {
            return Err(PrError::AzureDevOps {
                status: 0,
                message: format!("`{project}/{repo}` is not a project and repository name"),
            });
        }
        let collection = checked_api(collection)?;
        if collection.contains("/_apis") {
            return Err(PrError::InvalidRemote(format!(
                "`{collection}` must be the organization or collection URL"
            )));
        }
        let client = reqwest::Client::builder()
            .user_agent("aegis-code")
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| unreached(&e))?;
        Ok(Self {
            client,
            collection,
            project: project.to_owned(),
            repo: repo.to_owned(),
            token: token.to_owned(),
        })
    }

    /// Where the agent's branch is pushed:
    /// `<collection>/<project>/_git/<repo>`, same token.
    pub fn remote(&self) -> Result<Remote, PrError> {
        Remote::azure_devops(&self.collection, &self.project, &self.repo, &self.token)
    }

    /// The web page of PR `id`.
    pub fn pull_url(&self, id: u64) -> String {
        format!(
            "{}/{}/_git/{}/pullrequest/{id}",
            self.collection, self.project, self.repo
        )
    }

    /// Opens a PR from `head` into `base`, a draft when `draft`. A body
    /// over Azure's 4,000 characters is cut, saying so.
    pub async fn open_pull_request(
        &self,
        head: &AgentBranch,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<AzurePull, PrError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Opened {
            pull_request_id: u64,
        }
        let request = json!({
            "sourceRefName": format!("refs/heads/{}", head.name()),
            "targetRefName": format!("refs/heads/{base}"),
            "title": crate::masked(title),
            "description": cut(&crate::masked(body), MAX_DESCRIPTION),
            "isDraft": draft,
        });
        let opened: Opened = self
            .call(reqwest::Method::POST, "pullrequests", Some(&request))
            .await?;
        Ok(AzurePull {
            id: opened.pull_request_id,
            url: self.pull_url(opened.pull_request_id),
        })
    }

    /// Comments on PR `id` in a thread of its own, closed: it asks nothing
    /// of the reviewers. The caller makes `body` inert.
    pub async fn comment_on_pull_request(&self, id: u64, body: &str) -> Result<(), PrError> {
        let thread = json!({
            "comments": [{ "parentCommentId": 0, "content": crate::masked(body), "commentType": "text" }],
            "status": "closed",
        });
        let _: serde_json::Value = self
            .call(
                reqwest::Method::POST,
                &format!("pullRequests/{id}/threads"),
                Some(&thread),
            )
            .await?;
        Ok(())
    }

    /// PR `id`, refused unless it is active and from an `agent/*` branch of
    /// this same repository.
    pub async fn agent_pull(&self, id: u64) -> Result<PullState, PrError> {
        let pull: Pull = self
            .call(reqwest::Method::GET, &format!("pullrequests/{id}"), None)
            .await?;
        let refuse = |why: &str| PrError::AzureDevOps {
            status: 0,
            message: format!("PR #{id} {why}"),
        };
        if pull.status != "active" {
            return Err(refuse("is not active"));
        }
        let head_ref = branch(&pull.source_ref_name).ok_or_else(|| refuse("has no branch"))?;
        if !head_ref.starts_with(crate::branch::PREFIX) {
            return Err(refuse("is not from an agent branch"));
        }
        let repo = &pull.repository;
        let same = pull.fork_source.is_none()
            && repo.name.eq_ignore_ascii_case(&self.repo)
            && repo.project.name.eq_ignore_ascii_case(&self.project);
        if !same {
            return Err(refuse("is from another repository"));
        }
        let head = pull
            .last_merge_source_commit
            .map(|c| c.commit_id)
            .filter(|c| c.len() == 40 && c.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| refuse("has no head commit"))?;
        let base_ref = branch(&pull.target_ref_name).ok_or_else(|| refuse("has no base"))?;
        Ok(PullState {
            number: pull.pull_request_id,
            url: self.pull_url(pull.pull_request_id),
            head_ref: head_ref.to_owned(),
            head_sha: head,
            base_ref: base_ref.to_owned(),
        })
    }

    /// Whether the identity `id` (a GUID, as comments and service hooks
    /// name their authors) may write to this repository: a member, groups
    /// included, of one of the project's [`WRITER_GROUPS`].
    pub async fn can_write(&self, id: &str) -> Result<bool, PrError> {
        if !is_guid(id) {
            return Ok(false);
        }
        let ims = identities_root(&self.collection);
        let url = format!(
            "{ims}/_apis/identities?identityIds={id}&queryMembership=Expanded&{API_VERSION}"
        );
        let (status, text) = self.send(reqwest::Method::GET, &url, None).await?;
        let found: List<Identity> = parsed(status, &text)?;
        let Some(person) = found.value.into_iter().next() else {
            return Ok(false);
        };
        let member_of: Vec<String> = person.member_of.iter().filter_map(descriptor).collect();
        for group in WRITER_GROUPS {
            let url = reqwest::Url::parse_with_params(
                &format!("{ims}/_apis/identities"),
                &[
                    ("searchFilter", "General"),
                    ("filterValue", &format!("[{}]\\{group}", self.project)),
                    ("queryMembership", "None"),
                    ("api-version", "7.1"),
                ],
            )
            .map_err(|e| PrError::InvalidRemote(e.to_string()))?;
            let (status, text) = self.send(reqwest::Method::GET, url.as_str(), None).await?;
            let groups: List<Identity> = parsed(status, &text)?;
            let named = groups
                .value
                .iter()
                .filter_map(|g| descriptor(&g.descriptor));
            if named
                .into_iter()
                .any(|d| member_of.iter().any(|m| m.eq_ignore_ascii_case(&d)))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The comments in PR `id`'s active threads, oldest first, of people
    /// who may write to the repository (see the module): a thread on a
    /// file is a line note. Resolved threads (`fixed`, `closed`, …) and
    /// system comments are not read.
    pub async fn review_notes(&self, id: u64) -> Result<Vec<ReviewNote>, PrError> {
        let threads: List<Thread> = self
            .call(
                reqwest::Method::GET,
                &format!("pullRequests/{id}/threads"),
                None,
            )
            .await?;
        let mut access = BTreeMap::new();
        let mut kept = Vec::new();
        for thread in threads.value {
            if thread.is_deleted || thread.status.as_deref() != Some("active") {
                continue;
            }
            let at = match &thread.thread_context {
                Some(context) => NoteAt::Line {
                    path: context.file_path.trim_start_matches('/').to_owned(),
                    line: context.right_file_start.as_ref().map(|p| p.line),
                },
                None => NoteAt::Conversation,
            };
            for comment in thread.comments {
                let readable = comment.comment_type == "text"
                    && !comment.is_deleted
                    && !comment.content.trim().is_empty()
                    && !comment.content.contains(REPLY_MARKER);
                if !readable {
                    continue;
                }
                let writes = match access.get(&comment.author.id) {
                    Some(writes) => *writes,
                    None => {
                        let writes = self.can_write(&comment.author.id).await?;
                        access.insert(comment.author.id.clone(), writes);
                        writes
                    }
                };
                if writes {
                    kept.push(ReviewNote {
                        author: comment.author.unique_name,
                        at: at.clone(),
                        body: comment.content,
                        created_at: comment.published_date,
                    });
                }
            }
        }
        kept.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let drop = kept.len().saturating_sub(MAX_NOTES);
        Ok(kept.split_off(drop))
    }

    /// This repository as the token sees it; its web URL is its clone URL,
    /// the one a repo is registered with. Azure DevOps does not say what
    /// the token may do: pushing and commenting are taken as allowed, and
    /// a token without them fails on its first push.
    pub async fn access(&self) -> Result<RepoAccess, PrError> {
        let repo: Repository = self.call(reqwest::Method::GET, "", None).await?;
        Ok(RepoAccess {
            path: format!("{}/{}", repo.project.name, repo.name),
            web_url: without_user(&repo.remote_url),
            push: true,
            comment: true,
            issues_enabled: false,
            archived: repo.is_disabled,
        })
    }

    fn repo_url(&self) -> String {
        format!(
            "{}/{}/_apis/git/repositories/{}",
            self.collection, self.project, self.repo
        )
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, PrError> {
        let url = if path.is_empty() {
            format!("{}?{API_VERSION}", self.repo_url())
        } else {
            format!("{}/{path}?{API_VERSION}", self.repo_url())
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
        let request = self
            .client
            .request(method, url)
            .header("Accept", "application/json");
        let mut request = if is_entra_token(&self.token) {
            request.bearer_auth(&self.token)
        } else {
            request.basic_auth("aegis", Some(&self.token))
        };
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| unreached(&e))?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| unreached(&e))?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<ApiMessage>(&text)
                .map(|e| e.message)
                .unwrap_or_else(|_| text.chars().take(200).collect());
            return Err(PrError::AzureDevOps { status, message });
        }
        Ok((status, text))
    }
}

/// An access token for Azure DevOps of the service principal `client_id`
/// of `tenant`, from its client secret (client credentials, #636).
/// `authority` is [`ENTRA_AUTHORITY`] but in tests.
pub async fn entra_token(
    authority: &str,
    tenant: &str,
    client_id: &str,
    secret: &str,
) -> Result<String, PrError> {
    #[derive(Deserialize)]
    struct Issued {
        access_token: String,
    }
    #[derive(Deserialize)]
    struct Refused {
        #[serde(default)]
        error_description: String,
    }
    if !plain_name(tenant) || !plain_name(client_id) {
        return Err(PrError::AzureDevOps {
            status: 0,
            message: "the tenant and client ID must be plain names".into(),
        });
    }
    let authority = checked_api(authority)?;
    let mut form = reqwest::Url::parse("https://form.invalid/")
        .map_err(|e| PrError::InvalidRemote(e.to_string()))?;
    form.query_pairs_mut()
        .append_pair("grant_type", "client_credentials")
        .append_pair("client_id", client_id)
        .append_pair("client_secret", secret)
        .append_pair("scope", &format!("{AZURE_DEVOPS_RESOURCE}/.default"));
    let client = reqwest::Client::builder()
        .user_agent("aegis-code")
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| unreached(&e))?;
    let response = client
        .post(format!("{authority}/{tenant}/oauth2/v2.0/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.query().unwrap_or_default().to_owned())
        .send()
        .await
        .map_err(|e| unreached(&e))?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|e| unreached(&e))?;
    if !(200..300).contains(&status) {
        let message = serde_json::from_str::<Refused>(&text)
            .map(|r| r.error_description)
            .unwrap_or_default();
        // The first line only: the rest is trace and correlation IDs.
        let message = message.lines().next().unwrap_or("no token").to_owned();
        return Err(PrError::AzureDevOps { status, message });
    }
    let issued: Issued = parsed(status, &text)?;
    Ok(issued.access_token)
}

fn unreached(e: &reqwest::Error) -> PrError {
    PrError::AzureDevOps {
        status: 0,
        message: e.to_string(),
    }
}

/// Where identities are asked: the organization's identity service on
/// Azure DevOps Services, the collection itself on a server.
fn identities_root(collection: &str) -> String {
    if let Some(org) = collection.strip_prefix("https://dev.azure.com/") {
        return format!("https://vssps.dev.azure.com/{org}");
    }
    if let Some(rest) = collection.strip_prefix("https://")
        && let Some(org) = rest.strip_suffix(".visualstudio.com")
        && !org.contains('/')
    {
        return format!("https://{org}.vssps.visualstudio.com");
    }
    collection.to_owned()
}

/// An identity descriptor as a string: `type;identifier`, written either
/// way.
fn descriptor(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(o) => {
            let kind = o.get("identityType")?.as_str()?;
            let id = o.get("identifier")?.as_str()?;
            Some(format!("{kind};{id}"))
        }
        _ => None,
    }
}

fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `refs/heads/<branch>` → `<branch>`.
fn branch(name: &str) -> Option<&str> {
    name.strip_prefix("refs/heads/").filter(|b| !b.is_empty())
}

/// A clone URL without the user name Azure puts in it
/// (`https://acme@dev.azure.com/…`).
fn without_user(url: &str) -> String {
    match url.strip_prefix("https://").and_then(|r| r.split_once('@')) {
        Some((user, rest)) if !user.contains('/') => format!("https://{rest}"),
        _ => url.to_owned(),
    }
}

/// `text` cut to `max` characters, saying so.
fn cut(text: &str, max: usize) -> String {
    const CUT: &str = "\n\n…(cut: Azure DevOps keeps 4,000 characters)";
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let keep = max - CUT.chars().count();
    text.chars().take(keep).chain(CUT.chars()).collect()
}

fn parsed<T: serde::de::DeserializeOwned>(status: u16, text: &str) -> Result<T, PrError> {
    serde_json::from_str(text).map_err(|e| PrError::AzureDevOps {
        status,
        message: format!("unexpected reply: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agents_branch_goes_under_git_of_the_project() {
        for (collection, url) in [
            (
                "https://dev.azure.com/acme",
                "https://dev.azure.com/acme/Shop/_git/cart",
            ),
            (
                "https://tfs.acme.example/tfs/Main/",
                "https://tfs.acme.example/tfs/Main/Shop/_git/cart",
            ),
        ] {
            let az = AzureDevOps::new(collection, "Shop", "cart", "t").unwrap();
            assert_eq!(az.remote().unwrap().url(), url, "{collection}");
        }
        assert!(AzureDevOps::new("https://dev.azure.com/acme/_apis", "Shop", "cart", "t").is_err());
        assert!(AzureDevOps::new("https://dev.azure.com/acme", "My Shop", "cart", "t").is_err());
    }

    #[test]
    fn identities_are_asked_of_the_organizations_identity_service() {
        assert_eq!(
            identities_root("https://dev.azure.com/acme"),
            "https://vssps.dev.azure.com/acme"
        );
        assert_eq!(
            identities_root("https://acme.visualstudio.com"),
            "https://acme.vssps.visualstudio.com"
        );
        assert_eq!(
            identities_root("https://tfs.acme.example/tfs/Main"),
            "https://tfs.acme.example/tfs/Main"
        );
    }

    #[test]
    fn helpers_read_azures_names() {
        assert!(is_guid("6f5e1b2a-0c3d-4e5f-8a9b-0c1d2e3f4a5b"));
        assert!(!is_guid("../x"));
        assert_eq!(branch("refs/heads/agent/7"), Some("agent/7"));
        assert_eq!(branch("refs/tags/v1"), None);
        assert_eq!(
            without_user("https://acme@dev.azure.com/acme/Shop/_git/cart"),
            "https://dev.azure.com/acme/Shop/_git/cart"
        );
        assert_eq!(
            descriptor(&json!({ "identityType": "T", "identifier": "S-1" })).as_deref(),
            Some("T;S-1")
        );
        let long = "x".repeat(5000);
        let cut = cut(&long, MAX_DESCRIPTION);
        assert_eq!(cut.chars().count(), MAX_DESCRIPTION);
        assert!(cut.ends_with("characters)"));
        assert_eq!(super::cut("short", MAX_DESCRIPTION), "short");
    }
}
