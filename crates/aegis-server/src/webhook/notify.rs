//! Tells the issue how its job ended (#25): a comment by the platform's
//! account on the issue of every `github` job ([`GitHubNotifier`]) and
//! every `gitlab` job ([`GitLabNotifier`], #168), with the PR or the reason
//! it stopped, or that its repo's monthly budget is spent (#31). Jobs of
//! other sources are not reported.

use aegis_ledger::Outcome;
use aegis_pr::{GitHub, GitLab, inert};
use aegis_secrets::Secrets;
use aegis_store::{BUDGET_SPENT, Job, JobSource, JobStatus};

use crate::lifecycle::parse_issue_ref;
use crate::worker::JobNotifier;

/// The GitHub API root, `https://api.github.com` when unset.
pub const GITHUB_API_ENV: &str = "AEGIS_GITHUB_API";
/// The secret: a token that may comment on the issues of the registered
/// repos; no comments while the store has none.
pub const COMMENT_TOKEN_ENV: &str = "AEGIS_GITHUB_COMMENT_TOKEN";

/// The GitLab API root, `https://gitlab.com/api/v4` when unset; a
/// self-hosted instance's otherwise.
pub const GITLAB_API_ENV: &str = "AEGIS_GITLAB_API";
/// The secret: a token that may comment on the issues of the registered
/// GitLab projects; no comments while the store has none.
pub const GITLAB_COMMENT_TOKEN_ENV: &str = "AEGIS_GITLAB_COMMENT_TOKEN";

const DEFAULT_GITHUB_API: &str = "https://api.github.com";
const DEFAULT_GITLAB_API: &str = "https://gitlab.com/api/v4";

#[derive(Debug, Clone)]
pub struct GitHubNotifier {
    api: String,
    /// Where [`COMMENT_TOKEN_ENV`] is read, at each comment.
    secrets: Secrets,
}

impl GitHubNotifier {
    pub fn new(api: &str, secrets: Secrets) -> Self {
        Self {
            api: api.to_owned(),
            secrets,
        }
    }

    /// The API root from [`GITHUB_API_ENV`]; the token is read from
    /// `secrets` at each comment.
    pub fn from_env(secrets: Secrets) -> Self {
        let api = std::env::var(GITHUB_API_ENV).unwrap_or_else(|_| DEFAULT_GITHUB_API.into());
        Self::new(&api, secrets)
    }

    /// The token from `secrets`, read now; the API root from
    /// [`GITHUB_API_ENV`], not a secret.
    pub async fn from_secrets(secrets: &Secrets) -> Result<Self, String> {
        let token = secrets
            .load(COMMENT_TOKEN_ENV)
            .await
            .map_err(|e| e.to_string())?;
        if token.is_none() {
            tracing::info!("no secret {COMMENT_TOKEN_ENV}: jobs' ends are not commented on");
        }
        Ok(Self::from_env(secrets.clone()))
    }
}

impl GitHubNotifier {
    /// Whether `login` may write to `owner/repo` (#244), asked with the
    /// comment token; `Err` when there is no token or GitHub fails.
    pub async fn can_write(&self, owner: &str, repo: &str, login: &str) -> Result<bool, String> {
        let token = self
            .secrets
            .get(COMMENT_TOKEN_ENV)
            .ok_or_else(|| format!("no secret {COMMENT_TOKEN_ENV} to check the sender's rights"))?;
        let github =
            GitHub::new(&self.api, owner, repo, token.expose()).map_err(|e| e.to_string())?;
        let permission = github.permission(login).await.map_err(|e| e.to_string())?;
        Ok(matches!(
            permission.as_str(),
            "admin" | "maintain" | "write"
        ))
    }

    /// Posts `text` on issue or PR `number` of `owner/repo`, if the store
    /// has the comment token; failures are logged.
    pub async fn comment(&self, owner: &str, repo: &str, number: u64, text: &str) {
        let Some(token) = self.secrets.get(COMMENT_TOKEN_ENV) else {
            return;
        };
        let posted = match GitHub::new(&self.api, owner, repo, token.expose()) {
            Ok(github) => github.comment_on_issue(number, text).await.map(drop),
            Err(e) => Err(e),
        };
        if let Err(e) = posted {
            tracing::warn!(error = %e, "commenting on the pull request");
        }
    }
}

