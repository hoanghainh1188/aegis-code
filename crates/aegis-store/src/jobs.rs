//! Jobs and their runs. A job is queued, runs one or more attempts, and
//! ends once; at most one of its runs is live at a time.

use aegis_ledger::IssueId;
use chrono::{DateTime, Utc};

use crate::audit::{self, Target};
use crate::budget;
use crate::types::{count, decode_outcome, outcome_str};
use crate::{
    Actor, Job, JobEnd, JobFilter, JobId, JobSource, JobStatus, NewJob, Page, PageOf, RepoId, Run,
    RunEnd, RunId, RunStatus, Store, StoreError,
};

pub(crate) struct JobRow {
    pub(crate) id: i64,
    pub(crate) repo_id: i64,
    pub(crate) issue_ref: String,
    pub(crate) ledger_key: String,
    pub(crate) source: String,
    pub(crate) status: String,
    pub(crate) outcome: Option<String>,
    pub(crate) attempts: i32,
    pub(crate) locked_by: Option<String>,
    pub(crate) locked_until: Option<DateTime<Utc>>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) run_after: DateTime<Utc>,
    pub(crate) retries: i32,
    pub(crate) cancel_requested: bool,
    pub(crate) reason: Option<String>,
    pub(crate) pr_url: Option<String>,
    pub(crate) retry_of: Option<i64>,
    pub(crate) review_of: Option<i64>,
    pub(crate) review_round: i32,
}

impl TryFrom<JobRow> for Job {
    type Error = StoreError;

    fn try_from(row: JobRow) -> Result<Self, StoreError> {
        Ok(Self {
            id: JobId(row.id),
            repo_id: RepoId(row.repo_id),
            issue_ref: row.issue_ref,
            ledger_key: IssueId::parse(row.ledger_key)?,
            source: JobSource::decode("jobs.source", &row.source)?,
            status: JobStatus::decode("jobs.status", &row.status)?,
            outcome: row.outcome.as_deref().map(decode_outcome).transpose()?,
            attempts: count("jobs.attempts", row.attempts)?,
            locked_by: row.locked_by,
            locked_until: row.locked_until,
            created_at: row.created_at,
            updated_at: row.updated_at,
            run_after: row.run_after,
            retries: count("jobs.retries", row.retries)?,
            cancel_requested: row.cancel_requested,
            reason: row.reason,
            pr_url: row.pr_url,
            retry_of: row.retry_of.map(JobId),
            review_of: row.review_of.map(JobId),
            review_round: count("jobs.review_round", row.review_round)?,
        })
    }
}

pub(crate) struct RunRow {
    pub(crate) id: i64,
    pub(crate) job_id: i64,
    pub(crate) attempt: i32,
    pub(crate) status: String,
    pub(crate) error: Option<String>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
}

impl TryFrom<RunRow> for Run {
    type Error = StoreError;

    fn try_from(row: RunRow) -> Result<Self, StoreError> {
        Ok(Self {
            id: RunId(row.id),
            job_id: JobId(row.job_id),
            attempt: count("runs.attempt", row.attempt)?,
            status: RunStatus::decode("runs.status", &row.status)?,
            error: row.error,
            started_at: row.started_at,
            ended_at: row.ended_at,
        })
    }
}

pub(crate) fn live(status: JobStatus) -> bool {
    matches!(status, JobStatus::Queued | JobStatus::Running)
}

impl Store {
    /// Queues a job. `Conflict` when the issue already has a live job or
    /// the ledger key is taken; `NotFound` when the repo does not exist;
    /// `BudgetSpent` when its monthly budget is (#31).
    pub async fn create_job(&self, job: &NewJob) -> Result<Job, StoreError> {
        self.create_job_by(job, &Actor::system()).await
    }

