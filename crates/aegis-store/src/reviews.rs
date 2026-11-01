//! Review rounds (#243): after a review of an agent's PR, a job goes on
//! with that PR, on its branch. Each round has a ledger, so a budget, of
//! its own (`<key>-v<n>`), and is linked to the job that opened the PR;
//! a PR gets at most [`Store::max_review_rounds`] rounds.

use aegis_ledger::{IssueId, Outcome};

use crate::audit::{self, Target};
use crate::budget;
use crate::jobs::JobRow;
use crate::tx;
use crate::types::decode_outcome;
use crate::{Actor, Job, JobId, JobSource, JobStatus, Store, StoreError};

/// Rounds a PR gets when the server sets no other number.
pub const DEFAULT_MAX_REVIEW_ROUNDS: u32 = 3;

/// `https://github.com/<owner>/<name>/pull/<n>` → `<owner>/<name>#<n>`,
/// `https://<gitlab>/<group/sub/project>/-/merge_requests/<n>` →
/// `<group/sub/project>!<n>` (#254): the PR or MR as an issue ref, so it
/// has one live round at a time.
pub fn pr_issue_ref(pr_url: &str) -> Option<String> {
    let plain = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if let Some((front, iid)) = pr_url
        .trim_end_matches('/')
        .split_once("/-/merge_requests/")
    {
        let iid: u64 = iid.parse().ok()?;
        let after_scheme = front.split_once("://").map_or(front, |(_, r)| r);
        let (_, project) = after_scheme.split_once('/')?;
        let parts: Vec<&str> = project.split('/').collect();
        let ok = (2..=21).contains(&parts.len()) && parts.iter().all(|p| plain(p));
        return ok.then(|| format!("{project}!{iid}"));
    }
    let parts: Vec<&str> = pr_url.trim_end_matches('/').rsplitn(4, '/').collect();
    let [number, "pull", name, rest] = parts.as_slice() else {
        return None;
    };
    let owner = rest.rsplit('/').next()?;
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
        let work = async {
            let job = self
                .review_in(&mut tx, of, actor, serde_json::json!({}))
                .await?;
            Ok::<_, StoreError>(job)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// [`Store::review_job_by`] in `tx`, with `detail` added to its audit.
    pub(crate) async fn review_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        of: JobId,
        actor: &Actor,
        mut detail: serde_json::Value,
    ) -> Result<Job, StoreError> {
        let asked = sqlx::query!("SELECT review_of FROM jobs WHERE id = $1 FOR UPDATE", of.0)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", of.0)))?;
        let root_id = asked.review_of.unwrap_or(of.0);
        let root = sqlx::query!(
            "SELECT repo_id, ledger_key, status, outcome, pr_url FROM jobs
             WHERE id = $1 FOR UPDATE",
            root_id,
        )
        .fetch_one(&mut **tx)
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
        // A live round reads the review when it runs: no other is needed,
        // whatever the rounds left.
        let live = sqlx::query_scalar!(
            "SELECT id FROM jobs WHERE review_of = $1 AND status IN ('queued', 'running') LIMIT 1",
            root_id,
        )
        .fetch_optional(&mut **tx)
        .await?;
        if live.is_some() {
            return Err(StoreError::Conflict(format!(
                "the PR of job {root_id} has a live review round"
            )));
        }
        let rounds = sqlx::query_scalar!(
            r#"SELECT coalesce(max(review_round), 0) AS "rounds!" FROM jobs WHERE review_of = $1"#,
            root_id,
        )
        .fetch_one(&mut **tx)
        .await?;
        let rounds = u32::try_from(rounds).unwrap_or(u32::MAX);
        if rounds >= self.max_review_rounds {
            return Err(StoreError::Conflict(format!(
                "the PR of job {root_id} had its {} review round(s)",
                self.max_review_rounds
            )));
        }
        budget::check(tx, &self.budget, crate::RepoId(root.repo_id)).await?;
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
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| match StoreError::from_db(e) {
            StoreError::Conflict(_) => {
                StoreError::Conflict(format!("the PR of job {root_id} has a live review round"))
            }
            other => other,
        })?;
        let job: Job = row.try_into()?;
        detail["review_of"] = root_id.into();
        detail["round"] = round.into();
        audit::record(tx, actor, "job.review", Target::of_job(&job), detail).await?;
        Ok(job)
    }

    /// The job that opened the PR at `pr_url` (case aside), the last one
    /// if a PR was somehow opened twice: what a review of it goes on with.
    pub async fn job_by_pr_url(&self, pr_url: &str) -> Result<Option<Job>, StoreError> {
        let url = pr_url.trim().trim_end_matches('/').to_ascii_lowercase();
        sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, status, outcome,
                    attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round
             FROM jobs
             WHERE review_of IS NULL AND status = 'done' AND outcome = 'pr_opened'
               AND lower(rtrim(pr_url, '/')) = $1
             ORDER BY id DESC LIMIT 1",
            url,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Job::try_from)
        .transpose()
    }

    /// The review rounds of the PR job `root` opened, in order, retries
    /// included (#245).
    pub async fn review_rounds_of(&self, root: JobId) -> Result<Vec<Job>, StoreError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, status, outcome,
                    attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round
             FROM jobs WHERE review_of = $1 ORDER BY review_round, id LIMIT 100",
            root.0,
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Job::try_from)
        .collect()
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
            (
                "https://gitlab.com/acme/platform/app/-/merge_requests/4",
                Some("acme/platform/app!4"),
            ),
            ("https://gitlab.com/app/-/merge_requests/4", None),
            ("https://gitlab.com/acme/app/-/merge_requests/x", None),
            ("https://github.com/acme/typst/issues/12", None),
            ("https://github.com/acme/../pull/1", None),
            ("https://github.com/acme/typst/pull/x", None),
            ("pull/1", None),
        ] {
            assert_eq!(pr_issue_ref(url).as_deref(), want, "{url}");
        }
    }
}
