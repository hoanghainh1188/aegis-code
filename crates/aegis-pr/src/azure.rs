//! Pull requests on Azure DevOps (Azure Repos, REST API 7.1, #636): the
//! agent's PR opened from a branch of the same repository, comment
//! threads on it, and what a review round reads of it.
//!
//! The API root is the organization or collection URL
//! (`https://dev.azure.com/<org>`, `https://<host>/tfs/<collection>`); a
//! fork's `owner` is its project and `name` its repository. A personal
//! access token is sent as Basic auth, a Microsoft Entra access token
//! (from a service principal, [`entra_token`]) as a bearer token, to git
//! too ([`crate::AZURE_DEVOPS_USER`]). A service principal's token lives
//! about an hour: [`EntraToken`] keeps it and issues a new one before it
//! expires, asked at each call and each push (#643).
//!
//! Only comments of people who may write to the repository are read
//! (design §7): those whose effective permissions on it allow
//! `GenericContribute` (#642), read from its access control list; when
//! Azure does not answer that, members, groups included, of the project's
//! [`WRITER_GROUPS`].
//!
//! Project and repository names may hold spaces (`My Project`, #641):
//! they are kept as Azure shows them and written `%20` in URLs only
//! ([`azure_name`], [`azure_segment`], [`azure_unescaped`]).

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::github::{checked_api, plain_name};
use crate::review::{MAX_NOTES, NoteAt, PullState, REPLY_MARKER, ReviewNote};
use crate::{AgentBranch, PrError, Remote, RepoAccess, TokenSource, is_entra_token};

/// A request to Azure DevOps still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

const API_VERSION: &str = "api-version=7.1";

/// Azure DevOps takes a PR description up to this many characters.
const MAX_DESCRIPTION: usize = 4000;

/// The project groups whose members may write to its repositories, asked
/// when a repository's access control list cannot be read.
pub const WRITER_GROUPS: [&str; 2] = ["Contributors", "Project Administrators"];

/// How many groups up from a person's own the writer groups are looked
/// for: a team in a team in Contributors is two (#703).
const GROUP_DEPTH: usize = 4;

/// At most this many groups are read in one check, however wide the
/// groups above a person are.
const GROUP_LIMIT: usize = 100;

/// Groups read in one request to the identity service.
const GROUP_BATCH: usize = 20;

/// The Git Repositories security namespace.
const GIT_NAMESPACE: &str = "2e9eb7ed-3c0a-47d4-87c1-0ffdd275fd87";

/// Its `GenericContribute` bit: pushing to a repository.
const GENERIC_CONTRIBUTE: u32 = 4;

/// Where a tenant's tokens are issued.
pub const ENTRA_AUTHORITY: &str = "https://login.microsoftonline.com";

/// A service principal's token is issued again when it has less than this
/// left: a push or a call never starts with a token about to expire.
pub const ENTRA_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

/// Azure DevOps' application ID, the resource its tokens are for.
const AZURE_DEVOPS_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";

#[derive(Debug, Clone)]
pub struct AzureDevOps {
    client: reqwest::Client,
    /// The organization or collection URL.
    collection: String,
    project: String,
    repo: String,
    token: TokenSource,
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
    #[serde(default)]
    id: String,
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

/// An access control list, its entries by identity descriptor.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Acl {
    #[serde(default)]
    token: String,
    #[serde(default)]
    aces_dictionary: BTreeMap<String, Ace>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ace {
    #[serde(default)]
    allow: u32,
    #[serde(default)]
    deny: u32,
    #[serde(default)]
    extended_info: Option<AceExtended>,
}

/// What Azure DevOps says of one identity's right to write to a
/// repository (#642), for measuring the writer check on a real
/// organization: [`AzureDevOps::write_evidence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEvidence {
    /// The identity's descriptor; `None` when Azure knows no such identity.
    pub descriptor: Option<String>,
    /// Its entry in the repository's access control list, or why there is
    /// none: the ACL could not be read, or holds no entry for it.
    pub entry: Result<AclEntry, String>,
    /// The project's [`WRITER_GROUPS`] it is a member of, and through
    /// which groups; `None` when it is in none.
    pub writer_group: Option<WriterGroup>,
    /// What [`AzureDevOps::can_write`] decides.
    pub writes: bool,
}

