//! The job queue (ADR 0003 §3, #23). A worker claims the oldest queued job
//! with `FOR UPDATE SKIP LOCKED`, in the same statement that applies the
//! worker's `max_workers` and the repo's `repos.max_concurrent_jobs`, and holds a lease on it that it renews
//! while the job runs. Every write to a running job is conditioned on that
//! lease: a worker that lost it (expired, reclaimed) can no longer end it.

use std::time::Duration;

use aegis_ledger::{IssueId, Outcome};

use crate::audit::{self, Target};
use crate::budget;
use crate::jobs::{JobRow, RunRow, live};
use crate::types::outcome_str;
use crate::{Actor, Job, JobId, JobStatus, Run, RunStatus, Store, StoreError};

/// Claims take this transaction-scoped advisory lock, one at a time: the
/// count of a repo's running jobs is then exact when the next claim reads
/// it. Claims are short and a few per minute; the job runs unlocked.
const CLAIM_LOCK: i64 = 0x6165_6769_735f_6a6f; // "aegis_jo"

/// Rows one reclaim handles; the rest wait for the next one.
const RECLAIM_BATCH: i64 = 100;

/// A claimed job and the run that just started for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub job: Job,
    pub run: Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lease {
    Held {
        cancel_requested: bool,
    },
    /// Expired and reclaimed, or the job is no longer running.
    Lost,
}

/// How a claimed run ends, and what becomes of its job.
#[derive(Debug, Clone, PartialEq)]
pub enum AttemptEnd {
    /// The agent's result: a PR or a stop. Never retried.
    Done {
        outcome: Outcome,
        reason: Option<String>,
        pr_url: Option<String>,
    },
    Cancelled {
        reason: String,
    },
    /// Not retried: retries spent, or a failure no retry can fix.
    Failed {
        error: String,
    },
    /// An infrastructure failure: queued again after `after`.
    Retry {
        error: String,
        after: Duration,
    },
    /// Cut short by a shutdown: the run is interrupted, the job queued
    /// again at once, with no retry spent.
    Released,
}

impl AttemptEnd {
    fn run_status(&self) -> RunStatus {
        match self {
            Self::Done { .. } | Self::Cancelled { .. } => RunStatus::Finished,
            Self::Failed { .. } | Self::Retry { .. } => RunStatus::Failed,
            Self::Released => RunStatus::Interrupted,
        }
    }

    fn job_status(&self) -> JobStatus {
        match self {
            Self::Done { .. } => JobStatus::Done,
            Self::Cancelled { .. } => JobStatus::Cancelled,
            Self::Failed { .. } => JobStatus::Failed,
            Self::Retry { .. } | Self::Released => JobStatus::Queued,
        }
    }

    fn error(&self) -> Option<&str> {
        match self {
            Self::Failed { error } | Self::Retry { error, .. } => Some(error),
            Self::Done { .. } | Self::Cancelled { .. } | Self::Released => None,
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::Done { reason, .. } => reason.as_deref(),
            Self::Cancelled { reason } => Some(reason),
            Self::Failed { error } | Self::Retry { error, .. } => Some(error),
            Self::Released => Some("released at shutdown; queued again"),
        }
    }
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

impl Store {
    /// The oldest queued job `worker` may take while it holds fewer than
    /// `max_workers` and the job's repo runs fewer than its
    /// `max_concurrent_jobs` and has budget left this month (#31), now
    /// running under a lease of `lease`; `None` when there is none. Queued
    /// jobs of a repo whose budget is spent wait, and say so.
    pub async fn claim_job(
        &self,
        worker: &str,
        lease: Duration,
        max_workers: u32,
    ) -> Result<Option<Claim>, StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(CLAIM_LOCK)
            .execute(&mut *tx)
            .await?;
        let held = budget::hold_spent(&mut tx, &self.budget).await?;
        let row = sqlx::query_as!(
            JobRow,
            "WITH busy AS (
                 SELECT repo_id, count(*) AS n FROM jobs WHERE status = 'running'
                 GROUP BY repo_id
             ),
             next AS (
                 SELECT j.id FROM jobs j
                 JOIN repos r ON r.id = j.repo_id
                 LEFT JOIN busy b ON b.repo_id = j.repo_id
                 WHERE j.status = 'queued' AND j.run_after <= now()
                   AND NOT j.cancel_requested
                   AND coalesce(b.n, 0) < r.max_concurrent_jobs
                   AND j.repo_id <> ALL($4)
                   AND (SELECT count(*) FROM jobs
                        WHERE status = 'running' AND locked_by = $1) < $3
                 ORDER BY j.id
                 LIMIT 1
                 FOR UPDATE OF j SKIP LOCKED
             )
             UPDATE jobs SET status = 'running', attempts = attempts + 1, locked_by = $1,
                             locked_until = now() + make_interval(secs => $2),
                             reason = CASE WHEN starts_with(reason, $5) THEN NULL
                                      ELSE reason END,
                             updated_at = now()
             FROM next WHERE jobs.id = next.id
             RETURNING jobs.id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
            worker,
            secs(lease),
            i64::from(max_workers),
            &held,
            budget::waiting_prefix(),
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let run = sqlx::query_as!(
            RunRow,
            "INSERT INTO runs (job_id, attempt) VALUES ($1, $2)
             RETURNING id, job_id, attempt, status, error, started_at, ended_at",
            row.id,
            row.attempts,
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(Claim {
            job: row.try_into()?,
            run: run.try_into()?,
        }))
    }

