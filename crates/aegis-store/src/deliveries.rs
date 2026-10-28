//! Webhook deliveries (ADR 0003 §4, #25): a delivery and the job it asks
//! for are written in one transaction, so a delivery sent twice creates
//! one job, and a failure in between leaves neither.

use crate::audit::{self, Target};
use crate::budget;
use crate::jobs::JobRow;
use crate::{Actor, BudgetRules, Delivered, JobId, NewDelivery, NewJob, Store, StoreError};

const MAX_DELIVERY_ID_LEN: usize = 128;

impl Store {
    /// Records `delivery` and queues `job` for it, unless the delivery was
    /// received before or the issue already has a live job. When the
    /// repo's monthly budget is spent (#31), the job is recorded cancelled
    /// with the reason instead. `NotFound` when the repo does not exist.
    pub async fn deliver_job(
        &self,
        delivery: &NewDelivery,
        job: &NewJob,
    ) -> Result<Delivered, StoreError> {
        if delivery.delivery_id.is_empty() || delivery.delivery_id.len() > MAX_DELIVERY_ID_LEN {
            return Err(StoreError::Invalid {
                field: "delivery_id",
                message: format!("1 to {MAX_DELIVERY_ID_LEN} bytes"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let recorded = sqlx::query_scalar!(
            "INSERT INTO webhook_deliveries (source, delivery_id, event) VALUES ($1, $2, $3)
             ON CONFLICT (source, delivery_id) DO NOTHING
             RETURNING id",
            delivery.source.as_str(),
            delivery.delivery_id,
            delivery.event,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(recorded) = recorded else {
            return Ok(Delivered::Duplicate);
        };
        let detail = serde_json::json!({ "delivery_id": delivery.delivery_id });
        let (job_id, delivered) = queue_in(
            &mut tx,
            &self.budget,
            &Actor::webhook(delivery.source.as_str()),
            detail,
            job,
        )
        .await?;
        sqlx::query!(
            "UPDATE webhook_deliveries SET job_id = $2 WHERE id = $1",
            recorded,
            job_id.0,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(delivered)
    }
}

impl Store {
    /// Records `delivery` and queues the next review round of the PR job
    /// `of` opened (#244), as [`Store::review_job_by`] does, in one
    /// transaction: a delivery sent twice makes one round, and a round
    /// refused (no rounds left, one live, budget spent) records nothing.
    pub async fn deliver_review(
        &self,
        delivery: &NewDelivery,
        of: JobId,
    ) -> Result<Delivered, StoreError> {
        if delivery.delivery_id.is_empty() || delivery.delivery_id.len() > MAX_DELIVERY_ID_LEN {
            return Err(StoreError::Invalid {
                field: "delivery_id",
                message: format!("1 to {MAX_DELIVERY_ID_LEN} bytes"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let recorded = sqlx::query_scalar!(
            "INSERT INTO webhook_deliveries (source, delivery_id, event) VALUES ($1, $2, $3)
             ON CONFLICT (source, delivery_id) DO NOTHING
             RETURNING id",
            delivery.source.as_str(),
            delivery.delivery_id,
            delivery.event,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(recorded) = recorded else {
            return Ok(Delivered::Duplicate);
        };
        let actor = Actor::webhook(delivery.source.as_str());
        let detail = serde_json::json!({ "delivery_id": delivery.delivery_id });
        let job = self.review_in(&mut tx, of, &actor, detail).await?;
        sqlx::query!(
            "UPDATE webhook_deliveries SET job_id = $2 WHERE id = $1",
            recorded,
            job.id.0,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Delivered::Queued(Box::new(job)))
    }
}

/// Queues `job` in `tx` under `rules`, by `actor` with `detail` in the audit log: when
/// the repo's monthly budget is spent (#31) the job is recorded cancelled
/// with the reason; when the issue already has a live job, none is made.
pub(crate) async fn queue_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rules: &BudgetRules,
    actor: &Actor,
    mut detail: serde_json::Value,
    job: &NewJob,
) -> Result<(JobId, Delivered), StoreError> {
    let refused = match budget::check(tx, rules, job.repo_id).await {
        Ok(()) => None,
        Err(StoreError::BudgetSpent(reason)) => Some(reason),
        Err(e) => return Err(e),
    };
    // The live job of the issue wins over a new one, as in
    // `jobs_one_live_per_issue`, also over a refused one, which is not
    // live and so not under that index.
    let created = sqlx::query_as!(
        JobRow,
        "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source, status, reason)
             SELECT $1, $2, $3, $4,
                    CASE WHEN $5::text IS NULL THEN 'queued' ELSE 'cancelled' END, $5
             WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE repo_id = $1 AND issue_ref = $2
                               AND status IN ('queued', 'running'))
             ON CONFLICT (repo_id, issue_ref) WHERE status IN ('queued', 'running') DO NOTHING
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of, review_round",
        job.repo_id.0,
        job.issue_ref,
        job.ledger_key.as_str(),
        job.source.as_str(),
        refused,
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| match StoreError::from_db(e) {
        StoreError::NotFound(_) => StoreError::NotFound(format!("repo {}", job.repo_id.0)),
        other => other,
    })?;
    Ok(match created {
        Some(row) => {
            let created: crate::Job = row.try_into()?;
            detail["refused"] = refused.is_some().into();
            audit::record(tx, actor, "job.create", Target::of_job(&created), detail).await?;
            let id = created.id;
            match refused {
                None => (id, Delivered::Queued(Box::new(created))),
                Some(_) => (id, Delivered::Refused(Box::new(created))),
            }
        }
        None => {
            let live = sqlx::query_scalar!(
                "SELECT id FROM jobs
                     WHERE repo_id = $1 AND issue_ref = $2 AND status IN ('queued', 'running')",
                job.repo_id.0,
                job.issue_ref,
            )
            .fetch_one(&mut **tx)
            .await?;
            (JobId(live), Delivered::AlreadyLive(JobId(live)))
        }
    })
}
