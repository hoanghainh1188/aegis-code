//! How a transaction ends (#328). Dropping a [`Transaction`] only queues
//! its ROLLBACK, which the pool sends later from a task of its own; until
//! then its row locks stay, and `claim_job` (`SKIP LOCKED`) skips a job a
//! refused call had locked. So every transaction ends here: committed, or
//! rolled back and waited for before the call returns.

use sqlx::{Postgres, Transaction};

use crate::StoreError;

/// What a transaction's work gives back, and whether to keep its writes.
pub(crate) enum Done<T> {
    Commit(T),
    /// The work's answer stands, its writes do not.
    Discard(T),
}

/// Ends `tx` as `work` says. On an error the rollback runs before the
/// error is returned; should the rollback fail too, the connection is
/// broken, the pool closes it and the server drops its locks.
pub(crate) async fn end<T>(
    tx: Transaction<'_, Postgres>,
    work: Result<Done<T>, StoreError>,
) -> Result<T, StoreError> {
    match work {
        Ok(Done::Commit(value)) => {
            tx.commit().await?;
            Ok(value)
        }
        Ok(Done::Discard(value)) => {
            tx.rollback().await?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback) = tx.rollback().await {
                tracing::warn!(%error, %rollback, "rollback after a store error failed");
            }
            Err(error)
        }
    }
}

/// [`end`] for work that keeps its writes whenever it succeeds.
pub(crate) async fn commit<T>(
    tx: Transaction<'_, Postgres>,
    work: Result<T, StoreError>,
) -> Result<T, StoreError> {
    end(tx, work.map(Done::Commit)).await
}
