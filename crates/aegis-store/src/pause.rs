//! Pausing a repo (#374), for when it runs wrong or spends too fast. While
//! paused, its queued jobs are not claimed but wait, saying who paused it
//! and why, as jobs of a repo whose budget is spent do (#31); webhooks,
//! trackers and Sonar still record jobs, and a running job ends as it
//! would. Its queued jobs may be cancelled when it is paused instead.
//! Resumed, they are claimed again. Both are audited.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::audit::{self, Target};
use crate::budget::WAITING;
use crate::tx;
use crate::{Actor, RepoId, Store, StoreError};

/// How the reason of a job kept waiting, or cancelled, for a paused repo
/// begins, after `waiting: ` for one that waits.
pub const REPO_PAUSED: &str = "the repo is paused";
const MAX_REASON_CHARS: usize = 500;

/// Who paused a repo, when and why.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoPause {
    pub repo_id: RepoId,
    pub at: DateTime<Utc>,
    pub by: String,
    pub reason: Option<String>,
}

impl RepoPause {
    fn said(&self) -> String {
        match &self.reason {
            Some(why) => format!("{REPO_PAUSED} by {} ({why})", self.by),
            None => format!("{REPO_PAUSED} by {}", self.by),
        }
    }

    /// Why its queued jobs wait.
    pub fn waiting_reason(&self) -> String {
        format!(
            "{WAITING}{}; its jobs run again once it is resumed",
            self.said()
        )
    }

    fn cancel_reason(&self) -> String {
        format!("cancelled: {}", self.said())
    }
}

/// A repo just paused, and how many of its queued jobs were cancelled.
#[derive(Debug, Clone, PartialEq)]
pub struct Paused {
    pub pause: RepoPause,
    pub cancelled: u64,
}

struct PauseRow {
    id: i64,
    paused_at: Option<DateTime<Utc>>,
    paused_by: Option<String>,
    pause_reason: Option<String>,
}

impl PauseRow {
    fn pause(self) -> Option<RepoPause> {
        Some(RepoPause {
            repo_id: RepoId(self.id),
            at: self.paused_at?,
            by: self.paused_by.unwrap_or_default(),
            reason: self.pause_reason,
        })
    }
}

