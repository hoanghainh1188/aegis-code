//! Polls on a schedule kept in the database (ADR 0003 §4): Jira and
//! Backlog filters (#26). Whichever process moves a poll's `next_at` on
//! polls; the others, finding it in the future, do not. Nothing is held
//! while it polls, so a process that dies mid-poll only delays the next.

use std::time::Duration;

use crate::{Store, StoreError};

impl Store {
    /// Whether this caller polls `name` now: true for one caller per
    /// `every`, the first to ask once it is due. The next is due `every`
    /// from now.
    pub async fn claim_poll(&self, name: &str, every: Duration) -> Result<bool, StoreError> {
        let claimed = sqlx::query_scalar!(
            "INSERT INTO polls (name, next_at) VALUES ($1, now() + make_interval(secs => $2))
             ON CONFLICT (name) DO UPDATE SET next_at = EXCLUDED.next_at
             WHERE polls.next_at <= now()
             RETURNING name",
            name,
            every.as_secs_f64(),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(claimed.is_some())
    }
}
