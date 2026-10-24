//! Tells the issue how its job ended (#25): a comment by the platform's
//! GitHub account on the issue of every `github` job, with the PR or the
//! reason it stopped. Jobs of other sources are not reported yet.

use aegis_ledger::Outcome;
use aegis_pr::{GitHub, inert};
use aegis_secrets::Secrets;
use aegis_store::{Job, JobSource, JobStatus};

use crate::lifecycle::parse_issue_ref;
use crate::worker::JobNotifier;

/// The GitHub API root, `https://api.github.com` when unset.
pub const GITHUB_API_ENV: &str = "AEGIS_GITHUB_API";
/// The secret: a token that may comment on the issues of the registered
/// repos; no comments while the store has none.
pub const COMMENT_TOKEN_ENV: &str = "AEGIS_GITHUB_COMMENT_TOKEN";

const DEFAULT_GITHUB_API: &str = "https://api.github.com";

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

    /// The token from `secrets`, read now; the API root from
    /// [`GITHUB_API_ENV`], not a secret.
    pub async fn from_secrets(secrets: &Secrets) -> Result<Self, String> {
        let api = std::env::var(GITHUB_API_ENV).unwrap_or_else(|_| DEFAULT_GITHUB_API.into());
        let token = secrets
            .load(COMMENT_TOKEN_ENV)
            .await
            .map_err(|e| e.to_string())?;
        if token.is_none() {
            tracing::info!("no secret {COMMENT_TOKEN_ENV}: jobs' ends are not commented on");
        }
        Ok(Self::new(&api, secrets.clone()))
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

/// What the comment says. The stop reason may hold model text: it is made
/// inert. Infrastructure failures are not detailed in public.
pub fn status_comment(job: &Job) -> String {
    let (what, reason) = match (job.status, job.outcome) {
        (JobStatus::Done, Some(Outcome::PrOpened)) => {
            let url = job
                .pr_url
                .as_deref()
                .filter(|u| u.starts_with("https://") && !u.contains(char::is_whitespace));
            let what = match url {
                Some(url) => format!("opened a pull request for this issue: {url}"),
                None => "opened a pull request for this issue.".into(),
            };
            (what, None)
        }
        (JobStatus::Done, Some(outcome)) => (
            format!("stopped without a pull request: {}.", stop(outcome)),
            job.reason.as_deref(),
        ),
        (JobStatus::Cancelled, _) => ("cancelled its job for this issue.".into(), None),
        _ => (
            "could not finish this issue: the platform failed. Its operators can see why.".into(),
            None,
        ),
    };
    let mut text = format!("**aegis-code** {what}");
    if let Some(reason) = reason.map(inert).filter(|r| !r.is_empty()) {
        text.push_str(&format!("\n\nReason: {reason}"));
    }
    text.push_str(&format!("\n\n<sub>aegis-code job {}</sub>", job.id.0));
    text
}

fn stop(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::PrOpened => "a pull request was opened",
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
        }
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

        let failed = Job {
            reason: Some("database: connection refused at 10.0.0.3".into()),
            ..job(JobStatus::Failed, None)
        };
        assert!(!status_comment(&failed).contains("10.0.0.3"));
    }
}
