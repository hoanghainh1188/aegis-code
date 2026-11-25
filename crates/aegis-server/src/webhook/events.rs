//! What a delivery asks for. Only the fields needed are read, and only to
//! build the job's `issue_ref` from names and numbers checked here: the
//! issue's title, body and comments are untrusted (design §7) and stay out
//! of the job; the worker reads the issue itself.

use aegis_store::WebhookSource;
use serde::Deserialize;

/// The longest `issue_ref` built, as the internal API allows.
const MAX_ISSUE_REF_LEN: usize = 256;

/// A job a delivery asks for, before its repo is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// The repository's web URL, to find it in `repos`.
    pub repo_url: String,
    pub issue_ref: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The trigger label was put on an issue. `author`: who opened the issue.
    Issue { author: Option<String> },
    /// CI failed on `branch`; a job only if it is the repo's default one.
    CiFailure { branch: String },
    /// A review round asked for on the agent's open PR `pr_url` (#244):
    /// changes requested, or `/aegis fix` in a comment, by `sender`, whose
    /// right to write is checked before a job is made.
    ReviewRound { pr_url: String, sender: String },
    /// An agent PR (`agent/*`) was merged (#456): extract `merged_pr` and
    /// `review_lesson` memories for its repository.
    PrMerged { pr_url: String },
    /// An agent PR (`agent/*`) was closed without being merged (#657): what
    /// its reviewers said becomes a `rejected_pattern` memory.
    PrClosed { pr_url: String },
    /// An issue was closed as done by `closer` (#657): a diagnosis the
    /// diagnosis lane wrote back to it, not followed by a code fix, is kept
    /// as a `confirmed_diagnosis` memory if `closer` can write to the repo.
    IssueClosed { closer: String },
    /// A push to `branch` (#660): the repo's runbooks and business rules
    /// are synced if it is its default branch.
    Push { branch: String },
}

/// The command that asks for a review round in a comment on a PR.
pub const FIX_COMMAND: &str = "/aegis fix";

/// Whether a line of `body` is [`FIX_COMMAND`], alone or followed by a
/// space.
fn asks_for_fix(body: &str) -> bool {
    body.lines().any(|line| {
        let line = line.trim();
        line == FIX_COMMAND
            || line
                .strip_prefix(FIX_COMMAND)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    })
}

/// Why a delivery asks for nothing, or cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Ignored(String),
    Malformed(String),
}

fn ignored(why: impl Into<String>) -> Skip {
    Skip::Ignored(why.into())
}

/// The `action` of a GitHub delivery, read alone to pick how to read the
/// rest.
fn body_action(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Action {
        action: String,
    }
    serde_json::from_slice::<Action>(body)
        .ok()
        .map(|a| a.action)
}

fn parse<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, Skip> {
    serde_json::from_slice(body).map_err(|e| Skip::Malformed(e.to_string()))
}

/// Letters, digits, `-`, `_`, `.`: an owner, group or repository name.
fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `owner/name` on GitHub, `group/sub/project` on GitLab.
fn checked_path(path: &str, max_parts: usize) -> Result<&str, Skip> {
    let parts: Vec<&str> = path.split('/').collect();
    if (2..=max_parts).contains(&parts.len()) && parts.iter().all(|p| plain_name(p)) {
        Ok(path)
    } else {
        Err(Skip::Malformed(format!(
            "`{path}` is not a repository path"
        )))
    }
}

fn checked_ref(issue_ref: String) -> Result<String, Skip> {
    if issue_ref.len() <= MAX_ISSUE_REF_LEN {
        Ok(issue_ref)
    } else {
        Err(Skip::Malformed("the repository path is too long".into()))
    }
}

