//! A shadow job's PR, asked for by a person (#699): a job of its own that
//! opens the PR from the shadow job's patch, without the agent.

use aegis_ledger::IssueId;

use crate::audit::{self, Target};
use crate::jobs::JobRow;
use crate::{Actor, Job, JobId, Store, StoreError, budget, tx};

/// Why a shadow job's PR cannot be asked for, if it cannot; `None` when it
/// can. Checked under the lock of [`Store::publish_shadow_by`] too.
async fn refusal(
    conn: &mut sqlx::PgConnection,
    job: JobId,
    ledger_key: &str,
) -> Result<Option<String>, StoreError> {
    let patch = sqlx::query_scalar!("SELECT truncated FROM job_patches WHERE job_id = $1", job.0)
        .fetch_optional(&mut *conn)
        .await?;
    match patch {
        None => return Ok(Some(format!("job {} kept no patch", job.0))),
        Some(true) => {
            return Ok(Some(format!(
                "job {}'s patch was too large to keep whole; run the ticket again",
                job.0
            )));
        }
        Some(false) => {}
    }
    let kept = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM ledger_events
                          WHERE ledger_key = $1 AND event ->> 'event' = 'shadow_kept') AS "kept!""#,
        ledger_key,
    )
    .fetch_one(&mut *conn)
    .await?;
    if !kept {
        return Ok(Some(format!(
            "job {} kept no answer to open a PR from (only a bug fix's is kept)",
            job.0
        )));
    }
    let asked = sqlx::query_scalar!(
        "SELECT id FROM jobs WHERE publish_of = $1 ORDER BY id LIMIT 1",
        job.0
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(asked
        .map(|id| format!("its PR was asked for already: job {id}; retry that one if it failed")))
}

impl Store {
    /// Whether the PR of shadow job `job` may be asked for: `None` when it
    /// may, else why not. Says nothing of who asks.
    pub async fn shadow_publish_refusal(&self, job: &Job) -> Result<Option<String>, StoreError> {
        if job.outcome != Some(aegis_ledger::Outcome::Shadowed) {
            return Ok(Some(format!(
                "job {} is not a shadow job whose gate passed",
                job.id.0
            )));
        }
        let mut conn = self.pool.acquire().await?;
        refusal(&mut conn, job.id, job.ledger_key.as_str()).await
    }

    /// Queues the job that opens the PR of shadow job `job` from its patch
    /// (#699), audited as asked by `actor`: under the ledger key
    /// `<its key>-pr`, from the same ticket, not a shadow job. `Conflict`
    /// when it ended otherwise than `shadowed`, kept no whole patch or
    /// answer, or its PR was asked for already; `BudgetSpent` when its
    /// repo's budget is.
    pub async fn publish_shadow_by(&self, job: JobId, actor: &Actor) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let shadow = sqlx::query!(
                "SELECT repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                        status, outcome
                 FROM jobs WHERE id = $1 FOR UPDATE",
                job.0,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
            if (shadow.status.as_str(), shadow.outcome.as_deref()) != ("done", Some("shadowed")) {
                return Err(StoreError::Conflict(format!(
                    "job {} is not a shadow job whose gate passed",
                    job.0
                )));
            }
            if let Some(why) = refusal(&mut tx, job, &shadow.ledger_key).await? {
                return Err(StoreError::Conflict(why));
            }
            budget::check(&mut tx, &self.budget, crate::RepoId(shadow.repo_id)).await?;
            let key = IssueId::parse(format!("{}-pr", shadow.ledger_key))?;
            let created: Job = sqlx::query_as!(
                JobRow,
                "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                                   publish_of)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 RETURNING id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                           status, outcome, attempts, locked_by, locked_until, created_at,
                           updated_at, run_after, retries, cancel_requested, reason, pr_url,
                           retry_of, review_of, review_round, shadow, publish_of",
                shadow.repo_id,
                shadow.issue_ref,
                key.as_str(),
                shadow.source,
                shadow.priority,
                shadow.sla_deadline,
                job.0,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?
            .try_into()?;
            let detail = serde_json::json!({ "shadow_job": job.0 });
            audit::record(
                &mut tx,
                actor,
                "job.publish_shadow",
                Target::of_job(&created),
                detail,
            )
            .await?;
            Ok::<_, StoreError>(created)
        }
        .await;
        tx::commit(tx, work).await
    }
}
