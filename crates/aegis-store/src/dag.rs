//! The feature lane's job model (#675, ADR 0019): a job that planned a
//! feature has sub-jobs, each claimed only once the jobs it depends on
//! opened their PR. A plan that waits for a person ends
//! `awaiting_approval`; its approval and revisions are in `design_gate`.

use std::collections::BTreeSet;

use crate::audit::{self, Target};
use crate::budget;
use crate::jobs::JobRow;
use crate::tx;
use crate::{Actor, Job, JobId, NewJob, RepoId, SlaPriority, Store, StoreError};

/// The most sub-jobs one job plans, and the most jobs one depends on.
pub const MAX_SUB_JOBS: usize = 50;

/// Where a job sits in its feature's DAG.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobLinks {
    /// The job that planned it.
    pub parent: Option<JobId>,
    /// The jobs that must open their PR before it is claimed, in order.
    pub depends_on: Vec<JobId>,
}

impl Store {
    /// Queues `job` as a sub-job of `parent`, claimed only once every job of
    /// `depends_on` opened its PR. `parent` is a job with no parent of its
    /// own, in the same repo; `depends_on` are sub-jobs of `parent` already
    /// made, so no cycle can be formed. `BudgetSpent` when the repo's
    /// monthly budget (#31) or the feature's own budget is spent.
    pub async fn create_sub_job_by(
        &self,
        parent: JobId,
        depends_on: &[JobId],
        job: &NewJob,
        actor: &Actor,
    ) -> Result<Job, StoreError> {
        let deps: BTreeSet<JobId> = depends_on.iter().copied().collect();
        if deps.len() > MAX_SUB_JOBS {
            return Err(StoreError::Invalid {
                field: "depends_on",
                message: format!("at most {MAX_SUB_JOBS} jobs"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = insert_sub_job(&mut tx, self, parent, &deps, job, None, actor);
        let work = work.await;
        tx::commit(tx, work).await
    }

    /// The parent of `job` and the jobs it depends on.
    pub async fn job_links(&self, job: JobId) -> Result<JobLinks, StoreError> {
        let parent = sqlx::query_scalar!("SELECT parent_job_id FROM jobs WHERE id = $1", job.0)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
        let depends_on = sqlx::query_scalar!(
            "SELECT depends_on FROM job_dependencies WHERE job_id = $1 ORDER BY depends_on",
            job.0,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(JobLinks {
            parent: parent.map(JobId),
            depends_on: depends_on.into_iter().map(JobId).collect(),
        })
    }

    /// Which of its feature plan's sub-jobs `job` is (#678), by the plan's
    /// id for it; `None` for a job made otherwise.
    pub async fn plan_key(&self, job: JobId) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar!("SELECT plan_key FROM jobs WHERE id = $1", job.0)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))
    }

    /// Whether every part of the feature `parent` planned is merged into
    /// its integration branch (#678): it has sub-jobs, and the last run of
    /// each ended `integrated`.
    pub async fn feature_integrated(&self, parent: JobId) -> Result<bool, StoreError> {
        Ok(sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM jobs WHERE parent_job_id = $1)
                      AND NOT EXISTS (
                          SELECT 1 FROM jobs s
                          WHERE s.parent_job_id = $1
                            AND NOT EXISTS (SELECT 1 FROM jobs r WHERE r.retry_of = s.id)
                            AND (s.status <> 'done' OR s.outcome IS DISTINCT FROM 'integrated'))
               AS "integrated!""#,
            parent.0,
        )
        .fetch_one(&self.pool)
        .await?)
    }

    /// The sub-jobs `parent` planned, in the order they were made.
    pub async fn sub_jobs(&self, parent: JobId) -> Result<Vec<Job>, StoreError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                    status, outcome, attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of,
                    review_round, shadow, publish_of
             FROM jobs WHERE parent_job_id = $1 ORDER BY id LIMIT $2",
            parent.0,
            MAX_SUB_JOBS as i64 * 4,
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Job::try_from)
        .collect()
    }

    /// The newest plan of ticket `issue_ref` of `repo` that waits for
    /// approval, if any: what `/aegis approve` on the ticket approves.
    pub async fn awaiting_approval_for_issue(
        &self,
        repo: RepoId,
        issue_ref: &str,
    ) -> Result<Option<Job>, StoreError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                    status, outcome, attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of,
                    review_round, shadow, publish_of
             FROM jobs
             WHERE repo_id = $1 AND issue_ref = $2 AND parent_job_id IS NULL
               AND status = 'done' AND outcome = 'awaiting_approval'
             ORDER BY id DESC LIMIT 1",
            repo.0,
            issue_ref,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Job::try_from)
        .transpose()
    }
}