    /// [`Store::create_job`], audited as done by `actor`.
    pub async fn create_job_by(&self, job: &NewJob, actor: &Actor) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        budget::check(&mut tx, &self.budget, job.repo_id).await?;
        let created: Job = sqlx::query_as!(
            JobRow,
            "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source)
             VALUES ($1, $2, $3, $4)
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
            job.repo_id.0,
            job.issue_ref,
            job.ledger_key.as_str(),
            job.source.as_str(),
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from_db(e) {
            StoreError::NotFound(_) => StoreError::NotFound(format!("repo {}", job.repo_id.0)),
            other => other,
        })?
        .try_into()?;
        audit::record(
            &mut tx,
            actor,
            "job.create",
            Target::of_job(&created),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    pub async fn job(&self, id: JobId) -> Result<Option<Job>, StoreError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, status, outcome,
                    attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round
             FROM jobs WHERE id = $1",
            id.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Job::try_from)
        .transpose()
    }

    /// Newest first.
    pub async fn list_jobs(
        &self,
        filter: &JobFilter,
        page: Page,
    ) -> Result<PageOf<Job>, StoreError> {
        self.list_jobs_in(filter, None, page).await
    }

    /// [`Store::list_jobs`], only of the repos `within` when given.
    pub async fn list_jobs_in(
        &self,
        filter: &JobFilter,
        within: Option<&[RepoId]>,
        page: Page,
    ) -> Result<PageOf<Job>, StoreError> {
        let within: Option<Vec<i64>> = within.map(|repos| repos.iter().map(|r| r.0).collect());
        let rows = sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, status, outcome,
                    attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round
             FROM jobs
             WHERE ($1::bigint IS NULL OR repo_id = $1)
               AND ($2::text IS NULL OR status = $2)
               AND ($3::bigint IS NULL OR id < $3)
               AND ($5::bigint[] IS NULL OR repo_id = ANY($5))
             ORDER BY id DESC LIMIT $4",
            filter.repo_id.map(|r| r.0),
            filter.status.map(JobStatus::as_str),
            page.cursor(),
            page.fetch(),
            within.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;
        let jobs = rows
            .into_iter()
            .map(Job::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PageOf::new(jobs, page, |j| j.id.0))
    }

    /// Starts the job's next attempt. `Conflict` when the job has ended or
    /// a run of it is still live.
    pub async fn start_run(&self, job: JobId) -> Result<Run, StoreError> {
        let mut tx = self.pool.begin().await?;
        let status = sqlx::query_scalar!("SELECT status FROM jobs WHERE id = $1 FOR UPDATE", job.0)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
        let status = JobStatus::decode("jobs.status", &status)?;
        if !live(status) {
            return Err(StoreError::Conflict(format!(
                "job {} is {}",
                job.0,
                status.as_str()
            )));
        }
        let open = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM runs WHERE job_id = $1 AND status = 'running') AS "open!""#,
            job.0,
        )
        .fetch_one(&mut *tx)
        .await?;
        if open {
            return Err(StoreError::Conflict(format!(
                "job {} has a live run",
                job.0
            )));
        }
        let attempt = sqlx::query_scalar!(
            "UPDATE jobs SET attempts = attempts + 1, status = 'running', updated_at = now()
             WHERE id = $1 RETURNING attempts",
            job.0,
        )
        .fetch_one(&mut *tx)
        .await?;
        let run = sqlx::query_as!(
            RunRow,
            "INSERT INTO runs (job_id, attempt) VALUES ($1, $2)
             RETURNING id, job_id, attempt, status, error, started_at, ended_at",
            job.0,
            attempt,
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        run.try_into()
    }

    /// Ends a live run. `Conflict` when it has already ended.
    pub async fn end_run(&self, run: RunId, end: RunEnd) -> Result<Run, StoreError> {
        let row = sqlx::query_as!(
            RunRow,
            "UPDATE runs SET status = $2, error = $3, ended_at = now()
             WHERE id = $1 AND status = 'running'
             RETURNING id, job_id, attempt, status, error, started_at, ended_at",
            run.0,
            end.status().as_str(),
            end.error(),
        )
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => row.try_into(),
            None => match self.run(run).await? {
                Some(ended) => Err(StoreError::Conflict(format!(
                    "run {} is already {}",
                    run.0,
                    ended.status.as_str()
                ))),
                None => Err(StoreError::NotFound(format!("run {}", run.0))),
            },
        }
    }

    pub async fn run(&self, id: RunId) -> Result<Option<Run>, StoreError> {
        sqlx::query_as!(
            RunRow,
            "SELECT id, job_id, attempt, status, error, started_at, ended_at
             FROM runs WHERE id = $1",
            id.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Run::try_from)
        .transpose()
    }

    /// The job's runs, first attempt first.
    pub async fn list_runs(&self, job: JobId, page: Page) -> Result<PageOf<Run>, StoreError> {
        let rows = sqlx::query_as!(
            RunRow,
            "SELECT id, job_id, attempt, status, error, started_at, ended_at
             FROM runs WHERE job_id = $1 AND ($2::bigint IS NULL OR id > $2)
             ORDER BY id LIMIT $3",
            job.0,
            page.cursor(),
            page.fetch(),
        )
        .fetch_all(&self.pool)
        .await?;
        let runs = rows
            .into_iter()
            .map(Run::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PageOf::new(runs, page, |r| r.id.0))
    }

    /// Ends a live job and releases its lease. `Conflict` when it has
    /// already ended or a run of it is still live.
    pub async fn end_job(&self, job: JobId, end: JobEnd) -> Result<Job, StoreError> {
        let (status, outcome) = match end {
            JobEnd::Done(outcome) => (JobStatus::Done, Some(outcome_str(outcome))),
            JobEnd::Failed => (JobStatus::Failed, None),
            JobEnd::Cancelled => (JobStatus::Cancelled, None),
        };
        let mut tx = self.pool.begin().await?;
        let current =
            sqlx::query_scalar!("SELECT status FROM jobs WHERE id = $1 FOR UPDATE", job.0)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
        let current = JobStatus::decode("jobs.status", &current)?;
        if !live(current) {
            return Err(StoreError::Conflict(format!(
                "job {} is already {}",
                job.0,
                current.as_str()
            )));
        }
        let open = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM runs WHERE job_id = $1 AND status = 'running') AS "open!""#,
            job.0,
        )
        .fetch_one(&mut *tx)
        .await?;
        if open {
            return Err(StoreError::Conflict(format!(
                "job {} has a live run",
                job.0
            )));
        }
        let row = sqlx::query_as!(
            JobRow,
            "UPDATE jobs SET status = $2, outcome = $3, locked_by = NULL, locked_until = NULL,
                             updated_at = now()
             WHERE id = $1
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
            job.0,
            status.as_str(),
            outcome,
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        row.try_into()
    }
}
