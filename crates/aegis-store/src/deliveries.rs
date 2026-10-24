//! Webhook deliveries (ADR 0003 §4, #25): a delivery and the job it asks
//! for are written in one transaction, so a delivery sent twice creates
//! one job, and a failure in between leaves neither.

use crate::jobs::JobRow;
use crate::{Delivered, JobId, NewDelivery, NewJob, Store, StoreError};

const MAX_DELIVERY_ID_LEN: usize = 128;

impl Store {
    /// Records `delivery` and queues `job` for it, unless the delivery was
    /// received before or the issue already has a live job. `NotFound`
    /// when the repo does not exist.
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
        // The live job of the issue wins over a new one, as in
        // `jobs_one_live_per_issue`.
        let created = sqlx::query_as!(
            JobRow,
            "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo_id, issue_ref) WHERE status IN ('queued', 'running') DO NOTHING
             RETURNING id, repo_id, issue_ref, ledger_key, source, status, outcome,
                       attempts, locked_by, locked_until, created_at, updated_at,
                       run_after, retries, cancel_requested, reason, pr_url, retry_of",
            job.repo_id.0,
            job.issue_ref,
            job.ledger_key.as_str(),
            job.source.as_str(),
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| match StoreError::from_db(e) {
            StoreError::NotFound(_) => StoreError::NotFound(format!("repo {}", job.repo_id.0)),
            other => other,
        })?;
        let (job_id, delivered) = match created {
            Some(row) => {
                let created: crate::Job = row.try_into()?;
                (created.id, Delivered::Queued(Box::new(created)))
            }
            None => {
                let live = sqlx::query_scalar!(
                    "SELECT id FROM jobs
                     WHERE repo_id = $1 AND issue_ref = $2 AND status IN ('queued', 'running')",
                    job.repo_id.0,
                    job.issue_ref,
                )
                .fetch_one(&mut *tx)
                .await?;
                (JobId(live), Delivered::AlreadyLive(JobId(live)))
            }
        };
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
