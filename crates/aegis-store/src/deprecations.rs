//! A repo's deprecation batches (#705): one live at a time, queued on its
//! schedule, and the warnings earlier batches took, so the next takes
//! only new ones.

use crate::deliveries::queue_in;
use crate::tx;
use crate::{Actor, Delivered, NewJob, RepoId, Store, StoreError};

impl Store {
    /// Queues `job`, a `deprecation` batch, as [`Store::deliver_job`] does,
    /// unless the repo has one queued or running: then `None`, and nothing
    /// is written.
    pub async fn deliver_deprecation_batch(
        &self,
        job: &NewJob,
    ) -> Result<Option<Delivered>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            sqlx::query!(
                "SELECT pg_advisory_xact_lock(hashtextextended('deprecations', $1))",
                job.repo_id.0,
            )
            .execute(&mut *tx)
            .await?;
            let live = sqlx::query_scalar!(
                r#"SELECT EXISTS (
                     SELECT 1 FROM jobs WHERE repo_id = $1 AND source = 'deprecation'
                       AND status IN ('queued', 'running')
                   ) AS "live!""#,
                job.repo_id.0,
            )
            .fetch_one(&mut *tx)
            .await?;
            if live {
                return Ok(tx::Done::Discard(None));
            }
            let detail = serde_json::json!({ "batch": job.issue_ref });
            let (_, delivered) = queue_in(
                &mut tx,
                &self.budget,
                &Actor::webhook("deprecations"),
                detail,
                job,
            )
            .await?;
            Ok::<_, StoreError>(tx::Done::Commit(Some(delivered)))
        }
        .await;
        tx::end(tx, work).await
    }

    /// The warnings, by key, that `repo`'s deprecation batches took and
    /// that are still being fixed or were fixed in a PR: those of batches
    /// queued, running or ended with a PR. A batch that failed gave its
    /// warnings back.
    pub async fn deprecations_taken(&self, repo: RepoId) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar!(
            r#"SELECT DISTINCT k AS "k!"
               FROM jobs j
               JOIN ledger_events e ON e.ledger_key = j.ledger_key
                                   AND e.kind = 'deprecations_taken'
               CROSS JOIN LATERAL jsonb_array_elements_text(e.event -> 'keys') AS k
               WHERE j.repo_id = $1 AND j.source = 'deprecation'
                 AND (j.status IN ('queued', 'running') OR j.outcome IN ('pr_opened', 'shadowed'))
               ORDER BY 1"#,
            repo.0,
        )
        .fetch_all(&self.pool)
        .await?)
    }
}