#[derive(Debug, Deserialize)]
struct GithubRepo {
    full_name: String,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct Named {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GithubIssues {
    action: String,
    label: Option<Named>,
    issue: GithubIssue,
    repository: GithubRepo,
    #[serde(default)]
    sender: Option<Sender>,
}

#[derive(Debug, Deserialize)]
struct GithubIssue {
    number: u64,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
    #[serde(default)]
    user: Option<Sender>,
    /// Why it was closed: `completed`, `not_planned` or `duplicate`.
    #[serde(default)]
    state_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubWorkflowRun {
    action: String,
    workflow_run: WorkflowRun,
    repository: GithubRepo,
}

#[derive(Debug, Deserialize)]
struct WorkflowRun {
    id: u64,
    conclusion: Option<String>,
    head_branch: Option<String>,
    event: String,
    head_repository: Option<HeadRepo>,
}

#[derive(Debug, Deserialize)]
struct HeadRepo {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct Sender {
    login: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct PrHead {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Debug, Deserialize)]
struct GithubPull {
    number: u64,
    html_url: String,
    state: String,
    #[serde(default)]
    merged: bool,
    head: PrHead,
}

#[derive(Debug, Deserialize)]
struct GithubPullEvent {
    action: String,
    pull_request: GithubPull,
    repository: GithubRepo,
}

#[derive(Debug, Deserialize)]
struct Review {
    state: String,
}

#[derive(Debug, Deserialize)]
struct GithubReview {
    action: String,
    review: Review,
    pull_request: GithubPull,
    repository: GithubRepo,
    sender: Sender,
}

#[derive(Debug, Deserialize)]
struct Commented {
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubReviewComment {
    action: String,
    comment: Commented,
    pull_request: GithubPull,
    repository: GithubRepo,
    sender: Sender,
}

#[derive(Debug, Deserialize)]
struct CommentedIssue {
    number: u64,
    html_url: String,
    state: String,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GithubIssueComment {
    action: String,
    comment: Commented,
    issue: CommentedIssue,
    repository: GithubRepo,
    sender: Sender,
}

/// A review round on PR `number` of `repo` at `pr_url`, by a person.
fn review_round(
    repo: GithubRepo,
    number: u64,
    pr_url: String,
    sender: Sender,
) -> Result<Wanted, Skip> {
    if sender.kind == "Bot" {
        return Err(ignored("sent by a bot"));
    }
    let path = checked_path(&repo.full_name, 2)?;
    Ok(Wanted {
        issue_ref: checked_ref(format!("{path}#{number}"))?,
        repo_url: repo.html_url,
        kind: Kind::ReviewRound {
            pr_url,
            sender: sender.login,
        },
    })
}

/// An open PR from an `agent/*` branch.
fn agent_pull(pull: &GithubPull) -> Result<(), Skip> {
    if pull.state != "open" {
        return Err(ignored("the pull request is not open"));
    }
    if !pull.head.name.starts_with("agent/") {
        return Err(ignored("not a pull request of the agent"));
    }
    Ok(())
}

/// A delivery of GitHub event `event` (`X-GitHub-Event`).
/// The repo a delivery names (`repository.html_url` on GitHub,
/// `project.web_url` on GitLab), to match a delivery that made no job to
/// its repo (#331). Read only from a delivery that verified.
pub fn repo_url(source: WebhookSource, body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let pointer = match source {
        // Its payloads name a repository by key and slug only.
        WebhookSource::BitbucketDc => return None,
        WebhookSource::AzureDevops => {
            let url = [
                "/resource/pullRequest/repository/remoteUrl",
                "/resource/repository/remoteUrl",
            ]
            .iter()
            .find_map(|p| value.pointer(p)?.as_str())?;
            let url = without_user(url);
            return (url.len() <= MAX_ISSUE_REF_LEN).then_some(url);
        }
        WebhookSource::Gitlab => "/project/web_url",
        WebhookSource::Bitbucket => "/repository/links/html/href",
        WebhookSource::Github
        | WebhookSource::Jira
        | WebhookSource::Backlog
        | WebhookSource::Jsm
        | WebhookSource::Servicenow => "/repository/html_url",
    };
    let url = value.pointer(pointer)?.as_str()?;
    (url.len() <= MAX_ISSUE_REF_LEN).then(|| url.to_owned())
}

pub fn github(event: &str, body: &[u8], label: &str) -> Result<Wanted, Skip> {
    match event {
        "push" => {
            let e: GithubPush = parse(body)?;
            let branch = pushed_branch(&e.pushed.pushed)?;
            let path = checked_path(&e.repository.full_name, 2)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}@{branch}"))?,
                repo_url: e.repository.html_url,
                kind: Kind::Push { branch },
            })
        }
        "pull_request_review" => {
            let e: GithubReview = parse(body)?;
            if e.action != "submitted" || !e.review.state.eq_ignore_ascii_case("changes_requested")
            {
                return Err(ignored("not a review that requests changes"));
            }
            agent_pull(&e.pull_request)?;
            let (number, url) = (e.pull_request.number, e.pull_request.html_url);
            review_round(e.repository, number, url, e.sender)
        }
        "pull_request_review_comment" => {
            let e: GithubReviewComment = parse(body)?;
            if e.action != "created" || !asks_for_fix(e.comment.body.as_deref().unwrap_or("")) {
                return Err(ignored(format!("not a comment with `{FIX_COMMAND}`")));
            }
            agent_pull(&e.pull_request)?;
            let (number, url) = (e.pull_request.number, e.pull_request.html_url);
            review_round(e.repository, number, url, e.sender)
        }
        "issue_comment" => {
            let e: GithubIssueComment = parse(body)?;
            if e.action != "created" || !asks_for_fix(e.comment.body.as_deref().unwrap_or("")) {
                return Err(ignored(format!("not a comment with `{FIX_COMMAND}`")));
            }
            if e.issue.pull_request.is_none() {
                return Err(ignored("a comment on an issue, not a pull request"));
            }
            if e.issue.state != "open" {
                return Err(ignored("the pull request is not open"));
            }
            // An issue comment does not say the PR's branch: the job that
            // opened the PR is what makes it the agent's.
            review_round(e.repository, e.issue.number, e.issue.html_url, e.sender)
        }
        "issues" if body_action(body).as_deref() == Some("closed") => {
            let e: GithubIssues = parse(body)?;
            if e.issue.pull_request.is_some() {
                return Err(ignored("a pull request, not an issue"));
            }
            // Closed as not planned or as a duplicate confirms nothing.
            if e.issue
                .state_reason
                .as_deref()
                .is_some_and(|r| r != "completed")
            {
                return Err(ignored("an issue closed as not done"));
            }
            let closer = match e.sender {
                Some(s) if s.kind != "Bot" && !s.login.is_empty() => s.login,
                _ => return Err(ignored("not closed by a person")),
            };
            let path = checked_path(&e.repository.full_name, 2)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}#{}", e.issue.number))?,
                repo_url: e.repository.html_url,
                kind: Kind::IssueClosed { closer },
            })
        }
        "issues" => {
            let e: GithubIssues = parse(body)?;
            if e.action != "labeled" || e.label.is_none_or(|l| l.name != label) {
                return Err(ignored(format!("not the label `{label}` put on an issue")));
            }
            if e.issue.pull_request.is_some() {
                return Err(ignored("a pull request, not an issue"));
            }
            let path = checked_path(&e.repository.full_name, 2)?;
            let author = e.issue.user.map(|u| u.login);
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}#{}", e.issue.number))?,
                repo_url: e.repository.html_url,
                kind: Kind::Issue { author },
            })
        }
        "workflow_run" => {
            let e: GithubWorkflowRun = parse(body)?;
            let run = e.workflow_run;
            if e.action != "completed" || run.conclusion.as_deref() != Some("failure") {
                return Err(ignored("not a failed run"));
            }
            // A pull request's run has the head branch of the PR, which
            // anyone may name `main`: only runs of the repo's own pushes.
            let own = run
                .head_repository
                .is_some_and(|h| h.full_name == e.repository.full_name);
            if !own || run.event.starts_with("pull_request") {
                return Err(ignored("a run of a pull request"));
            }
            let branch = run
                .head_branch
                .ok_or_else(|| ignored("a run of no branch"))?;
            let path = checked_path(&e.repository.full_name, 2)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}/actions/runs/{}", run.id))?,
                repo_url: e.repository.html_url,
                kind: Kind::CiFailure { branch },
            })
        }
        "pull_request" => {
            let e: GithubPullEvent = parse(body)?;
            if e.action != "closed" {
                return Err(ignored("not a closed pull request"));
            }
            if !e.pull_request.head.name.starts_with("agent/") {
                return Err(ignored("not a pull request of the agent"));
            }
            let path = checked_path(&e.repository.full_name, 2)?;
            let pr_url = e.pull_request.html_url;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}#{}", e.pull_request.number))?,
                repo_url: e.repository.html_url,
                kind: if e.pull_request.merged {
                    Kind::PrMerged { pr_url }
                } else {
                    Kind::PrClosed { pr_url }
                },
            })
        }
        other => Err(ignored(format!("event `{other}`"))),
    }
}

/// A push, on GitHub and GitLab alike: the ref it moved.
#[derive(Debug, Deserialize)]
struct PushRef {
    #[serde(rename = "ref")]
    pushed: String,
}

/// The branch a push moved; `None` for a tag.
fn pushed_branch(pushed: &str) -> Result<String, Skip> {
    pushed
        .strip_prefix("refs/heads/")
        .filter(|b| !b.is_empty() && b.len() <= MAX_ISSUE_REF_LEN)
        .map(ToOwned::to_owned)
        .ok_or_else(|| ignored("a push of a tag, not a branch"))
}

#[derive(Debug, Deserialize)]
struct GithubPush {
    #[serde(flatten)]
    pushed: PushRef,
    repository: GithubRepo,
}

#[derive(Debug, Deserialize)]
struct GitlabPush {
    #[serde(flatten)]
    pushed: PushRef,
    project: GitlabProject,
}

#[derive(Debug, Deserialize)]
struct GitlabProject {
    path_with_namespace: String,
    web_url: String,
}

#[derive(Debug, Deserialize)]
struct Titled {
    title: String,
}

#[derive(Debug, Deserialize)]
struct GitlabIssueHook {
    project: GitlabProject,
    object_attributes: GitlabIssue,
    #[serde(default)]
    changes: GitlabChanges,
    #[serde(default)]
    user: Option<GitlabUser>,
}