/// How a person is in one of the project's [`WRITER_GROUPS`] (#703).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterGroup {
    /// The writer group, as [`WRITER_GROUPS`] names it.
    pub name: &'static str,
    /// The descriptors of the groups from the person's own to the writer
    /// group, both included: one when the person is in it directly.
    pub via: Vec<String>,
}

/// An identity's entry in an access control list, its bits as Azure gives
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclEntry {
    /// Set on this token for this identity itself.
    pub allow: u32,
    pub deny: u32,
    /// Everything that applies, groups and inheritance included, as
    /// `(allow, deny)`; `None` when Azure gave no extended information.
    pub effective: Option<(u32, u32)>,
}

impl AclEntry {
    /// Whether the entry decides: its effective bits allow
    /// `GenericContribute` and do not deny it.
    fn writes(&self) -> Option<bool> {
        let (allow, deny) = self.effective?;
        Some(allow & GENERIC_CONTRIBUTE != 0 && deny & GENERIC_CONTRIBUTE == 0)
    }
}

/// An identity's permissions on a token, its groups' and the inherited
/// ones included.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AceExtended {
    // Left out when 0.
    #[serde(default)]
    effective_allow: u32,
    #[serde(default)]
    effective_deny: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    #[serde(default)]
    id: String,
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
        Self::with_source(
            collection,
            project,
            repo,
            TokenSource::Static(token.to_owned()),
        )
    }

    /// The same, with its token from `source`: a fixed token, or a
    /// service principal's ([`TokenSource::Entra`]); never a GitHub App's.
    pub fn with_source(
        collection: &str,
        project: &str,
        repo: &str,
        source: TokenSource,
    ) -> Result<Self, PrError> {
        if matches!(source, TokenSource::App { .. }) {
            return Err(PrError::App(
                "the GitHub App reaches GitHub repos only".into(),
            ));
        }
        if !azure_name(project) || !azure_name(repo) {
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
            token: source,
        })
    }

    /// Where the agent's branch is pushed:
    /// `<collection>/<project>/_git/<repo>`, same token. Only for a fixed
    /// token: a service principal's expires, so [`AzureDevOps::remote_now`]
    /// asks for one right before the push.
    pub fn remote(&self) -> Result<Remote, PrError> {
        match &self.token {
            TokenSource::Static(token) => {
                Remote::azure_devops(&self.collection, &self.project, &self.repo, token)
            }
            TokenSource::App { .. } | TokenSource::Entra(_) => Err(PrError::App(
                "a service principal's token is asked for at each push: use `remote_now`".into(),
            )),
        }
    }

    /// The same remote with a token asked for now: what a push uses.
    pub async fn remote_now(&self) -> Result<Remote, PrError> {
        let token = self.token.token().await?;
        Remote::azure_devops(&self.collection, &self.project, &self.repo, &token)
    }

    /// The web page of PR `id`.
    pub fn pull_url(&self, id: u64) -> String {
        format!(
            "{}/{}/_git/{}/pullrequest/{id}",
            self.collection,
            azure_segment(&self.project),
            azure_segment(&self.repo)
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
    /// name their authors) may write to this repository: its effective
    /// permissions there allow `GenericContribute` and do not deny it,
    /// however they come (a group, the repository, the project, #642).
    /// When Azure does not give them (the token may not read the
    /// repository's security, an older server), a member, groups included,
    /// of one of the project's [`WRITER_GROUPS`].
    pub async fn can_write(&self, id: &str) -> Result<bool, PrError> {
        let Some(person) = self.identity(id).await? else {
            return Ok(false);
        };
        if let Ok(Some(writes)) = self.acl_entry(&person).await.map(|e| e.writes()) {
            return Ok(writes);
        }
        Ok(self.writer_group(&person).await?.is_some())
    }

    /// What Azure says of `id`'s right to write to this repository (#642):
    /// its entry in the repository's ACL, its groups, and what
    /// [`Self::can_write`] makes of them. For measuring the check on a real
    /// organization; the job's own check is [`Self::can_write`].
    pub async fn write_evidence(&self, id: &str) -> Result<WriteEvidence, PrError> {
        let Some(person) = self.identity(id).await? else {
            return Ok(WriteEvidence {
                descriptor: None,
                entry: Err("no such identity".into()),
                writer_group: None,
                writes: false,
            });
        };
        let entry = self.acl_entry(&person).await;
        let writer_group = self.writer_group(&person).await?;
        let writes = match entry.as_ref().ok().and_then(AclEntry::writes) {
            Some(writes) => writes,
            None => writer_group.is_some(),
        };
        Ok(WriteEvidence {
            descriptor: descriptor(&person.descriptor),
            entry,
            writer_group,
            writes,
        })
    }

    /// The identity `id` (a GUID), its groups expanded; `None` when Azure
    /// knows none.
    async fn identity(&self, id: &str) -> Result<Option<Identity>, PrError> {
        if !is_guid(id) {
            return Ok(None);
        }
        let ims = identities_root(&self.collection);
        let url = format!(
            "{ims}/_apis/identities?identityIds={id}&queryMembership=Expanded&{API_VERSION}"
        );
        let (status, text) = self.send(reqwest::Method::GET, &url, None).await?;
        let found: List<Identity> = parsed(status, &text)?;
        Ok(found.value.into_iter().next())
    }

    /// The project's [`WRITER_GROUPS`] `person` is a member of, and through
    /// which groups. Its own groups are compared first, a writer group at a
    /// time; then the groups above them, up to [`GROUP_DEPTH`] (#703): the
    /// identity service may give only a person's direct groups, and a team
    /// in Contributors is not one of them.
    async fn writer_group(&self, person: &Identity) -> Result<Option<WriterGroup>, PrError> {
        let own: Vec<Vec<String>> = person
            .member_of
            .iter()
            .filter_map(descriptor)
            .map(|d| vec![d])
            .collect();
        let mut writers = Vec::new();
        for name in WRITER_GROUPS {
            let named = self.group_descriptors(name).await?;
            if let Some(found) = reaching(&own, name, &named) {
                return Ok(Some(found));
            }
            writers.push((name, named));
        }
        let mut seen: Vec<String> = own.iter().map(|p| p[0].to_ascii_lowercase()).collect();
        let mut level = own;
        for _ in 0..GROUP_DEPTH {
            level = self.groups_above(&level, &mut seen).await?;
            if level.is_empty() {
                break;
            }
            if let Some(found) = writers
                .iter()
                .find_map(|(name, named)| reaching(&level, name, named))
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The descriptors of the project's group `name`.
    async fn group_descriptors(&self, name: &str) -> Result<Vec<String>, PrError> {
        let ims = identities_root(&self.collection);
        let url = reqwest::Url::parse_with_params(
            &format!("{ims}/_apis/identities"),
            &[
                ("searchFilter", "General"),
                ("filterValue", &format!("[{}]\\{name}", self.project)),
                ("queryMembership", "None"),
                ("api-version", "7.1"),
            ],
        )
        .map_err(|e| PrError::InvalidRemote(e.to_string()))?;
        let (status, text) = self.send(reqwest::Method::GET, url.as_str(), None).await?;
        let groups: List<Identity> = parsed(status, &text)?;
        Ok(groups
            .value
            .iter()
            .filter_map(|g| descriptor(&g.descriptor))
            .collect())
    }

    /// The groups right above the last group of each path in `level`, each
    /// as its path made one longer; a group in `seen` is not followed
    /// again, and no more than [`GROUP_LIMIT`] are.
    async fn groups_above(
        &self,
        level: &[Vec<String>],
        seen: &mut Vec<String>,
    ) -> Result<Vec<Vec<String>>, PrError> {
        let ims = identities_root(&self.collection);
        let mut above = Vec::new();
        for batch in level.chunks(GROUP_BATCH) {
            let wanted: Vec<&str> = batch
                .iter()
                .filter_map(|p| p.last())
                .map(String::as_str)
                .collect();
            let url = reqwest::Url::parse_with_params(
                &format!("{ims}/_apis/identities"),
                &[
                    ("descriptors", wanted.join(",").as_str()),
                    ("queryMembership", "Direct"),
                    ("api-version", "7.1"),
                ],
            )
            .map_err(|e| PrError::InvalidRemote(e.to_string()))?;
            let (status, text) = self.send(reqwest::Method::GET, url.as_str(), None).await?;
            let groups: List<Identity> = parsed(status, &text)?;
            for group in &groups.value {
                let Some(own) = descriptor(&group.descriptor) else {
                    continue;
                };
                let Some(path) = batch
                    .iter()
                    .find(|p| p.last().is_some_and(|d| d.eq_ignore_ascii_case(&own)))
                else {
                    continue;
                };
                for parent in group.member_of.iter().filter_map(descriptor) {
                    let key = parent.to_ascii_lowercase();
                    if seen.len() >= GROUP_LIMIT || seen.contains(&key) {
                        continue;
                    }
                    seen.push(key);
                    let mut longer = path.clone();
                    longer.push(parent);
                    above.push(longer);
                }
            }
        }
        Ok(above)
    }

    /// `person`'s entry in this repository's access control list, in the
    /// Git Repositories namespace, with its effective bits; why there is
    /// none otherwise (an error, no descriptor, no entry), for the groups to
    /// be asked instead.
    async fn acl_entry(&self, person: &Identity) -> Result<AclEntry, String> {
        let who = descriptor(&person.descriptor).ok_or("the identity has no descriptor")?;
        let repo: Repository = self
            .call(reqwest::Method::GET, "", None)
            .await
            .map_err(|e| format!("reading the repository: {e}"))?;
        let project = repo.project.id;
        if !is_guid(&project) || !is_guid(&repo.id) {
            return Err("the repository has no project and repository IDs".into());
        }
        let token = format!("repoV2/{project}/{}", repo.id);
        let url = reqwest::Url::parse_with_params(
            &format!(
                "{}/_apis/accesscontrollists/{GIT_NAMESPACE}",
                self.collection
            ),
            &[
                ("token", token.as_str()),
                ("descriptors", who.as_str()),
                ("includeExtendedInfo", "true"),
                ("api-version", "7.1"),
            ],
        )
        .map_err(|e| e.to_string())?;
        let (status, text) = self
            .send(reqwest::Method::GET, url.as_str(), None)
            .await
            .map_err(|e| format!("reading the ACL: {e}"))?;
        let acls: List<Acl> = parsed(status, &text).map_err(|e| format!("reading the ACL: {e}"))?;
        acls.value
            .iter()
            .filter(|acl| acl.token.eq_ignore_ascii_case(&token))
            .flat_map(|acl| &acl.aces_dictionary)
            .find(|(key, _)| key.eq_ignore_ascii_case(&who))
            .map(|(_, ace)| AclEntry {
                allow: ace.allow,
                deny: ace.deny,
                effective: ace
                    .extended_info
                    .as_ref()
                    .map(|e| (e.effective_allow, e.effective_deny)),
            })
            .ok_or_else(|| format!("the ACL of `{token}` has no entry for the identity"))
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
            self.collection,
            azure_segment(&self.project),
            azure_segment(&self.repo)
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
        let token = self.token.token().await?;
        let mut request = if is_entra_token(&token) {
            request.bearer_auth(&token)
        } else {
            request.basic_auth("aegis", Some(&token))
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
    Ok(issue_entra(authority, tenant, client_id, secret).await?.0)
}

/// A service principal's token, issued when first asked and again once it
/// is within [`ENTRA_REFRESH_MARGIN`] of expiring (#643): what a job
/// longer than the token's hour, and the webhook's notifier, ask.
pub struct EntraToken {
    authority: String,
    tenant: String,
    client_id: String,
    secret: String,
    issued: std::sync::Mutex<Option<(String, Instant)>>,
}

impl fmt::Debug for EntraToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntraToken({}/{})", self.tenant, self.client_id)
    }
}

impl EntraToken {
    pub fn new(authority: &str, tenant: &str, client_id: &str, secret: &str) -> Self {
        Self {
            authority: authority.to_owned(),
            tenant: tenant.to_owned(),
            client_id: client_id.to_owned(),
            secret: secret.to_owned(),
            issued: std::sync::Mutex::new(None),
        }
    }

    /// The token to send now: the one held while it has more than
    /// [`ENTRA_REFRESH_MARGIN`] left, a new one otherwise.
    /// Two callers that find it expiring together may both get a new
    /// one: both are good.
    pub async fn token(&self) -> Result<String, PrError> {
        if let Some((token, expires)) = self.held()
            && Instant::now() + ENTRA_REFRESH_MARGIN < expires
        {
            return Ok(token);
        }
        let (token, lifetime) =
            issue_entra(&self.authority, &self.tenant, &self.client_id, &self.secret).await?;
        *self
            .issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((token.clone(), Instant::now() + lifetime));
        Ok(token)
    }

    fn held(&self) -> Option<(String, Instant)> {
        self.issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A new token and how long it lives (`expires_in`; an hour when Entra
/// does not say).
async fn issue_entra(
    authority: &str,
    tenant: &str,
    client_id: &str,
    secret: &str,
) -> Result<(String, Duration), PrError> {
    #[derive(Deserialize)]
    struct Issued {
        access_token: String,
        #[serde(default = "an_hour")]
        expires_in: u64,
    }
    fn an_hour() -> u64 {
        3600
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
    Ok((issued.access_token, Duration::from_secs(issued.expires_in)))
}

/// Whether `name` is an Azure DevOps project or repository name aegis
/// takes: letters, digits, `-`, `_`, `.` and spaces inside (#641); never
/// `/`, `..`, `%` or a control character, nor a leading or trailing space.
pub fn azure_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.trim() == name
        && !name.contains("..")
        && name != "."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_. ".contains(c))
}

/// An [`azure_name`] as a URL path segment: its spaces as `%20`.
pub fn azure_segment(name: &str) -> String {
    name.replace(' ', "%20")
}

/// The name a URL path segment holds, `%20` read as a space: `None` when
/// it is not an [`azure_name`] (another escape, `/`, …).
pub fn azure_unescaped(segment: &str) -> Option<String> {
    let name = segment.replace("%20", " ");
    azure_name(&name).then_some(name)
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

/// The first path in `paths` whose last group is one of `named`, as the
/// way into the writer group `name`.
fn reaching(paths: &[Vec<String>], name: &'static str, named: &[String]) -> Option<WriterGroup> {
    paths
        .iter()
        .find(|p| {
            p.last()
                .is_some_and(|d| named.iter().any(|n| n.eq_ignore_ascii_case(d)))
        })
        .map(|p| WriterGroup {
            name,
            via: p.clone(),
        })
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
        let spaced =
            AzureDevOps::new("https://dev.azure.com/acme", "My Shop", "the cart", "t").unwrap();
        assert_eq!(
            spaced.remote().unwrap().url(),
            "https://dev.azure.com/acme/My%20Shop/_git/the%20cart"
        );
        assert_eq!(
            spaced.pull_url(12),
            "https://dev.azure.com/acme/My%20Shop/_git/the%20cart/pullrequest/12"
        );
        for bad in [
            "a/b",
            "..",
            "a..b",
            " Shop",
            "Shop ",
            "Sh\top",
            "Sh\u{7}op",
            "My%20Shop",
            "",
        ] {
            assert!(
                AzureDevOps::new("https://dev.azure.com/acme", bad, "cart", "t").is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn names_with_spaces_are_escaped_in_urls_only() {
        assert!(azure_name("My Project"));
        assert!(azure_name("cart.v2"));
        assert_eq!(azure_segment("My Project"), "My%20Project");
        assert_eq!(
            azure_unescaped("My%20Project").as_deref(),
            Some("My Project")
        );
        assert_eq!(azure_unescaped("Shop").as_deref(), Some("Shop"));
        for bad in ["My%2FProject", "a%2e%2e", "%20Shop", "a/b", "..", ""] {
            assert_eq!(azure_unescaped(bad), None, "{bad}");
        }
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
