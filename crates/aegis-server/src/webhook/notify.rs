//! Tells the issue how its job ended (#25): a comment by the platform's
//! account on the issue of every `github` job ([`GitHubNotifier`]) and
//! every `gitlab` job ([`GitLabNotifier`], #168), with the PR or the reason
//! it stopped, or that its repo's monthly budget is spent (#31). Jobs of
//! other sources are not reported.

use aegis_ledger::Outcome;
use std::sync::Arc;

use aegis_pr::{GitHub, GitLab, TokenSource, TokenUse, inert};
use aegis_secrets::Secrets;
use aegis_store::{BUDGET_SPENT, Job, JobSource, JobStatus, Store};

use crate::github_app::AppHandle;

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

pub(crate) const DEFAULT_GITHUB_API: &str = "https://api.github.com";
pub(crate) const DEFAULT_GITLAB_API: &str = "https://gitlab.com/api/v4";

#[derive(Debug, Clone)]
pub struct GitHubNotifier {
    api: String,
    /// Where [`COMMENT_TOKEN_ENV`] is read, at each comment.
    secrets: Secrets,
    /// The instance's GitHub App (ADR 0017 §4), for repos on the app path…
    app: Option<Arc<AppHandle>>,
    /// …told apart by their settings, read here for a job's end.
    store: Option<Store>,
}

/// Whether a repo with these settings is on the app path (ADR 0017).
pub fn on_app_path(config: &serde_json::Value) -> bool {
    config.get("app").is_some_and(|a| !a.is_null())
}

impl GitHubNotifier {
    pub fn new(api: &str, secrets: Secrets) -> Self {
        Self {
            api: api.to_owned(),
            secrets,
            app: None,
            store: None,
        }
    }

    /// With the instance's GitHub App: repos on the app path are
    /// commented on and asked about as the app, from their settings in
    /// `store`.
    pub fn with_app(self, app: Option<Arc<AppHandle>>, store: Store) -> Self {
        Self {
            app,
            store: Some(store),
            ..self
        }
    }