#[derive(Debug, Deserialize)]
struct GitlabIssue {
    iid: u64,
    #[serde(default)]
    author_id: Option<u64>,
    #[serde(default)]
    action: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct GitlabChanges {
    labels: Option<LabelChange>,
}

#[derive(Debug, Deserialize)]
struct LabelChange {
    #[serde(default)]
    previous: Vec<Titled>,
    #[serde(default)]
    current: Vec<Titled>,
}

#[derive(Debug, Deserialize)]
struct GitlabPipelineHook {
    project: GitlabProject,
    object_attributes: Pipeline,
}

#[derive(Debug, Deserialize)]
struct Pipeline {
    id: u64,
    #[serde(rename = "ref")]
    branch: String,
    #[serde(default)]
    tag: bool,
    status: String,
    #[serde(default)]
    source: Option<String>,
}

/// GitLab subgroups nest up to 20 deep.
const MAX_GITLAB_PARTS: usize = 21;

#[derive(Debug, Deserialize)]
struct GitlabNoteHook {
    project: GitlabProject,
    user: GitlabUser,
    object_attributes: GitlabNote,
    #[serde(default)]
    merge_request: Option<GitlabMergeRequest>,
}

#[derive(Debug, Deserialize)]
struct GitlabUser {
    id: u64,
    #[serde(default)]
    bot: bool,
}

#[derive(Debug, Deserialize)]
struct GitlabNote {
    #[serde(default)]
    note: String,
    noteable_type: String,
}

#[derive(Debug, Deserialize)]
struct GitlabMergeRequest {
    iid: u64,
    url: String,
    state: String,
    #[serde(default)]
    action: Option<String>,
    source_branch: String,
}

#[derive(Debug, Deserialize)]
struct GitlabMergeRequestHook {
    project: GitlabProject,
    object_attributes: GitlabMergeRequest,
}

/// A delivery of GitLab event `event` (`X-Gitlab-Event`).
pub fn gitlab(event: &str, body: &[u8], label: &str) -> Result<Wanted, Skip> {
    match event {
        "Push Hook" => {
            let e: GitlabPush = parse(body)?;
            let branch = pushed_branch(&e.pushed.pushed)?;
            let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}@{branch}"))?,
                repo_url: e.project.web_url,
                kind: Kind::Push { branch },
            })
        }
        // `/aegis fix` on the agent's MR (#254); the sender is GitLab's
        // user id, whose access level is checked before a job is made.
        "Note Hook" => {
            let e: GitlabNoteHook = parse(body)?;
            if e.object_attributes.noteable_type != "MergeRequest"
                || !asks_for_fix(&e.object_attributes.note)
            {
                return Err(ignored(format!(
                    "not a comment with `{FIX_COMMAND}` on a merge request"
                )));
            }
            let mr = e
                .merge_request
                .ok_or_else(|| Skip::Malformed("a merge request note without its MR".into()))?;
            if mr.state != "opened" {
                return Err(ignored("the merge request is not open"));
            }
            if !mr.source_branch.starts_with("agent/") {
                return Err(ignored("not a merge request of the agent"));
            }
            if e.user.bot {
                return Err(ignored("sent by a bot"));
            }
            let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}!{}", mr.iid))?,
                repo_url: e.project.web_url,
                kind: Kind::ReviewRound {
                    pr_url: mr.url,
                    sender: e.user.id.to_string(),
                },
            })
        }
        "Issue Hook" => {
            let e: GitlabIssueHook = parse(body)?;
            if e.object_attributes.action.as_deref() == Some("close") {
                let closer = match e.user {
                    Some(u) if !u.bot => u.id.to_string(),
                    _ => return Err(ignored("not closed by a person")),
                };
                let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
                return Ok(Wanted {
                    issue_ref: checked_ref(format!("{path}#{}", e.object_attributes.iid))?,
                    repo_url: e.project.web_url,
                    kind: Kind::IssueClosed { closer },
                });
            }
            let has = |list: &[Titled]| list.iter().any(|l| l.title == label);
            let added = e
                .changes
                .labels
                .is_some_and(|c| has(&c.current) && !has(&c.previous));
            if !added {
                return Err(ignored(format!("not the label `{label}` put on an issue")));
            }
            let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
            let author = e.object_attributes.author_id.map(|id| id.to_string());
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}#{}", e.object_attributes.iid))?,
                repo_url: e.project.web_url,
                kind: Kind::Issue { author },
            })
        }
        "Pipeline Hook" => {
            let e: GitlabPipelineHook = parse(body)?;
            let p = e.object_attributes;
            if p.status != "failed" {
                return Err(ignored("not a failed pipeline"));
            }
            if p.tag || p.source.as_deref() == Some("merge_request_event") {
                return Err(ignored("a pipeline of a tag or merge request"));
            }
            let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}/-/pipelines/{}", p.id))?,
                repo_url: e.project.web_url,
                kind: Kind::CiFailure { branch: p.branch },
            })
        }
        "Merge Request Hook" => {
            let e: GitlabMergeRequestHook = parse(body)?;
            let mr = e.object_attributes;
            let merged = mr.state == "merged" || mr.action.as_deref() == Some("merge");
            let closed = mr.action.as_deref() == Some("close");
            if !merged && !closed {
                return Err(ignored("not a merged or closed merge request"));
            }
            if !mr.source_branch.starts_with("agent/") {
                return Err(ignored("not a merge request of the agent"));
            }
            let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}!{}", mr.iid))?,
                repo_url: e.project.web_url,
                kind: if merged {
                    Kind::PrMerged { pr_url: mr.url }
                } else {
                    Kind::PrClosed { pr_url: mr.url }
                },
            })
        }
        other => Err(ignored(format!("event `{other}`"))),
    }
}

