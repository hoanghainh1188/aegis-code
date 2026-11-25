//! The feature lane's job model (#675, ADR 0019): a job that planned a
//! feature has sub-jobs, each claimed only once the jobs it depends on
//! opened their PR; a plan that waits for a person ends
//! `awaiting_approval` and an approval queues it again.

use std::collections::BTreeSet;

use time::OffsetDateTime;

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

/// The last approval of a job's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// The actor's name as it was then: an e-mail, a forge login.
    pub by: String,
    pub at: OffsetDateTime,
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
        let work = async {
            let root = sqlx::query!(
                "SELECT repo_id, parent_job_id FROM jobs WHERE id = $1 FOR UPDATE",
                parent.0,
            )
            .fetch_optional(&mut *tx)
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
            .fetch_one(&mut *tx)
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
            .fetch_one(&mut *tx)
            .await?;
            if siblings != ids.len() as i64 {
                return Err(invalid(
                    "depends_on",
                    "every job depended on is a sub-job of the same parent",
                ));
            }
            budget::check(&mut tx, &self.budget, job.repo_id).await?;
            crate::epic::check(&mut tx, &self.budget, parent).await?;
            let created: Job = sqlx::query_as!(
                JobRow,
                "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                                   parent_job_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 RETURNING id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                           status, outcome, attempts, locked_by, locked_until, created_at,
                           updated_at, run_after, retries, cancel_requested, reason, pr_url,
                           retry_of, review_of, review_round",
                job.repo_id.0,
                job.issue_ref,
                job.ledger_key.as_str(),
                job.source.as_str(),
                job.priority.map(SlaPriority::as_str),
                job.sla_deadline,
                parent.0,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?
            .try_into()?;
            sqlx::query!(
                "INSERT INTO job_dependencies (job_id, depends_on)
                 SELECT $1, unnest($2::bigint[])",
                created.id.0,
                &ids,
            )
            .execute(&mut *tx)
            .await?;
            let detail = serde_json::json!({ "parent": parent.0, "depends_on": ids });
            audit::record(
                &mut tx,
                actor,
                "job.create",
                Target::of_job(&created),
                detail,
            )
            .await?;
            Ok::<_, StoreError>(created)
        }
        .await;
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

    /// The sub-jobs `parent` planned, in the order they were made.
    pub async fn sub_jobs(&self, parent: JobId) -> Result<Vec<Job>, StoreError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                    status, outcome, attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of,
                    review_round
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

    /// Queues again the job whose plan waited for approval, as approved by
    /// `actor`; who may approve is the caller's to check. `Conflict` when
    /// the job does not wait for approval, or its ticket has another live
    /// job; `BudgetSpent` when the repo's monthly budget is spent.
    pub async fn approve_job_by(&self, job: JobId, actor: &Actor) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let held = sqlx::query!(
                "SELECT repo_id, status, outcome FROM jobs WHERE id = $1 FOR UPDATE",
                job.0,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
            if (held.status.as_str(), held.outcome.as_deref())
                != ("done", Some("awaiting_approval"))
            {
                return Err(StoreError::Conflict(format!(
                    "job {} does not wait for approval",
                    job.0
                )));
            }
            budget::check(&mut tx, &self.budget, RepoId(held.repo_id)).await?;
            let approved: Job = sqlx::query_as!(
                JobRow,
                "UPDATE jobs SET status = 'queued', outcome = NULL, reason = NULL,
                                 cancel_requested = false, run_after = now(), updated_at = now()
                 WHERE id = $1
                 RETURNING id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                           status, outcome, attempts, locked_by, locked_until, created_at,
                           updated_at, run_after, retries, cancel_requested, reason, pr_url,
                           retry_of, review_of, review_round",
                job.0,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| match StoreError::from_db(e) {
                StoreError::Conflict(_) => StoreError::Conflict(format!(
                    "the ticket of job {} has another live job",
                    job.0
                )),
                other => other,
            })?
            .try_into()?;
            sqlx::query!(
                "INSERT INTO job_approvals (job_id, approved_by) VALUES ($1, $2)
                 ON CONFLICT (job_id) DO UPDATE
                 SET approved_by = EXCLUDED.approved_by, approved_at = now()",
                job.0,
                actor.name,
            )
            .execute(&mut *tx)
            .await?;
            audit::record(
                &mut tx,
                actor,
                "job.approve",
                Target::of_job(&approved),
                serde_json::json!({}),
            )
            .await?;
            Ok::<_, StoreError>(approved)
        }
        .await;
        tx::commit(tx, work).await
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
                    review_round
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

    /// The last approval of `job`'s plan, if it had one.
    pub async fn approval(&self, job: JobId) -> Result<Option<Approval>, StoreError> {
        Ok(sqlx::query!(
            "SELECT approved_by, approved_at FROM job_approvals WHERE job_id = $1",
            job.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|r| Approval {
            by: r.approved_by,
            at: r.approved_at,
        }))
    }
}

fn invalid(field: &'static str, message: &str) -> StoreError {
    StoreError::Invalid {
        field,
        message: message.to_owned(),
    }
}
