//! What the queue holds now, for metrics (#32): a few counts, no job.

use crate::{Store, StoreError};

/// The queue at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueStats {
    /// Queued and due: a worker with room would take them now.
    pub ready: i64,
    /// Queued but waiting for their retry's backoff.
    pub waiting: i64,
    pub running: i64,
    /// How long the oldest ready job has waited since it was due, in
    /// seconds; 0 when none is ready.
    pub oldest_ready_s: f64,
}

impl Store {
    pub async fn queue_stats(&self) -> Result<QueueStats, StoreError> {
        let row = sqlx::query!(
            r#"SELECT
                 count(*) FILTER (WHERE status = 'queued' AND run_after <= now()) AS "ready!",
                 count(*) FILTER (WHERE status = 'queued' AND run_after > now()) AS "waiting!",
                 count(*) FILTER (WHERE status = 'running') AS "running!",
                 coalesce(extract(epoch FROM now() - min(run_after)
                   FILTER (WHERE status = 'queued' AND run_after <= now())), 0)::float8
                   AS "oldest_ready_s!"
               FROM jobs WHERE status IN ('queued', 'running')"#
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(QueueStats {
            ready: row.ready,
            waiting: row.waiting,
            running: row.running,
            oldest_ready_s: row.oldest_ready_s,
        })
    }
}
