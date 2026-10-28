//! Review rounds (#243): after a review of an agent's PR, a job goes on
//! with that PR, on its branch. Each round has a ledger, so a budget, of
//! its own (`<key>-v<n>`), and is linked to the job that opened the PR;
//! a PR gets at most [`Store::max_review_rounds`] rounds.

use aegis_ledger::{IssueId, Outcome};

use crate::audit::{self, Target};
use crate::budget;
use crate::jobs::JobRow;
use crate::types::decode_outcome;
use crate::{Actor, Job, JobId, JobSource, JobStatus, Store, StoreError};

/// Rounds a PR gets when the server sets no other number.
pub const DEFAULT_MAX_REVIEW_ROUNDS: u32 = 3;

/// `https://github.com/<owner>/<name>/pull/<n>` → `<owner>/<name>#<n>`:
/// the PR as an issue ref, so a PR has one live round at a time.
pub fn pr_issue_ref(pr_url: &str) -> Option<String> {
    let parts: Vec<&str> = pr_url.trim_end_matches('/').rsplitn(4, '/').collect();
    let [number, "pull", name, rest] = parts.as_slice() else {
        return None;
    };
    let owner = rest.rsplit('/').next()?;
    let plain = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    let number: u64 = number.parse().ok()?;
    (plain(owner) && plain(name)).then(|| format!("{owner}/{name}#{number}"))
}

impl Store {
    /// Rounds a PR may have (`[reviews] max_rounds` of the server).
    pub fn with_max_review_rounds(self, rounds: u32) -> Self {
        Self {
            max_review_rounds: rounds,
            ..self
        }
    }

    pub fn max_review_rounds(&self) -> u32 {
        self.max_review_rounds
    }

    /// The next review round of the PR that job `of` opened, or that the
    /// review round `of` works on. `Conflict` when that job opened no PR,
    /// when the PR had all its rounds, or when one is live; `BudgetSpent`
    /// when the repo's monthly budget is (#31).
    pub async fn review_job_by(&self, of: JobId, actor: &Actor) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        let asked = sqlx::query!("SELECT review_of FROM jobs WHERE id = $1 FOR UPDATE", of.0)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", of.0)))?;
        let root_id = asked.review_of.unwrap_or(of.0);
        let root = sqlx::query!(
            "SELECT repo_id, ledger_key, status, outcome, pr_url FROM jobs
             WHERE id = $1 FOR UPDATE",
            root_id,
        )
        .fetch_one(&mut *tx)
        .await?;
        let status = JobStatus::decode("jobs.status", &root.status)?;
        let outcome = root.outcome.as_deref().map(decode_outcome).transpose()?;
        let pr_url = match (status, outcome, root.pr_url) {
            (JobStatus::Done, Some(Outcome::PrOpened), Some(url)) => url,
            _ => {
                return Err(StoreError::Conflict(format!(
                    "job {root_id} opened no PR to review"
                )));
            }
        };
        let issue_ref = pr_issue_ref(&pr_url).ok_or_else(|| StoreError::Invalid {
            field: "pr_url",
            message: format!("`{pr_url}` is not a pull request URL"),
        })?;
        let rounds = sqlx::query_scalar!(
            r#"SELECT coalesce(max(review_round), 0) AS "rounds!" FROM jobs WHERE review_of = $1"#,
            root_id,
        )
        .fetch_one(&mut *tx)
        .await?;
        let rounds = u32::try_from(rounds).unwrap_or(u32::MAX);
        if rounds >= self.max_review_rounds {
            return Err(StoreError::Conflict(format!(
                "the PR of job {root_id} had its {} review round(s)",
                self.max_review_rounds
            )));
        }
        budget::check(&mut tx, &self.budget, crate::RepoId(root.repo_id)).await?;
        let round = rounds + 1;
        let key = IssueId::parse(format!("{}-v{round}", root.ledger_key))?;
        let row = sqlx::query_as!(
            JobRow,
            "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source, review_of, review_round)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
            root.repo_id,
            issue_ref,
            key.as_str(),
            JobSource::Review.as_str(),
            root_id,
            i32::try_from(round).unwrap_or(i32::MAX),
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from_db(e) {
            StoreError::Conflict(_) => {
                StoreError::Conflict(format!("the PR of job {root_id} has a live review round"))
            }
            other => other,
        })?;
        let job: Job = row.try_into()?;
        let detail = serde_json::json!({ "review_of": root_id, "round": round });
        audit::record(&mut tx, actor, "job.review", Target::of_job(&job), detail).await?;
        tx.commit().await?;
        Ok(job)
    }

    /// The job that opened the PR review round `job` works on.
    pub async fn review_root(&self, job: &Job) -> Result<Option<Job>, StoreError> {
        match job.review_of {
            Some(root) => self.job(root).await,
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pr_url_is_its_issue_ref() {
        for (url, want) in [
            (
                "https://github.com/acme-bot/typst/pull/12",
                Some("acme-bot/typst#12"),
            ),
            (
                "http://127.0.0.1:8070/demo-org/demo/pull/3/",
                Some("demo-org/demo#3"),
            ),
            ("https://github.com/acme/typst/issues/12", None),
            ("https://github.com/acme/../pull/1", None),
            ("https://github.com/acme/typst/pull/x", None),
            ("pull/1", None),
        ] {
            assert_eq!(pr_issue_ref(url).as_deref(), want, "{url}");
        }
    }
}