    /// Extends the lease by `lease` from now, and says whether a cancel
    /// was asked for.
    pub async fn renew_lease(
        &self,
        job: JobId,
        worker: &str,
        lease: Duration,
    ) -> Result<Lease, StoreError> {
        let cancel = sqlx::query_scalar!(
            "UPDATE jobs SET locked_until = now() + make_interval(secs => $3)
             WHERE id = $1 AND locked_by = $2 AND status = 'running'
             RETURNING cancel_requested",
            job.0,
            worker,
            secs(lease),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(match cancel {
            Some(cancel_requested) => Lease::Held { cancel_requested },
            None => Lease::Lost,
        })
    }

    /// Ends the claimed run and moves its job on. `Conflict` when `worker`
    /// no longer holds the lease or the run has already ended.
    pub async fn finish_attempt(
        &self,
        run: &Run,
        worker: &str,
        end: &AttemptEnd,
    ) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        let held = sqlx::query_scalar!(
            "SELECT id FROM jobs WHERE id = $1 AND locked_by = $2 AND status = 'running'
             FOR UPDATE",
            run.job_id.0,
            worker,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if held.is_none() {
            return Err(StoreError::Conflict(format!(
                "job {} is not leased to {worker}",
                run.job_id.0
            )));
        }
        let ended = sqlx::query!(
            "UPDATE runs SET status = $3, error = $4, ended_at = now()
             WHERE id = $1 AND job_id = $2 AND status = 'running'",
            run.id.0,
            run.job_id.0,
            end.run_status().as_str(),
            end.error(),
        )
        .execute(&mut *tx)
        .await?;
        if ended.rows_affected() == 0 {
            return Err(StoreError::Conflict(format!(
                "run {} is not running",
                run.id.0
            )));
        }
        let (outcome, pr_url) = match end {
            AttemptEnd::Done {
                outcome, pr_url, ..
            } => (Some(outcome_str(*outcome)), pr_url.as_deref()),
            _ => (None, None),
        };
        let (retried, delay) = match end {
            AttemptEnd::Retry { after, .. } => (1, Some(secs(*after))),
            AttemptEnd::Released => (0, Some(0.0)),
            _ => (0, None),
        };
        let row = sqlx::query_as!(
            JobRow,
            "UPDATE jobs SET status = $2, outcome = $3, reason = $4, pr_url = $5,
                             retries = retries + $6,
                             run_after = CASE WHEN $7::float8 IS NULL THEN run_after
                                         ELSE now() + make_interval(secs => $7) END,
                             locked_by = NULL, locked_until = NULL, updated_at = now()
             WHERE id = $1
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
            run.job_id.0,
            end.job_status().as_str(),
            outcome,
            end.reason(),
            pr_url,
            retried,
            delay,
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        row.try_into()
    }

    /// Jobs whose lease ran out (their worker died or hung): the live run
    /// is marked `interrupted` and the job queued again from the start,
    /// or failed once `max_retries` are spent, or cancelled when that was
    /// asked for. Returns the jobs handled.
    pub async fn reclaim_expired(&self, max_retries: u32) -> Result<Vec<JobId>, StoreError> {
        let max = i32::try_from(max_retries).unwrap_or(i32::MAX);
        let ids = sqlx::query_scalar!(
            "WITH expired AS (
                 SELECT id FROM jobs WHERE status = 'running' AND locked_until < now()
                 ORDER BY id LIMIT $2
                 FOR UPDATE SKIP LOCKED
             ),
             closed AS (
                 UPDATE runs SET status = 'interrupted', error = 'lease expired',
                                 ended_at = now()
                 WHERE job_id IN (SELECT id FROM expired) AND status = 'running'
             )
             UPDATE jobs SET
                 status = CASE WHEN cancel_requested THEN 'cancelled'
                               WHEN retries < $1 THEN 'queued'
                               ELSE 'failed' END,
                 reason = CASE WHEN cancel_requested THEN 'cancelled; its lease expired'
                               WHEN retries < $1 THEN 'lease expired; queued again'
                               ELSE 'lease expired with no retry left' END,
                 retries = retries + 1, run_after = now(),
                 locked_by = NULL, locked_until = NULL, updated_at = now()
             WHERE id IN (SELECT id FROM expired)
             RETURNING id",
            max,
            RECLAIM_BATCH,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(ids.into_iter().map(JobId).collect())
    }

    /// A queued job is cancelled at once; a running one is marked, and its
    /// worker stops it when it next renews the lease. `Conflict` when the
    /// job has ended.
    pub async fn request_cancel(&self, job: JobId) -> Result<Job, StoreError> {
        self.request_cancel_by(job, &Actor::system()).await
    }

    /// [`Store::request_cancel`], audited as done by `actor`.
    pub async fn request_cancel_by(&self, job: JobId, actor: &Actor) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as!(
            JobRow,
            "UPDATE jobs SET cancel_requested = true,
                 status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END,
                 reason = CASE WHEN status = 'queued' THEN 'cancelled before it ran'
                               ELSE reason END,
                 updated_at = now()
             WHERE id = $1 AND status IN ('queued', 'running')
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
            job.0,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            drop(tx);
            return Err(self.ended_or_missing(job).await);
        };
        let cancelled: Job = row.try_into()?;
        audit::record(
            &mut tx,
            actor,
            "job.cancel",
            Target::of_job(&cancelled),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await?;
        Ok(cancelled)
    }

    /// A new job for the issue of a failed or cancelled one, with a ledger
    /// of its own (`<key>-r<n>`) and so a budget of its own. A job that is
    /// done has the agent's result and is not run again; `BudgetSpent`
    /// when the repo's monthly budget is (#31).
    pub async fn retry_job(&self, job: JobId) -> Result<Job, StoreError> {
        self.retry_job_by(job, &Actor::system()).await
    }

    /// [`Store::retry_job`], audited as done by `actor`.
    pub async fn retry_job_by(&self, job: JobId, actor: &Actor) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        let old = sqlx::query!(
            "SELECT repo_id, issue_ref, ledger_key, source, status, review_of, review_round
             FROM jobs WHERE id = $1 FOR UPDATE",
            job.0,
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
        let status = JobStatus::decode("jobs.status", &old.status)?;
        if !matches!(status, JobStatus::Failed | JobStatus::Cancelled) {
            return Err(StoreError::Conflict(format!(
                "job {} is {}; only a failed or cancelled job is retried",
                job.0,
                status.as_str()
            )));
        }
        budget::check(&mut tx, &self.budget, crate::RepoId(old.repo_id)).await?;
        let base = retry_base(&old.ledger_key);
        let taken = sqlx::query_scalar!(
            "SELECT ledger_key FROM jobs WHERE starts_with(ledger_key, $1)",
            base,
        )
        .fetch_all(&mut *tx)
        .await?;
        let key = IssueId::parse(next_retry_key(base, &taken))?;
        let row = sqlx::query_as!(
            JobRow,
            "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source, retry_of, review_of, review_round)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
            old.repo_id,
            old.issue_ref,
            key.as_str(),
            old.source,
            job.0,
            // A review round run again is the same round of the same PR.
            old.review_of,
            old.review_round,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::from_db)?;
        let retried: Job = row.try_into()?;
        let detail = serde_json::json!({ "retry_of": job.0 });
        audit::record(
            &mut tx,
            actor,
            "job.retry",
            Target::of_job(&retried),
            detail,
        )
        .await?;
        tx.commit().await?;
        Ok(retried)
    }

    /// What the job's ledger has spent over all its runs, in $.
    pub async fn job_spent(&self, ledger_key: &IssueId) -> Result<f64, StoreError> {
        let spent = sqlx::query_scalar!(
            r#"SELECT coalesce(sum(cost_usd), 0) AS "spent!" FROM ledger_events
               WHERE ledger_key = $1"#,
            ledger_key.as_str(),
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(spent)
    }

    async fn ended_or_missing(&self, job: JobId) -> StoreError {
        match self.job(job).await {
            Ok(Some(j)) if !live(j.status) => {
                StoreError::Conflict(format!("job {} is already {}", job.0, j.status.as_str()))
            }
            Ok(Some(_)) => StoreError::Conflict(format!("job {} changed; try again", job.0)),
            Ok(None) => StoreError::NotFound(format!("job {}", job.0)),
            Err(e) => e,
        }
    }
}

/// `typst-12-r2` → `typst-12`: retries of retries count from the first.
fn retry_base(key: &str) -> &str {
    match key.rsplit_once("-R") {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => key,
    }
}

/// `<base>-r<n>` with `n` one more than any taken.
fn next_retry_key(base: &str, taken: &[String]) -> String {
    let n = taken
        .iter()
        .filter_map(|k| {
            k.strip_prefix(base)?
                .strip_prefix("-R")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    format!("{base}-r{}", n + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_are_numbered_from_the_first_job() {
        assert_eq!(retry_base("TYPST-12"), "TYPST-12");
        assert_eq!(retry_base("TYPST-12-R3"), "TYPST-12");
        assert_eq!(retry_base("TYPST-R"), "TYPST-R");
        let taken = [
            "TYPST-12".into(),
            "TYPST-12-R1".into(),
            "TYPST-12-R4".into(),
        ];
        assert_eq!(next_retry_key("TYPST-12", &taken), "TYPST-12-r5");
        assert_eq!(next_retry_key("TYPST-12", &[]), "TYPST-12-r1");
        // `TYPST-120` shares the prefix but is another issue.
        assert_eq!(
            next_retry_key("TYPST-12", &["TYPST-120-R9".into()]),
            "TYPST-12-r1"
        );
    }
}