impl JobNotifier for GitHubNotifier {
    async fn job_ended(&self, job: &Job) {
        if job.source != JobSource::Github {
            return;
        }
        let Some(token) = self.secrets.get(COMMENT_TOKEN_ENV) else {
            return;
        };
        let Ok((owner, repo, number)) = parse_issue_ref(&job.issue_ref) else {
            tracing::warn!(job = job.id.0, "not an issue to comment on");
            return;
        };
        let posted = match GitHub::new(&self.api, &owner, &repo, token.expose()) {
            Ok(github) => github
                .comment_on_issue(number, &status_comment(job))
                .await
                .map(|_| ()),
            Err(e) => Err(e),
        };
        match posted {
            Ok(()) => tracing::info!(job = job.id.0, "commented on the issue"),
            Err(e) => tracing::warn!(job = job.id.0, error = %e, "commenting on the issue"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitLabNotifier {
    api: String,
    /// Where [`GITLAB_COMMENT_TOKEN_ENV`] is read, at each comment.
    secrets: Secrets,
}

impl GitLabNotifier {
    pub fn new(api: &str, secrets: Secrets) -> Self {
        Self {
            api: api.to_owned(),
            secrets,
        }
    }

    /// The API root from [`GITLAB_API_ENV`]; the token is read from
    /// `secrets` at each comment.
    pub fn from_env(secrets: Secrets) -> Self {
        let api = std::env::var(GITLAB_API_ENV).unwrap_or_else(|_| DEFAULT_GITLAB_API.into());
        Self::new(&api, secrets)
    }

    /// The token from `secrets`, read now; the API root from
    /// [`GITLAB_API_ENV`], not a secret.
    pub async fn from_secrets(secrets: &Secrets) -> Result<Self, String> {
        let token = secrets
            .load(GITLAB_COMMENT_TOKEN_ENV)
            .await
            .map_err(|e| e.to_string())?;
        if token.is_none() {
            tracing::info!(
                "no secret {GITLAB_COMMENT_TOKEN_ENV}: GitLab jobs' ends are not commented on"
            );
        }
        Ok(Self::from_env(secrets.clone()))
    }
}

impl GitLabNotifier {
    /// Whether user `user_id` is a Developer or above in GitLab project
    /// `project` (#254), asked with the comment token; `Err` when there is
    /// no token or GitLab fails.
    pub async fn can_develop(&self, project: &str, user_id: u64) -> Result<bool, String> {
        let token = self.secrets.get(GITLAB_COMMENT_TOKEN_ENV).ok_or_else(|| {
            format!("no secret {GITLAB_COMMENT_TOKEN_ENV} to check the sender's rights")
        })?;
        let gitlab = GitLab::new(&self.api, project, token.expose()).map_err(|e| e.to_string())?;
        let level = gitlab
            .member_access(user_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(level >= aegis_pr::GITLAB_DEVELOPER)
    }

    /// Posts `text` on MR `iid` of `project`, if the store has the comment
    /// token; failures are logged.
    pub async fn comment_on_merge_request(&self, project: &str, iid: u64, text: &str) {
        let Some(token) = self.secrets.get(GITLAB_COMMENT_TOKEN_ENV) else {
            return;
        };
        let posted = match GitLab::new(&self.api, project, token.expose()) {
            Ok(gitlab) => gitlab.comment_on_merge_request(iid, text).await,
            Err(e) => Err(e),
        };
        if let Err(e) = posted {
            tracing::warn!(error = %e, "commenting on the merge request");
        }
    }
}

impl JobNotifier for GitLabNotifier {
    async fn job_ended(&self, job: &Job) {
        if job.source != JobSource::Gitlab {
            return;
        }
        let Some(token) = self.secrets.get(GITLAB_COMMENT_TOKEN_ENV) else {
            return;
        };
        let Some((project, iid)) = gitlab_issue_ref(&job.issue_ref) else {
            tracing::warn!(job = job.id.0, "not an issue to comment on");
            return;
        };
        let posted = match GitLab::new(&self.api, project, token.expose()) {
            Ok(gitlab) => gitlab
                .comment_on_issue(iid, &status_comment(job))
                .await
                .map(|_| ()),
            Err(e) => Err(e),
        };
        match posted {
            Ok(()) => tracing::info!(job = job.id.0, "commented on the issue"),
            Err(e) => tracing::warn!(job = job.id.0, error = %e, "commenting on the issue"),
        }
    }
}

/// `group/sub/project#iid`, as [`super::events::gitlab`] builds it; the
/// path is checked by [`GitLab::new`].
fn gitlab_issue_ref(text: &str) -> Option<(&str, u64)> {
    let (project, iid) = text.rsplit_once('#')?;
    Some((project, iid.parse().ok()?))
}

/// What the comment says, in Markdown. The stop reason may hold model
/// text: it is made inert. Infrastructure failures are not detailed in
/// public.
pub fn status_comment(job: &Job) -> String {
    let (what, reason) = status_parts(job);
    let mut text = format!("**aegis-code** {what}");
    if let Some(reason) = reason {
        text.push_str(&format!("\n\nReason: {reason}"));
    }
    text.push_str(&format!("\n\n<sub>aegis-code job {}</sub>", job.id.0));
    text
}

/// [`status_comment`] in plain text, for Jira and Backlog (#26), whose
/// markup is not Markdown.
pub fn plain_status_comment(job: &Job) -> String {
    let (what, reason) = status_parts(job);
    let mut text = format!("aegis-code {what}");
    if let Some(reason) = reason {
        text.push_str(&format!("\n\nReason: {reason}"));
    }
    text.push_str(&format!("\n\n(aegis-code job {})", job.id.0));
    text
}

/// What happened, and the reason made inert.
fn status_parts(job: &Job) -> (String, Option<String>) {
    // GitLab's issues get a merge request (#252).
    let request = if job.source == JobSource::Gitlab {
        "merge request"
    } else {
        "pull request"
    };
    let (what, reason) = match (job.status, job.outcome) {
        (JobStatus::Done, Some(Outcome::PrOpened)) => {
            let what = match pr_url(job) {
                Some(url) => format!("opened a {request} for this issue: {url}"),
                None => format!("opened a {request} for this issue."),
            };
            (what, None)
        }
        (JobStatus::Done, Some(outcome)) => (
            format!("stopped without a {request}: {}.", stop(outcome)),
            job.reason.as_deref(),
        ),
        (JobStatus::Cancelled, _) => match job.reason.as_deref() {
            Some(r) if r.starts_with(BUDGET_SPENT) => (
                "did not take this issue: the repository's monthly budget is spent.".into(),
                Some(r),
            ),
            _ => ("cancelled its job for this issue.".into(), None),
        },
        _ => (
            "could not finish this issue: the platform failed. Its operators can see why.".into(),
            None,
        ),
    };
    (what, reason.map(inert).filter(|r| !r.is_empty()))
}

/// The PR of a job that opened one, if its URL is a plain `https` one.
pub(crate) fn pr_url(job: &Job) -> Option<&str> {
    if (job.status, job.outcome) != (JobStatus::Done, Some(Outcome::PrOpened)) {
        return None;
    }
    job.pr_url
        .as_deref()
        .filter(|u| u.starts_with("https://") && !u.contains(char::is_whitespace))
}

fn stop(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::PrOpened => "a pull or merge request was opened",
        Outcome::StoppedBudget => "its budget is spent",
        Outcome::StoppedSteps => "it reached its step limit",
        Outcome::StoppedExpertFail => "even the strongest model could not fix it",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_ledger::IssueId;
    use aegis_store::{JobId, RepoId};
    use chrono::Utc;

    fn job(status: JobStatus, outcome: Option<Outcome>) -> Job {
        Job {
            id: JobId(7),
            repo_id: RepoId(1),
            issue_ref: "typst/typst#12".into(),
            ledger_key: IssueId::parse("typst-12").unwrap(),
            source: JobSource::Github,
            status,
            outcome,
            attempts: 1,
            locked_by: None,
            locked_until: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            run_after: Utc::now(),
            retries: 0,
            cancel_requested: false,
            reason: None,
            pr_url: None,
            retry_of: None,
            review_of: None,
            review_round: 0,
        }
    }

    #[test]
    fn gitlab_issue_refs_keep_their_subgroups() {
        assert_eq!(
            gitlab_issue_ref("group/sub/app#4"),
            Some(("group/sub/app", 4))
        );
        for bad in ["group/app", "group/app#x", "group/app#"] {
            assert_eq!(gitlab_issue_ref(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_gitlab_issue_is_told_of_its_merge_request() {
        let mr = Job {
            source: JobSource::Gitlab,
            pr_url: Some("https://gitlab.com/acme/app/-/merge_requests/4".into()),
            ..job(JobStatus::Done, Some(Outcome::PrOpened))
        };
        let text = status_comment(&mr);
        assert!(
            text.contains("opened a merge request for this issue: https://gitlab.com/acme/app/-/merge_requests/4"),
            "{text}"
        );
        let stopped = Job {
            source: JobSource::Gitlab,
            ..job(JobStatus::Done, Some(Outcome::StoppedSteps))
        };
        assert!(status_comment(&stopped).contains("stopped without a merge request"));
    }

    #[test]
    fn the_comment_says_how_the_job_ended() {
        let pr = Job {
            pr_url: Some("https://github.com/me/typst/pull/3".into()),
            ..job(JobStatus::Done, Some(Outcome::PrOpened))
        };
        let text = status_comment(&pr);
        assert!(
            text.contains("https://github.com/me/typst/pull/3"),
            "{text}"
        );
        assert!(text.contains("job 7"), "{text}");

        let stopped = Job {
            reason: Some("@everyone see `x`\nhttps://evil.example".into()),
            ..job(JobStatus::Done, Some(Outcome::StoppedSteps))
        };
        let text = status_comment(&stopped);
        assert!(text.contains("step limit"), "{text}");
        assert!(
            !text.contains("@everyone"),
            "mentions are made inert: {text}"
        );
        assert!(!text.contains("https://evil"), "{text}");

        let refused = Job {
            reason: Some(format!("{BUDGET_SPENT}: $97.00 of $100.00 used")),
            ..job(JobStatus::Cancelled, None)
        };
        let text = status_comment(&refused);
        assert!(text.contains("monthly budget"), "{text}");
        assert!(text.contains("$97.00 of $100.00"), "{text}");

        let failed = Job {
            reason: Some("database: connection refused at 10.0.0.3".into()),
            ..job(JobStatus::Failed, None)
        };
        assert!(!status_comment(&failed).contains("10.0.0.3"));
    }

    #[test]
    fn the_plain_comment_says_the_same_without_markup() {
        let pr = Job {
            pr_url: Some("https://github.com/me/app/pull/3".into()),
            ..job(JobStatus::Done, Some(Outcome::PrOpened))
        };
        let text = plain_status_comment(&pr);
        assert!(
            text.starts_with("aegis-code opened a pull request"),
            "{text}"
        );
        assert!(text.contains("https://github.com/me/app/pull/3"), "{text}");
        assert!(!text.contains("**") && !text.contains("<sub>"), "{text}");
        assert_eq!(pr_url(&pr), Some("https://github.com/me/app/pull/3"));

        let stopped = Job {
            pr_url: Some("https://github.com/me/app/pull/3".into()),
            reason: Some("[x|https://evil.example]".into()),
            ..job(JobStatus::Done, Some(Outcome::StoppedSteps))
        };
        assert_eq!(pr_url(&stopped), None, "only a job that opened it");
        assert!(!plain_status_comment(&stopped).contains("https://evil"));
    }
}