/// The paused repos; their queued jobs say why they wait. For the claim,
/// under its lock.
pub(crate) async fn hold_paused(conn: &mut PgConnection) -> Result<Vec<i64>, StoreError> {
    let rows = sqlx::query_as!(
        PauseRow,
        "SELECT id, paused_at, paused_by, pause_reason FROM repos WHERE paused_at IS NOT NULL",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut held = Vec::new();
    for pause in rows.into_iter().filter_map(PauseRow::pause) {
        sqlx::query!(
            "UPDATE jobs SET reason = $2
             WHERE repo_id = $1 AND status = 'queued' AND reason IS DISTINCT FROM $2",
            pause.repo_id.0,
            pause.waiting_reason(),
        )
        .execute(&mut *conn)
        .await?;
        held.push(pause.repo_id.0);
    }
    Ok(held)
}

impl Store {
    /// Who paused `repo` and why; `None` when it runs.
    pub async fn repo_pause(&self, repo: RepoId) -> Result<Option<RepoPause>, StoreError> {
        Ok(sqlx::query_as!(
            PauseRow,
            "SELECT id, paused_at, paused_by, pause_reason FROM repos WHERE id = $1",
            repo.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .and_then(PauseRow::pause))
    }

    /// Every paused repo, by id.
    pub async fn paused_repos(&self) -> Result<Vec<RepoPause>, StoreError> {
        let rows = sqlx::query_as!(
            PauseRow,
            "SELECT id, paused_at, paused_by, pause_reason FROM repos
             WHERE paused_at IS NOT NULL ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(PauseRow::pause).collect())
    }

    /// Pauses `repo`, as `actor`, for `reason` (at most 500 characters),
    /// cancelling its queued jobs when `cancel_queued`. `Conflict` when it
    /// is paused already, `NotFound` when there is no such repo.
    pub async fn pause_repo_by(
        &self,
        repo: RepoId,
        reason: Option<&str>,
        cancel_queued: bool,
        actor: &Actor,
    ) -> Result<Paused, StoreError> {
        let reason = reason.map(str::trim).filter(|r| !r.is_empty());
        if let Some(r) = reason
            && r.chars().count() > MAX_REASON_CHARS
        {
            return Err(StoreError::Invalid {
                field: "reason",
                message: format!("at most {MAX_REASON_CHARS} characters"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let was = sqlx::query!(
                "SELECT slug, paused_at FROM repos WHERE id = $1 FOR UPDATE",
                repo.0,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("repo {}", repo.0)))?;
            if was.paused_at.is_some() {
                return Err(StoreError::Conflict(format!(
                    "repo `{}` is paused already",
                    was.slug
                )));
            }
            let pause = sqlx::query_as!(
                PauseRow,
                "UPDATE repos SET paused_at = now(), paused_by = $2, pause_reason = $3
                 WHERE id = $1
                 RETURNING id, paused_at, paused_by, pause_reason",
                repo.0,
                actor.name,
                reason,
            )
            .fetch_one(&mut *tx)
            .await?
            .pause()
            .ok_or_else(|| StoreError::Conflict("the pause was not kept".into()))?;
            let cancelled = if cancel_queued {
                sqlx::query!(
                    "UPDATE jobs SET status = 'cancelled', cancel_requested = true,
                         reason = $2, updated_at = now()
                     WHERE repo_id = $1 AND status = 'queued'",
                    repo.0,
                    pause.cancel_reason(),
                )
                .execute(&mut *tx)
                .await?
                .rows_affected()
            } else {
                // Said at once, not only when a claim next looks (#328
                // drops what a claim that takes nothing wrote).
                sqlx::query!(
                    "UPDATE jobs SET reason = $2 WHERE repo_id = $1 AND status = 'queued'",
                    repo.0,
                    pause.waiting_reason(),
                )
                .execute(&mut *tx)
                .await?;
                0
            };
            let target = Target {
                repo: Some(repo),
                job: None,
            };
            let detail = serde_json::json!({ "reason": reason, "cancelled": cancelled });
            audit::record(&mut tx, actor, "repo.pause", target, detail).await?;
            Ok::<_, StoreError>(Paused { pause, cancelled })
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Resumes `repo`, as `actor`: its queued jobs are claimed again.
    /// `Conflict` when it is not paused, `NotFound` when there is no such
    /// repo.
    pub async fn resume_repo_by(&self, repo: RepoId, actor: &Actor) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let was = sqlx::query!(
                "SELECT slug, paused_at FROM repos WHERE id = $1 FOR UPDATE",
                repo.0,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("repo {}", repo.0)))?;
            if was.paused_at.is_none() {
                return Err(StoreError::Conflict(format!(
                    "repo `{}` is not paused",
                    was.slug
                )));
            }
            sqlx::query!(
                "UPDATE repos SET paused_at = NULL, paused_by = NULL, pause_reason = NULL
                 WHERE id = $1",
                repo.0,
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                "UPDATE jobs SET reason = NULL
                 WHERE repo_id = $1 AND status = 'queued' AND starts_with(reason, $2)",
                repo.0,
                format!("{WAITING}{REPO_PAUSED}"),
            )
            .execute(&mut *tx)
            .await?;
            let target = Target {
                repo: Some(repo),
                job: None,
            };
            audit::record(&mut tx, actor, "repo.resume", target, serde_json::json!({})).await?;
            Ok::<_, StoreError>(())
        }
        .await;
        tx::commit(tx, work).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_job_says_who_paused_the_repo_and_why() {
        let pause = RepoPause {
            repo_id: RepoId(1),
            at: Utc::now(),
            by: "admin@x.test".into(),
            reason: Some("runs wild".into()),
        };
        assert_eq!(
            pause.waiting_reason(),
            "waiting: the repo is paused by admin@x.test (runs wild); \
             its jobs run again once it is resumed"
        );
        let silent = RepoPause {
            reason: None,
            ..pause
        };
        assert_eq!(
            silent.cancel_reason(),
            "cancelled: the repo is paused by admin@x.test"
        );
    }
}