    /// A client of `owner/repo` for `use_`: the app's on the app path, the
    /// comment token's otherwise; `Ok(None)` when the token path has no
    /// comment token.
    async fn client(
        &self,
        owner: &str,
        repo: &str,
        on_app: bool,
        use_: TokenUse,
    ) -> Result<Option<GitHub>, String> {
        if on_app {
            let handle = self
                .app
                .as_ref()
                .ok_or("the repo is on the GitHub App path and the instance has no [github_app]")?;
            let source = TokenSource::App {
                app: handle.app().await?,
                owner: owner.to_owned(),
                repo: repo.to_owned(),
                use_,
            };
            return GitHub::with_source(handle.api(), owner, repo, source)
                .map(Some)
                .map_err(|e| e.to_string());
        }
        let Some(token) = self.secrets.get(COMMENT_TOKEN_ENV) else {
            return Ok(None);
        };
        GitHub::new(&self.api, owner, repo, token.expose())
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// Whether the repo of `job` is on the app path.
    async fn job_on_app(&self, job: &Job) -> bool {
        let Some(store) = self.store.as_ref().filter(|_| self.app.is_some()) else {
            return false;
        };
        match store.repo(job.repo_id).await {
            Ok(Some(repo)) => on_app_path(&repo.config),
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(job = job.id.0, error = %e, "reading the job's repo");
                false
            }
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
    /// comment token, or as the app for a repo on the app path; `Err` when
    /// there is no token or GitHub fails.
    pub async fn can_write(
        &self,
        owner: &str,
        repo: &str,
        login: &str,
        on_app: bool,
    ) -> Result<bool, String> {
        let github = self
            .client(owner, repo, on_app, TokenUse::Comment)
            .await?
            .ok_or_else(|| format!("no secret {COMMENT_TOKEN_ENV} to check the sender's rights"))?;
        let permission = github.permission(login).await.map_err(|e| e.to_string())?;
        Ok(matches!(
            permission.as_str(),
            "admin" | "maintain" | "write"
        ))
    }

    /// Posts `text` on issue or PR `number` of `owner/repo`, if the store
    /// has the comment token (or as the app, on the app path); failures
    /// are logged.
    pub async fn comment(&self, owner: &str, repo: &str, number: u64, text: &str, on_app: bool) {
        let posted = match self.client(owner, repo, on_app, TokenUse::Comment).await {
            Ok(Some(github)) => github
                .comment_on_issue(number, text)
                .await
                .map(drop)
                .map_err(|e| e.to_string()),
            Ok(None) => return,
            Err(e) => Err(e),
        };
        if let Err(e) = posted {
            tracing::warn!(error = %e, "commenting on the pull request");
        }
    }
}

impl JobNotifier for GitHubNotifier {
    async fn job_ended(&self, job: &Job, diagnosis: Option<&str>) {
        if job.source != JobSource::Github {
            return;
        }
        let Ok((owner, repo, number)) = parse_issue_ref(&job.issue_ref) else {
            tracing::warn!(job = job.id.0, "not an issue to comment on");
            return;
        };
        let on_app = self.job_on_app(job).await;
        let posted = match self.client(&owner, &repo, on_app, TokenUse::Comment).await {
            Ok(Some(github)) => github
                .comment_on_issue(number, &comment_with(job, diagnosis))
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Ok(None) => return,
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
    async fn job_ended(&self, job: &Job, diagnosis: Option<&str>) {
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
                .comment_on_issue(iid, &comment_with(job, diagnosis))
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

/// The comment on the issue: the job's status, and for a job the
/// diagnosis lane took (#451) its report, which the lifecycle built from
/// checked sources and made inert item by item.
pub fn comment_with(job: &Job, diagnosis: Option<&str>) -> String {
    match diagnosis.filter(|_| job.outcome == Some(Outcome::Diagnosed)) {
        Some(report) => {
            let status = status_comment(job);
            let (head, foot) = status.rsplit_once("\n\n").unwrap_or((&status, ""));
            format!("{head}\n\n{report}\n\n{foot}")
        }
        None => status_comment(job),
    }
}

/// [`comment_with`] in plain text, for the trackers.
pub fn plain_comment_with(job: &Job, diagnosis: Option<&str>) -> String {
    match diagnosis.filter(|_| job.outcome == Some(Outcome::Diagnosed)) {
        Some(report) => {
            let status = plain_status_comment(job);
            let (head, foot) = status.rsplit_once("\n\n").unwrap_or((&status, ""));
            format!("{head}\n\n{report}\n\n{foot}")
        }
        None => plain_status_comment(job),
    }
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
        (JobStatus::Done, Some(Outcome::Diagnosed)) => (
            format!(
                "looked into this issue: it does not read as a code defect, so no {request} \
                 was opened. What it found is below; a person decides what to do."
            ),
            None,
        ),
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
        Outcome::Diagnosed => "it was diagnosed as needing no code change",
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
            priority: None,
            sla_deadline: None,
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
    fn a_diagnosed_job_writes_its_report_back_with_no_request() {
        let done = job(JobStatus::Done, Some(Outcome::Diagnosed));
        let report = "**Diagnosis**: ops_or_config (confidence 0.90). Read-only.";
        let text = comment_with(&done, Some(report));
        assert!(
            text.starts_with("**aegis-code** looked into this issue: it does not read as a code defect, so no pull request was opened."),
            "{text}"
        );
        assert!(
            text.contains(&format!("\n\n{report}\n\n<sub>aegis-code job 7</sub>")),
            "{text}"
        );
        let plain = plain_comment_with(&done, Some(report));
        assert!(
            plain.contains(report) && plain.ends_with("(aegis-code job 7)"),
            "{plain}"
        );
        // A report goes only with a diagnosed job.
        let stopped = job(JobStatus::Done, Some(Outcome::StoppedSteps));
        assert!(!comment_with(&stopped, Some(report)).contains("Diagnosis"));
        assert_eq!(comment_with(&done, None), status_comment(&done));
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