/// When `job`, a sub-job just ended `integrated`, is the last part of its
/// feature to be merged (#678): queues the job that planned it again, in
/// `conn`'s transaction, to open the feature's one PR. Only an approved
/// plan that still waits, whose every sub-job (the last run of each) is
/// integrated, and whose ticket has no other live job.
pub(crate) async fn deliver_when_integrated(
    conn: &mut sqlx::PgConnection,
    job: JobId,
) -> Result<(), StoreError> {
    let Some(parent) = sqlx::query_scalar!("SELECT parent_job_id FROM jobs WHERE id = $1", job.0)
        .fetch_one(&mut *conn)
        .await?
    else {
        return Ok(());
    };
    // One part at a time: the last to commit sees every other one.
    sqlx::query!("SELECT id FROM jobs WHERE id = $1 FOR UPDATE", parent)
        .fetch_one(&mut *conn)
        .await?;
    let queued = sqlx::query!(
        "UPDATE jobs p SET status = 'queued', outcome = NULL, reason = NULL,
                           run_after = now(), updated_at = now()
         WHERE p.id = $1 AND p.status = 'done' AND p.outcome = 'awaiting_approval'
           AND EXISTS (SELECT 1 FROM job_approvals a WHERE a.job_id = p.id)
           AND NOT EXISTS (
               SELECT 1 FROM jobs s
               WHERE s.parent_job_id = p.id
                 AND NOT EXISTS (SELECT 1 FROM jobs r WHERE r.retry_of = s.id)
                 AND (s.status <> 'done' OR s.outcome IS DISTINCT FROM 'integrated'))
           AND NOT EXISTS (
               SELECT 1 FROM jobs o
               WHERE o.repo_id = p.repo_id AND o.issue_ref = p.issue_ref
                 AND o.parent_job_id IS NULL AND o.id <> p.id
                 AND o.status IN ('queued', 'running'))",
        parent,
    )
    .execute(&mut *conn)
    .await?;
    if queued.rows_affected() == 1 {
        tracing::info!(job = parent, "every part merged: the feature's PR is next");
    }
    Ok(())
}

/// Queues `job` under `parent` in `conn`'s transaction, as
/// [`Store::create_sub_job_by`] says, as the plan's sub-job `plan_key` when
/// one is given.
pub(crate) async fn insert_sub_job(
    conn: &mut sqlx::PgConnection,
    store: &Store,
    parent: JobId,
    deps: &BTreeSet<JobId>,
    job: &NewJob,
    plan_key: Option<&str>,
    actor: &Actor,
) -> Result<Job, StoreError> {
    let root = sqlx::query!(
        "SELECT repo_id, parent_job_id FROM jobs WHERE id = $1 FOR UPDATE",
        parent.0,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("job {}", parent.0)))?;
    if root.repo_id != job.repo_id.0 {
        return Err(invalid("parent", "a job of another repo"));
    }
    if root.parent_job_id.is_some() {
        return Err(invalid("parent", "a sub-job plans no sub-jobs"));
    }
    let planned = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM jobs WHERE parent_job_id = $1"#,
        parent.0,
    )
    .fetch_one(&mut *conn)
    .await?;
    if planned >= MAX_SUB_JOBS as i64 {
        return Err(invalid(
            "parent",
            &format!("it has {MAX_SUB_JOBS} sub-jobs already"),
        ));
    }
    let ids: Vec<i64> = deps.iter().map(|d| d.0).collect();
    let siblings = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM jobs WHERE id = ANY($1) AND parent_job_id = $2"#,
        &ids,
        parent.0,
    )
    .fetch_one(&mut *conn)
    .await?;
    if siblings != ids.len() as i64 {
        return Err(invalid(
            "depends_on",
            "every job depended on is a sub-job of the same parent",
        ));
    }
    budget::check(&mut *conn, &store.budget, job.repo_id).await?;
    crate::epic::check(&mut *conn, &store.budget, parent).await?;
    let created: Job = sqlx::query_as!(
        JobRow,
        "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                               parent_job_id, plan_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                       status, outcome, attempts, locked_by, locked_until, created_at,
                       updated_at, run_after, retries, cancel_requested, reason, pr_url,
                       retry_of, review_of, review_round, shadow, publish_of",
        job.repo_id.0,
        job.issue_ref,
        job.ledger_key.as_str(),
        job.source.as_str(),
        job.priority.map(SlaPriority::as_str),
        job.sla_deadline,
        parent.0,
        plan_key,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(StoreError::from_db)?
    .try_into()?;
    sqlx::query!(
        "INSERT INTO job_dependencies (job_id, depends_on)
             SELECT $1, unnest($2::bigint[])",
        created.id.0,
        &ids,
    )
    .execute(&mut *conn)
    .await?;
    let detail = serde_json::json!({ "parent": parent.0, "depends_on": ids, "plan_key": plan_key });
    audit::record(
        &mut *conn,
        actor,
        "job.create",
        Target::of_job(&created),
        detail,
    )
    .await?;
    Ok(created)
}

fn invalid(field: &'static str, message: &str) -> StoreError {
    StoreError::Invalid {
        field,
        message: message.to_owned(),
    }
}
