//! What the queue holds now, for metrics (#32): a few counts, no job.

use crate::{Store, StoreError};

/// The queue at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueStats {
    /// Queued and due: a worker with room would take them now.
    pub ready: i64,
    /// Queued but waiting for their retry's backoff.
    pub waiting: i64,
    pub running: i64,
    /// How long the oldest ready job has waited since it was due, in
    /// seconds; 0 when none is ready.
    pub oldest_ready_s: f64,
    /// Ready jobs by SLA level (#524), `P1`, `P2`, `P3` and `none`, each
    /// always present.
    pub by_priority: Vec<PriorityStats>,
}

/// Ready jobs of one SLA level.
#[derive(Debug, Clone, PartialEq)]
pub struct PriorityStats {
    /// `P1`, `P2`, `P3`, or `none` for jobs without one.
    pub priority: &'static str,
    pub ready: i64,
    /// How long the oldest of them has waited since it was due; 0 for none.
    pub oldest_ready_s: f64,
}

/// The levels [`QueueStats::by_priority`] reports, in order.
pub const PRIORITY_LEVELS: [&str; 4] = ["P1", "P2", "P3", "none"];

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
        let levels = sqlx::query!(
            r#"SELECT coalesce(priority, 'none') AS "priority!",
                      count(*) AS "ready!",
                      coalesce(extract(epoch FROM now() - min(run_after)), 0)::float8
                        AS "oldest_ready_s!"
               FROM jobs WHERE status = 'queued' AND run_after <= now()
               GROUP BY 1"#
        )
        .fetch_all(&self.pool)
        .await?;
        let by_priority = PRIORITY_LEVELS
            .iter()
            .map(|&level| {
                let found = levels.iter().find(|l| l.priority == level);
                PriorityStats {
                    priority: level,
                    ready: found.map_or(0, |l| l.ready),
                    oldest_ready_s: found.map_or(0.0, |l| l.oldest_ready_s),
                }
            })
            .collect();
        Ok(QueueStats {
            ready: row.ready,
            waiting: row.waiting,
            running: row.running,
            oldest_ready_s: row.oldest_ready_s,
            by_priority,
        })
    }
}