#[derive(Debug, Deserialize)]
struct BitbucketHref {
    href: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketLinks {
    html: BitbucketHref,
}

#[derive(Debug, Deserialize)]
struct BitbucketBranch {
    name: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketEnd {
    branch: BitbucketBranch,
}

#[derive(Debug, Deserialize)]
struct BitbucketPull {
    id: u64,
    state: String,
    source: BitbucketEnd,
    links: BitbucketLinks,
}

#[derive(Debug, Deserialize)]
struct BitbucketRepo {
    full_name: String,
    links: BitbucketLinks,
}

#[derive(Debug, Deserialize)]
struct BitbucketActor {
    #[serde(default)]
    uuid: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketContent {
    #[serde(default)]
    raw: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketComment {
    content: BitbucketContent,
}

/// A pull request event of Bitbucket Cloud: who, which PR, which
/// repository, and the comment when there is one.
#[derive(Debug, Deserialize)]
struct BitbucketPullEvent {
    actor: BitbucketActor,
    pullrequest: BitbucketPull,
    repository: BitbucketRepo,
    #[serde(default)]
    comment: Option<BitbucketComment>,
}

/// A delivery of Bitbucket Cloud event `event` (`X-Event-Key`, #634):
/// `/aegis fix` in a comment on the agent's open PR, or changes requested
/// on it, by a person, asks for a review round; the agent's PR merged is
/// recorded (#456). The sender is their `{uuid}`, whose right to write is
/// checked before a job is made.
pub fn bitbucket(event: &str, body: &[u8]) -> Result<Wanted, Skip> {
    let round = match event {
        "pullrequest:comment_created" => true,
        "pullrequest:changes_request_created" => true,
        "pullrequest:fulfilled" | "pullrequest:rejected" => false,
        other => return Err(ignored(format!("event `{other}`"))),
    };
    let e: BitbucketPullEvent = parse(body)?;
    if event == "pullrequest:comment_created"
        && !asks_for_fix(e.comment.as_ref().map_or("", |c| c.content.raw.as_str()))
    {
        return Err(ignored(format!("not a comment with `{FIX_COMMAND}`")));
    }
    if !e.pullrequest.source.branch.name.starts_with("agent/") {
        return Err(ignored("not a pull request of the agent"));
    }
    let path = checked_path(&e.repository.full_name, 2)?;
    let issue_ref = checked_ref(format!("{path}#{}", e.pullrequest.id))?;
    let pr_url = e.pullrequest.links.html.href;
    let kind = if round {
        if e.pullrequest.state != "OPEN" {
            return Err(ignored("the pull request is not open"));
        }
        if e.actor.kind != "user" || e.actor.uuid.is_empty() {
            return Err(ignored("not sent by a person"));
        }
        Kind::ReviewRound {
            pr_url,
            sender: e.actor.uuid,
        }
    } else if event == "pullrequest:rejected" {
        Kind::PrClosed { pr_url }
    } else {
        Kind::PrMerged { pr_url }
    };
    Ok(Wanted {
        issue_ref,
        repo_url: e.repository.links.html.href,
        kind,
    })
}

#[derive(Debug, Deserialize)]
struct DcProject {
    key: String,
}

#[derive(Debug, Deserialize)]
struct DcRepo {
    slug: String,
    project: DcProject,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DcRef {
    display_id: String,
    repository: DcRepo,
}

#[derive(Debug, Deserialize)]
struct DcSelf {
    href: String,
}

#[derive(Debug, Deserialize)]
struct DcLinks {
    #[serde(rename = "self", default)]
    own: Vec<DcSelf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DcPull {
    id: u64,
    state: String,
    from_ref: DcRef,
    to_ref: DcRef,
    links: DcLinks,
}

#[derive(Debug, Deserialize)]
struct DcActor {
    #[serde(default)]
    slug: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct DcComment {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DcPullEvent {
    actor: DcActor,
    pull_request: DcPull,
    #[serde(default)]
    comment: Option<DcComment>,
}

/// A delivery of Bitbucket Data Center event `event` (`X-Event-Key`,
/// #635): `/aegis fix` in a comment on the agent's open PR, or a reviewer
/// marking it "needs work", by a person, asks for a review round; the
/// agent's PR merged is recorded (#456). The sender is their user slug,
/// whose right to write is checked before a job is made.
pub fn bitbucket_dc(event: &str, body: &[u8]) -> Result<Wanted, Skip> {
    let round = match event {
        "pr:comment:added" | "pr:reviewer:needs_work" => true,
        "pr:merged" | "pr:declined" => false,
        other => return Err(ignored(format!("event `{other}`"))),
    };
    let e: DcPullEvent = parse(body)?;
    if event == "pr:comment:added"
        && !asks_for_fix(e.comment.as_ref().map_or("", |c| c.text.as_str()))
    {
        return Err(ignored(format!("not a comment with `{FIX_COMMAND}`")));
    }
    let pull = e.pull_request;
    if !pull.from_ref.display_id.starts_with("agent/") {
        return Err(ignored("not a pull request of the agent"));
    }
    let (from, to) = (&pull.from_ref.repository, &pull.to_ref.repository);
    if !(from.slug.eq_ignore_ascii_case(&to.slug)
        && from.project.key.eq_ignore_ascii_case(&to.project.key))
    {
        return Err(ignored("a pull request from another repository"));
    }
    let path = checked_path(&format!("{}/{}", to.project.key, to.slug), 2)?.to_owned();
    let issue_ref = checked_ref(format!("{path}#{}", pull.id))?;
    let pr_url = pull
        .links
        .own
        .into_iter()
        .next()
        .map(|l| l.href)
        .filter(|u| u.len() <= MAX_ISSUE_REF_LEN)
        .ok_or_else(|| Skip::Malformed("a pull request without its link".into()))?;
    let repo_url = pr_url
        .rsplit_once("/pull-requests/")
        .map_or_else(|| pr_url.clone(), |(repo, _)| repo.to_owned());
    let kind = if round {
        if pull.state != "OPEN" {
            return Err(ignored("the pull request is not open"));
        }
        if e.actor.kind != "NORMAL" || e.actor.slug.is_empty() {
            return Err(ignored("not sent by a person"));
        }
        Kind::ReviewRound {
            pr_url,
            sender: e.actor.slug,
        }
    } else if event == "pr:declined" {
        Kind::PrClosed { pr_url }
    } else {
        Kind::PrMerged { pr_url }
    };
    Ok(Wanted {
        issue_ref,
        repo_url,
        kind,
    })
}

#[derive(Debug, Deserialize)]
struct AzProject {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AzRepo {
    name: String,
    project: AzProject,
    remote_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AzPull {
    pull_request_id: u64,
    status: String,
    source_ref_name: String,
    repository: AzRepo,
    #[serde(default)]
    fork_source: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AzAuthor {
    #[serde(default)]
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AzComment {
    #[serde(default)]
    content: String,
    author: AzAuthor,
    #[serde(default)]
    comment_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AzCommented {
    comment: AzComment,
    pull_request: AzPull,
}

#[derive(Debug, Deserialize)]
struct AzEvent<T> {
    resource: T,
}

/// The service hook event of an Azure DevOps delivery (#636).
pub const AZURE_COMMENTED: &str = "ms.vss-code.git-pullrequest-comment-event";
pub const AZURE_UPDATED: &str = "git.pullrequest.updated";

/// A delivery of Azure DevOps service hook event `event` (its body's
/// `eventType`, #636): `/aegis fix` in a comment on the agent's active PR
/// asks for a review round; the agent's PR completed is recorded as
/// merged (#456). The sender is the comment author's identity ID, whose
/// right to write is checked before a job is made.
pub fn azure_devops(event: &str, body: &[u8]) -> Result<Wanted, Skip> {
    let (pull, sender) = match event {
        AZURE_COMMENTED => {
            let e: AzEvent<AzCommented> = parse(body)?;
            let comment = e.resource.comment;
            if comment.comment_type != "text" || !asks_for_fix(&comment.content) {
                return Err(ignored(format!("not a comment with `{FIX_COMMAND}`")));
            }
            (e.resource.pull_request, Some(comment.author.id))
        }
        AZURE_UPDATED => (parse::<AzEvent<AzPull>>(body)?.resource, None),
        other => return Err(ignored(format!("event `{other}`"))),
    };
    if !pull.source_ref_name.starts_with("refs/heads/agent/") {
        return Err(ignored("not a pull request of the agent"));
    }
    if pull.fork_source.is_some() {
        return Err(ignored("a pull request from another repository"));
    }
    let repo = &pull.repository;
    // Names may hold spaces; the ref keeps them as Azure shows them (#641).
    if !aegis_pr::azure_name(&repo.project.name) || !aegis_pr::azure_name(&repo.name) {
        return Err(Skip::Malformed(format!(
            "`{}/{}` is not a project and repository name",
            repo.project.name, repo.name
        )));
    }
    let path = format!("{}/{}", repo.project.name, repo.name);
    let issue_ref = checked_ref(format!("{path}#{}", pull.pull_request_id))?;
    let repo_url = without_user(&repo.remote_url);
    let pr_url = format!("{repo_url}/pullrequest/{}", pull.pull_request_id);
    if !repo_url.starts_with("https://") || pr_url.len() > MAX_ISSUE_REF_LEN {
        return Err(Skip::Malformed("a repository without its URL".into()));
    }
    let kind = match sender {
        Some(sender) => {
            if pull.status != "active" {
                return Err(ignored("the pull request is not active"));
            }
            if sender.is_empty() {
                return Err(ignored("not sent by a person"));
            }
            Kind::ReviewRound { pr_url, sender }
        }
        None if pull.status == "completed" => Kind::PrMerged { pr_url },
        None if pull.status == "abandoned" => Kind::PrClosed { pr_url },
        None => return Err(ignored("the pull request is not completed or abandoned")),
    };
    Ok(Wanted {
        issue_ref,
        repo_url,
        kind,
    })
}

/// A clone URL without the user name Azure DevOps puts in it.
fn without_user(url: &str) -> String {
    match url.strip_prefix("https://").and_then(|r| r.split_once('@')) {
        Some((user, rest)) if !user.contains('/') => format!("https://{rest}"),
        _ => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AZ_WRITER: &str = "6f5e1b2a-0c3d-4e5f-8a9b-0c1d2e3f4a5b";

    fn az_pull(status: &str, branch: &str) -> serde_json::Value {
        json!({
            "pullRequestId": 12, "status": status,
            "sourceRefName": format!("refs/heads/{branch}"),
            "targetRefName": "refs/heads/main",
            "repository": {
                "name": "cart", "project": { "name": "Shop" },
                "remoteUrl": "https://acme@dev.azure.com/acme/Shop/_git/cart"
            }
        })
    }

    fn az_comment(text: &str, kind: &str, status: &str, branch: &str) -> Vec<u8> {
        json!({
            "id": "d1", "eventType": AZURE_COMMENTED,
            "resource": {
                "comment": { "content": text, "commentType": kind, "author": { "id": AZ_WRITER } },
                "pullRequest": az_pull(status, branch),
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn an_azure_project_with_a_space_keeps_its_name_in_the_ref() {
        let event = |project: &str| {
            let mut pull = az_pull("completed", "agent/cart-1");
            pull["repository"]["project"]["name"] = json!(project);
            pull["repository"]["remoteUrl"] =
                json!("https://dev.azure.com/acme/My%20Shop/_git/cart");
            json!({ "eventType": AZURE_UPDATED, "resource": pull }).to_string()
        };
        let merged = azure_devops(AZURE_UPDATED, event("My Shop").as_bytes()).unwrap();
        assert_eq!(merged.issue_ref, "My Shop/cart#12");
        for bad in ["My/Shop", "My..Shop", "My\nShop", " Shop"] {
            assert!(
                matches!(
                    azure_devops(AZURE_UPDATED, event(bad).as_bytes()),
                    Err(Skip::Malformed(_))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_fix_comment_on_an_agent_pr_on_azure_asks_for_a_round() {
        let wanted = azure_devops(
            AZURE_COMMENTED,
            &az_comment("Please:\n/aegis fix", "text", "active", "agent/cart-1"),
        )
        .unwrap();
        assert_eq!(wanted.issue_ref, "Shop/cart#12");
        assert_eq!(wanted.repo_url, "https://dev.azure.com/acme/Shop/_git/cart");
        assert_eq!(
            wanted.kind,
            Kind::ReviewRound {
                pr_url: "https://dev.azure.com/acme/Shop/_git/cart/pullrequest/12".into(),
                sender: AZ_WRITER.into()
            }
        );
        let completed =
            json!({ "eventType": AZURE_UPDATED, "resource": az_pull("completed", "agent/cart-1") });
        let merged = azure_devops(AZURE_UPDATED, completed.to_string().as_bytes()).unwrap();
        assert!(matches!(merged.kind, Kind::PrMerged { .. }));
        for (text, kind, status, branch) in [
            ("looks fine", "text", "active", "agent/cart-1"),
            ("/aegis fix", "system", "active", "agent/cart-1"),
            ("/aegis fix", "text", "abandoned", "agent/cart-1"),
            ("/aegis fix", "text", "active", "feature/x"),
        ] {
            assert!(
                matches!(
                    azure_devops(AZURE_COMMENTED, &az_comment(text, kind, status, branch)),
                    Err(Skip::Ignored(_))
                ),
                "{text} {kind} {status} {branch}"
            );
        }
        let active = json!({ "resource": az_pull("active", "agent/cart-1") });
        assert!(matches!(
            azure_devops(AZURE_UPDATED, active.to_string().as_bytes()),
            Err(Skip::Ignored(_))
        ));
        assert!(matches!(
            azure_devops("git.push", b"{}"),
            Err(Skip::Ignored(_))
        ));
        assert!(matches!(
            azure_devops(AZURE_COMMENTED, b"{}"),
            Err(Skip::Malformed(_))
        ));
        assert_eq!(
            repo_url(
                WebhookSource::AzureDevops,
                &az_comment("x", "text", "active", "agent/1")
            )
            .as_deref(),
            Some("https://dev.azure.com/acme/Shop/_git/cart")
        );
    }

    fn dc_event(comment: Option<&str>, state: &str, branch: &str, actor: &str) -> Vec<u8> {
        let repo = json!({ "slug": "cart", "project": { "key": "PAY" } });
        json!({
            "eventKey": "pr:comment:added",
            "actor": { "slug": "lan", "type": actor },
            "pullRequest": {
                "id": 12, "state": state,
                "fromRef": { "displayId": branch, "repository": repo },
                "toRef": { "displayId": "main", "repository": repo },
                "links": { "self": [{ "href": "https://git.acme.example/projects/PAY/repos/cart/pull-requests/12" }] }
            },
            "comment": comment.map(|text| json!({ "text": text })),
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_fix_comment_on_an_agent_pr_on_data_center_asks_for_a_round() {
        let wanted = bitbucket_dc(
            "pr:comment:added",
            &dc_event(
                Some("Please:\n/aegis fix"),
                "OPEN",
                "agent/cart-1",
                "NORMAL",
            ),
        )
        .unwrap();
        assert_eq!(wanted.issue_ref, "PAY/cart#12");
        assert_eq!(
            wanted.repo_url,
            "https://git.acme.example/projects/PAY/repos/cart"
        );
        assert_eq!(
            wanted.kind,
            Kind::ReviewRound {
                pr_url: "https://git.acme.example/projects/PAY/repos/cart/pull-requests/12".into(),
                sender: "lan".into()
            }
        );
        let needs_work = bitbucket_dc(
            "pr:reviewer:needs_work",
            &dc_event(None, "OPEN", "agent/cart-1", "NORMAL"),
        )
        .unwrap();
        assert!(matches!(needs_work.kind, Kind::ReviewRound { .. }));
        let merged = bitbucket_dc(
            "pr:merged",
            &dc_event(None, "MERGED", "agent/cart-1", "NORMAL"),
        )
        .unwrap();
        assert!(matches!(merged.kind, Kind::PrMerged { .. }));
        for (event, comment, state, branch, actor) in [
            (
                "pr:comment:added",
                Some("looks fine"),
                "OPEN",
                "agent/cart-1",
                "NORMAL",
            ),
            (
                "pr:comment:added",
                Some("/aegis fix"),
                "DECLINED",
                "agent/cart-1",
                "NORMAL",
            ),
            (
                "pr:comment:added",
                Some("/aegis fix"),
                "OPEN",
                "feature/x",
                "NORMAL",
            ),
            (
                "pr:comment:added",
                Some("/aegis fix"),
                "OPEN",
                "agent/cart-1",
                "SERVICE",
            ),
            (
                "pr:reviewer:approved",
                None,
                "OPEN",
                "agent/cart-1",
                "NORMAL",
            ),
        ] {
            assert!(
                matches!(
                    bitbucket_dc(event, &dc_event(comment, state, branch, actor)),
                    Err(Skip::Ignored(_))
                ),
                "{event} {comment:?} {state} {branch} {actor}"
            );
        }
        assert!(matches!(
            bitbucket_dc("pr:merged", b"{}"),
            Err(Skip::Malformed(_))
        ));
    }

    fn bitbucket_event(comment: Option<&str>, state: &str, branch: &str, actor: &str) -> Vec<u8> {
        json!({
            "actor": { "uuid": "{42}", "type": actor },
            "pullrequest": {
                "id": 7, "state": state,
                "source": { "branch": { "name": branch } },
                "links": { "html": { "href": "https://bitbucket.org/acme/app/pull-requests/7" } }
            },
            "repository": {
                "full_name": "acme/app",
                "links": { "html": { "href": "https://bitbucket.org/acme/app" } }
            },
            "comment": comment.map(|raw| json!({ "content": { "raw": raw } })),
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_fix_comment_on_an_agent_pr_on_bitbucket_asks_for_a_round() {
        let wanted = bitbucket(
            "pullrequest:comment_created",
            &bitbucket_event(Some("Please:\n/aegis fix"), "OPEN", "agent/app-1", "user"),
        )
        .unwrap();
        assert_eq!(wanted.issue_ref, "acme/app#7");
        assert_eq!(wanted.repo_url, "https://bitbucket.org/acme/app");
        assert_eq!(
            wanted.kind,
            Kind::ReviewRound {
                pr_url: "https://bitbucket.org/acme/app/pull-requests/7".into(),
                sender: "{42}".into()
            }
        );
        let changes = bitbucket(
            "pullrequest:changes_request_created",
            &bitbucket_event(None, "OPEN", "agent/app-1", "user"),
        )
        .unwrap();
        assert!(matches!(changes.kind, Kind::ReviewRound { .. }));
        for (event, comment, state, branch, actor) in [
            (
                "pullrequest:comment_created",
                Some("looks fine"),
                "OPEN",
                "agent/app-1",
                "user",
            ),
            (
                "pullrequest:comment_created",
                Some("/aegis fix"),
                "MERGED",
                "agent/app-1",
                "user",
            ),
            (
                "pullrequest:comment_created",
                Some("/aegis fix"),
                "OPEN",
                "feature/x",
                "user",
            ),
            (
                "pullrequest:comment_created",
                Some("/aegis fix"),
                "OPEN",
                "agent/app-1",
                "app_user",
            ),
            ("pullrequest:approved", None, "OPEN", "agent/app-1", "user"),
        ] {
            assert!(
                matches!(
                    bitbucket(event, &bitbucket_event(comment, state, branch, actor)),
                    Err(Skip::Ignored(_))
                ),
                "{event} {comment:?} {state} {branch} {actor}"
            );
        }
    }

    #[test]
    fn a_merged_agent_pr_on_bitbucket_is_recorded() {
        let wanted = bitbucket(
            "pullrequest:fulfilled",
            &bitbucket_event(None, "MERGED", "agent/app-1", "user"),
        )
        .unwrap();
        assert_eq!(
            wanted.kind,
            Kind::PrMerged {
                pr_url: "https://bitbucket.org/acme/app/pull-requests/7".into()
            }
        );
        assert!(matches!(
            bitbucket(
                "pullrequest:fulfilled",
                &bitbucket_event(None, "MERGED", "main", "user")
            ),
            Err(Skip::Ignored(_))
        ));
        assert!(matches!(
            bitbucket("pullrequest:comment_created", b"{}"),
            Err(Skip::Malformed(_))
        ));
    }

    fn pull(state: &str, head: &str) -> serde_json::Value {
        json!({ "number": 100, "html_url": "https://github.com/bot/demo/pull/100",
                "state": state, "head": { "ref": head } })
    }

    fn repo() -> serde_json::Value {
        json!({ "full_name": "bot/demo", "html_url": "https://github.com/bot/demo" })
    }

    fn person() -> serde_json::Value {
        json!({ "login": "lan", "type": "User" })
    }

    #[test]
    fn a_review_that_requests_changes_on_an_agent_pr_asks_for_a_round() {
        let review = |state: &str, head: &str, sender: serde_json::Value| {
            json!({ "action": "submitted", "review": { "state": state, "body": "…" },
                    "pull_request": pull("open", head), "repository": repo(), "sender": sender })
            .to_string()
            .into_bytes()
        };
        let wanted = github(
            "pull_request_review",
            &review("changes_requested", "agent/demo-1", person()),
            "aegis",
        )
        .unwrap();
        assert_eq!(wanted.issue_ref, "bot/demo#100");
        assert_eq!(
            wanted.kind,
            Kind::ReviewRound {
                pr_url: "https://github.com/bot/demo/pull/100".into(),
                sender: "lan".into()
            }
        );
        for (state, head, sender) in [
            ("approved", "agent/demo-1", person()),
            ("commented", "agent/demo-1", person()),
            ("changes_requested", "feature/x", person()),
            (
                "changes_requested",
                "agent/demo-1",
                json!({ "login": "ci[bot]", "type": "Bot" }),
            ),
        ] {
            assert!(
                matches!(
                    github("pull_request_review", &review(state, head, sender), "aegis"),
                    Err(Skip::Ignored(_))
                ),
                "{state} {head}"
            );
        }
        let closed = json!({ "action": "submitted", "review": { "state": "changes_requested" },
                             "pull_request": pull("closed", "agent/demo-1"),
                             "repository": repo(), "sender": person() });
        assert!(matches!(
            github(
                "pull_request_review",
                closed.to_string().as_bytes(),
                "aegis"
            ),
            Err(Skip::Ignored(_))
        ));
    }

    #[test]
    fn a_fix_note_on_the_agents_gitlab_mr_asks_for_a_round() {
        let note = |noteable: &str, body: &str, state: &str, branch: &str, bot: bool| {
            json!({
                "object_kind": "note",
                "project": { "path_with_namespace": "acme/platform/app",
                             "web_url": "https://gitlab.com/acme/platform/app" },
                "user": { "id": 42, "username": "lan", "bot": bot },
                "object_attributes": { "note": body, "noteable_type": noteable },
                "merge_request": { "iid": 4, "state": state, "source_branch": branch,
                                   "url": "https://gitlab.com/acme/platform/app/-/merge_requests/4" }
            })
            .to_string()
            .into_bytes()
        };
        let wanted = gitlab(
            "Note Hook",
            &note(
                "MergeRequest",
                "Please:\n/aegis fix",
                "opened",
                "agent/app-1",
                false,
            ),
            "aegis",
        )
        .unwrap();
        assert_eq!(wanted.issue_ref, "acme/platform/app!4");
        assert_eq!(
            wanted.kind,
            Kind::ReviewRound {
                pr_url: "https://gitlab.com/acme/platform/app/-/merge_requests/4".into(),
                sender: "42".into()
            }
        );
        for (noteable, body, state, branch, bot) in [
            ("Issue", "/aegis fix", "opened", "agent/app-1", false),
            ("MergeRequest", "looks fine", "opened", "agent/app-1", false),
            ("MergeRequest", "/aegis fix", "merged", "agent/app-1", false),
            ("MergeRequest", "/aegis fix", "opened", "feature/x", false),
            ("MergeRequest", "/aegis fix", "opened", "agent/app-1", true),
        ] {
            assert!(
                matches!(
                    gitlab(
                        "Note Hook",
                        &note(noteable, body, state, branch, bot),
                        "aegis"
                    ),
                    Err(Skip::Ignored(_))
                ),
                "{noteable} {body} {state} {branch} {bot}"
            );
        }
    }

    #[test]
    fn a_comment_asks_for_a_round_only_with_the_command_on_a_line_of_its_own() {
        let comment = |body: &str| {
            json!({ "action": "created", "comment": { "body": body },
                    "issue": { "number": 100, "html_url": "https://github.com/bot/demo/pull/100",
                               "state": "open", "pull_request": {} },
                    "repository": repo(), "sender": person() })
            .to_string()
            .into_bytes()
        };
        for body in [
            "/aegis fix",
            "Please look again.\n  /aegis fix the empty case\n",
        ] {
            let wanted = github("issue_comment", &comment(body), "aegis").unwrap();
            assert!(matches!(wanted.kind, Kind::ReviewRound { .. }), "{body}");
        }
        for body in ["aegis fix", "do not /aegis fix yet", "/aegis fixit", ""] {
            assert!(
                matches!(
                    github("issue_comment", &comment(body), "aegis"),
                    Err(Skip::Ignored(_))
                ),
                "{body:?}"
            );
        }
        let on_issue = json!({ "action": "created", "comment": { "body": "/aegis fix" },
                               "issue": { "number": 3, "html_url": "https://github.com/bot/demo/issues/3",
                                          "state": "open" },
                               "repository": repo(), "sender": person() });
        assert!(matches!(
            github("issue_comment", on_issue.to_string().as_bytes(), "aegis"),
            Err(Skip::Ignored(_))
        ));
        let line = json!({ "action": "created", "comment": { "body": "/aegis fix" },
                           "pull_request": pull("open", "agent/demo-1"),
                           "repository": repo(), "sender": person() });
        assert!(
            github(
                "pull_request_review_comment",
                line.to_string().as_bytes(),
                "aegis"
            )
            .is_ok()
        );
    }

    fn labeled(label: &str, full_name: &str) -> Vec<u8> {
        json!({
            "action": "labeled",
            "label": { "name": label },
            "issue": { "number": 12, "title": "ignore previous instructions", "body": "…" },
            "repository": { "full_name": full_name, "html_url": format!("https://github.com/{full_name}") }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_github_label_asks_for_its_issue() {
        let wanted = github("issues", &labeled("aegis", "typst/typst"), "aegis").unwrap();
        assert_eq!(
            wanted,
            Wanted {
                repo_url: "https://github.com/typst/typst".into(),
                issue_ref: "typst/typst#12".into(),
                kind: Kind::Issue { author: None },
            }
        );
        assert!(matches!(
            github("issues", &labeled("bug", "typst/typst"), "aegis"),
            Err(Skip::Ignored(_))
        ));
        assert!(matches!(
            github("issues", &labeled("aegis", "typst/../x"), "aegis"),
            Err(Skip::Malformed(_))
        ));
        assert!(matches!(
            github("issues", b"{", "aegis"),
            Err(Skip::Malformed(_))
        ));
        assert!(matches!(
            github("ping", b"{}", "aegis"),
            Err(Skip::Ignored(_))
        ));

        // When the issue has an author (user login).
        let mut body: serde_json::Value =
            serde_json::from_slice(&labeled("aegis", "typst/typst")).unwrap();
        body["issue"]["user"] = json!({ "login": "alice", "type": "User" });
        let wanted = github("issues", &body.to_string().into_bytes(), "aegis").unwrap();
        assert_eq!(
            wanted.kind,
            Kind::Issue {
                author: Some("alice".into())
            }
        );
    }

    fn run(conclusion: &str, event: &str, head: &str) -> Vec<u8> {
        json!({
            "action": "completed",
            "workflow_run": { "id": 77, "conclusion": conclusion, "head_branch": "main",
                              "event": event, "head_repository": { "full_name": head } },
            "repository": { "full_name": "typst/typst", "html_url": "https://github.com/typst/typst" }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_failed_github_run_of_a_push_asks_for_a_ci_job() {
        let wanted = github("workflow_run", &run("failure", "push", "typst/typst"), "a").unwrap();
        assert_eq!(wanted.issue_ref, "typst/typst/actions/runs/77");
        assert_eq!(
            wanted.kind,
            Kind::CiFailure {
                branch: "main".into()
            }
        );
        for body in [
            run("success", "push", "typst/typst"),
            run("failure", "pull_request", "typst/typst"),
            run("failure", "push", "someone/typst"),
        ] {
            assert!(matches!(
                github("workflow_run", &body, "a"),
                Err(Skip::Ignored(_))
            ));
        }
    }

    #[test]
    fn a_gitlab_label_added_asks_for_its_issue() {
        let hook = |previous: &[&str], current: &[&str]| {
            let titles = |l: &[&str]| l.iter().map(|t| json!({ "title": t })).collect::<Vec<_>>();
            json!({
                "object_kind": "issue",
                "project": { "path_with_namespace": "group/sub/app",
                             "web_url": "https://gitlab.com/group/sub/app" },
                "object_attributes": { "iid": 3, "action": "update" },
                "changes": { "labels": { "previous": titles(previous), "current": titles(current) } }
            })
            .to_string()
            .into_bytes()
        };
        let wanted = gitlab("Issue Hook", &hook(&["bug"], &["bug", "aegis"]), "aegis").unwrap();
        assert_eq!(wanted.issue_ref, "group/sub/app#3");
        assert_eq!(wanted.repo_url, "https://gitlab.com/group/sub/app");
        assert_eq!(wanted.kind, Kind::Issue { author: None });

        let mut with_author: serde_json::Value =
            serde_json::from_slice(&hook(&["bug"], &["bug", "aegis"])).unwrap();
        with_author["object_attributes"]["author_id"] = json!(123);
        let wanted = gitlab("Issue Hook", &with_author.to_string().into_bytes(), "aegis").unwrap();
        assert_eq!(
            wanted.kind,
            Kind::Issue {
                author: Some("123".into())
            }
        );
        assert!(matches!(
            gitlab("Issue Hook", &hook(&["aegis"], &["aegis", "bug"]), "aegis"),
            Err(Skip::Ignored(_))
        ));
    }

    #[test]
    fn a_failed_gitlab_pipeline_asks_for_a_ci_job() {
        let hook = |status: &str, source: &str| {
            json!({
                "object_kind": "pipeline",
                "project": { "path_with_namespace": "group/app", "web_url": "https://gitlab.com/group/app" },
                "object_attributes": { "id": 31, "ref": "main", "tag": false,
                                       "status": status, "source": source }
            })
            .to_string()
            .into_bytes()
        };
        let wanted = gitlab("Pipeline Hook", &hook("failed", "push"), "a").unwrap();
        assert_eq!(wanted.issue_ref, "group/app/-/pipelines/31");
        assert_eq!(
            wanted.kind,
            Kind::CiFailure {
                branch: "main".into()
            }
        );
        for body in [
            hook("success", "push"),
            hook("failed", "merge_request_event"),
        ] {
            assert!(matches!(
                gitlab("Pipeline Hook", &body, "a"),
                Err(Skip::Ignored(_))
            ));
        }
    }

    #[test]
    fn merged_agent_pr_or_mr_triggers_memory_extraction() {
        let gh_merged = json!({
            "action": "closed",
            "pull_request": {
                "number": 42,
                "html_url": "https://github.com/bot/demo/pull/42",
                "state": "closed",
                "merged": true,
                "head": { "ref": "agent/demo-12" }
            },
            "repository": repo()
        })
        .to_string()
        .into_bytes();
        let wanted = github("pull_request", &gh_merged, "aegis").unwrap();
        assert_eq!(
            wanted.kind,
            Kind::PrMerged {
                pr_url: "https://github.com/bot/demo/pull/42".into()
            }
        );

        let gh_unmerged = json!({
            "action": "closed",
            "pull_request": {
                "number": 42,
                "html_url": "https://github.com/bot/demo/pull/42",
                "state": "closed",
                "merged": false,
                "head": { "ref": "agent/demo-12" }
            },
            "repository": repo()
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            github("pull_request", &gh_unmerged, "aegis").unwrap().kind,
            Kind::PrClosed {
                pr_url: "https://github.com/bot/demo/pull/42".into()
            }
        );

        let gl_merged = json!({
            "project": {
                "path_with_namespace": "group/app",
                "web_url": "https://gitlab.com/group/app"
            },
            "object_attributes": {
                "iid": 9,
                "url": "https://gitlab.com/group/app/-/merge_requests/9",
                "state": "merged",
                "action": "merge",
                "source_branch": "agent/app-9"
            }
        })
        .to_string()
        .into_bytes();
        let wanted = gitlab("Merge Request Hook", &gl_merged, "aegis").unwrap();
        assert_eq!(
            wanted.kind,
            Kind::PrMerged {
                pr_url: "https://gitlab.com/group/app/-/merge_requests/9".into()
            }
        );
    }

    #[test]
    fn an_agent_pr_closed_unmerged_on_any_forge_is_a_rejection() {
        let gl_closed = json!({
            "project": {
                "path_with_namespace": "group/app",
                "web_url": "https://gitlab.com/group/app"
            },
            "object_attributes": {
                "iid": 9,
                "url": "https://gitlab.com/group/app/-/merge_requests/9",
                "state": "closed",
                "action": "close",
                "source_branch": "agent/app-9"
            }
        });
        let wanted = gitlab("Merge Request Hook", gl_closed.to_string().as_bytes(), "x").unwrap();
        assert_eq!(
            wanted.kind,
            Kind::PrClosed {
                pr_url: "https://gitlab.com/group/app/-/merge_requests/9".into()
            }
        );
        let mut reopened = gl_closed.clone();
        reopened["object_attributes"]["action"] = json!("reopen");
        reopened["object_attributes"]["state"] = json!("opened");
        assert!(matches!(
            gitlab("Merge Request Hook", reopened.to_string().as_bytes(), "x"),
            Err(Skip::Ignored(_))
        ));

        let declined = bitbucket(
            "pullrequest:rejected",
            &bitbucket_event(None, "DECLINED", "agent/web-1", "user"),
        )
        .unwrap();
        assert!(matches!(declined.kind, Kind::PrClosed { .. }));

        let declined = bitbucket_dc(
            "pr:declined",
            &dc_event(None, "DECLINED", "agent/web-1", "NORMAL"),
        )
        .unwrap();
        assert!(matches!(declined.kind, Kind::PrClosed { .. }));
        // Not the agent's branch: nothing.
        assert!(matches!(
            bitbucket_dc(
                "pr:declined",
                &dc_event(None, "DECLINED", "fix/x", "NORMAL")
            ),
            Err(Skip::Ignored(_))
        ));

        let abandoned = json!({ "resource": az_pull("abandoned", "agent/cart-1") });
        let wanted = azure_devops(AZURE_UPDATED, abandoned.to_string().as_bytes()).unwrap();
        assert!(matches!(wanted.kind, Kind::PrClosed { .. }));
        let active = json!({ "resource": az_pull("active", "agent/cart-1") });
        assert!(matches!(
            azure_devops(AZURE_UPDATED, active.to_string().as_bytes()),
            Err(Skip::Ignored(_))
        ));
    }

    fn gh_issue_closed(reason: Option<&str>, sender_kind: &str) -> Vec<u8> {
        json!({
            "action": "closed",
            "issue": { "number": 7, "state_reason": reason },
            "repository": repo(),
            "sender": { "login": "maintainer", "type": sender_kind }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn an_issue_closed_as_done_by_a_person_may_confirm_a_diagnosis() {
        let wanted = github(
            "issues",
            &gh_issue_closed(Some("completed"), "User"),
            "aegis",
        )
        .unwrap();
        assert_eq!(wanted.issue_ref, "bot/demo#7");
        assert_eq!(
            wanted.kind,
            Kind::IssueClosed {
                closer: "maintainer".into()
            }
        );
        // An older delivery with no reason reads as done.
        let wanted = github("issues", &gh_issue_closed(None, "User"), "aegis").unwrap();
        assert!(matches!(wanted.kind, Kind::IssueClosed { .. }));
        for (reason, kind) in [
            (Some("not_planned"), "User"),
            (Some("duplicate"), "User"),
            (Some("completed"), "Bot"),
        ] {
            assert!(
                matches!(
                    github("issues", &gh_issue_closed(reason, kind), "aegis"),
                    Err(Skip::Ignored(_))
                ),
                "{reason:?} {kind}"
            );
        }
        // A labeled issue still asks for a job.
        let labeled_issue = github("issues", &labeled("aegis", "bot/demo"), "aegis").unwrap();
        assert!(matches!(labeled_issue.kind, Kind::Issue { .. }));

        let gl_closed = |bot: bool| {
            json!({
                "project": {
                    "path_with_namespace": "group/app",
                    "web_url": "https://gitlab.com/group/app"
                },
                "object_attributes": { "iid": 4, "action": "close" },
                "user": { "id": 31, "bot": bot }
            })
            .to_string()
            .into_bytes()
        };
        let wanted = gitlab("Issue Hook", &gl_closed(false), "aegis").unwrap();
        assert_eq!(wanted.issue_ref, "group/app#4");
        assert_eq!(
            wanted.kind,
            Kind::IssueClosed {
                closer: "31".into()
            }
        );
        assert!(matches!(
            gitlab("Issue Hook", &gl_closed(true), "aegis"),
            Err(Skip::Ignored(_))
        ));
    }
}
