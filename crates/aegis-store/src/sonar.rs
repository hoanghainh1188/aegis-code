//! Sonar warnings polled from a repo's project (#27): the new warnings of
//! one poll and the job of their batch are written in one transaction, so
//! a warning is taken by one batch however often the poll finds it.

use crate::deliveries::queue_in;
use crate::tx;
use crate::{Actor, Delivered, NewJob, RepoId, Store, StoreError};

/// The most warnings one batch takes.
pub const MAX_BATCH_WARNINGS: usize = 100;
const MAX_WARNING_KEY_LEN: usize = 128;

impl Store {
    /// Takes the warnings of `keys` not taken before for the batch `job`
    /// (its `issue_ref` names the batch) and queues the job for them, as
    /// [`Store::deliver_job`] does. `None` when every warning was taken
    /// before, or the batch already has a live job: nothing is written.
    /// Otherwise the job and the keys it took.
    pub async fn deliver_sonar_batch(
        &self,
        job: &NewJob,
        keys: &[String],
    ) -> Result<Option<(Delivered, Vec<String>)>, StoreError> {
        if keys.len() > MAX_BATCH_WARNINGS {
            return Err(StoreError::Invalid {
                field: "keys",
                message: format!("at most {MAX_BATCH_WARNINGS} warnings"),
            });
        }
        if keys
            .iter()
            .any(|k| k.is_empty() || k.len() > MAX_WARNING_KEY_LEN)
        {
            return Err(StoreError::Invalid {
                field: "keys",
                message: format!("each key 1 to {MAX_WARNING_KEY_LEN} bytes"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let mut taken = sqlx::query_scalar!(
                "INSERT INTO sonar_warnings (repo_id, warning_key, batch)
                 SELECT $1, k, $3 FROM unnest($2::text[]) AS k
                 ON CONFLICT (repo_id, warning_key) DO NOTHING
                 RETURNING warning_key",
                job.repo_id.0,
                keys,
                job.issue_ref,
            )
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| match StoreError::from_db(e) {
                StoreError::NotFound(_) => StoreError::NotFound(format!("repo {}", job.repo_id.0)),
                other => other,
            })?;
            if taken.is_empty() {
                return Ok(tx::Done::Discard(None));
            }
            taken.sort();
            let detail = serde_json::json!({ "batch": job.issue_ref, "warnings": taken.len() });
            let (_, delivered) =
                queue_in(&mut tx, &self.budget, &Actor::webhook("sonar"), detail, job).await?;
            // Its warnings stay free for the batch after the live one.
            if matches!(delivered, Delivered::AlreadyLive(_)) {
                return Ok(tx::Done::Discard(None));
            }
            Ok::<_, StoreError>(tx::Done::Commit(Some((delivered, taken))))
        }
        .await;
        tx::end(tx, work).await
    }

    /// The keys of the warnings batch `batch` of `repo` took, sorted.
    pub async fn sonar_batch_warnings(
        &self,
        repo: RepoId,
        batch: &str,
    ) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar!(
            "SELECT warning_key FROM sonar_warnings WHERE repo_id = $1 AND batch = $2
             ORDER BY warning_key",
            repo.0,
            batch,
        )
        .fetch_all(&self.pool)
        .await?)
    }
}
