//! Pausing every agent of the instance at once (#510), for an incident
//! that is not one repo's (#374): a model acting strangely, a prompt
//! injection spreading, a provider billing wrong. Two levels:
//!
//! - [`PauseLevel::Intake`]: no queued job is claimed; webhooks, trackers
//!   and Sonar still record jobs, which wait and say why. Running jobs end
//!   as they would.
//! - [`PauseLevel::Emergency`]: that, and every running job is asked to
//!   cancel, as `cancel` asks one (its worker stops at its next lease
//!   renewal).
//!
//! Resumed, the queued jobs are claimed again; none is lost. Both are
//! audited (`instance.pause`, `instance.resume`).

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::audit::{self, Target};
use crate::budget::WAITING;
use crate::tx;
use crate::{Actor, Store, StoreError};

/// How the reason of a job kept waiting by an instance pause begins, after
/// `waiting: `.
pub const INSTANCE_PAUSED: &str = "all agents are paused";
const MAX_REASON_CHARS: usize = 500;

/// How far a pause goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PauseLevel {
    /// No new job is claimed.
    Intake,
    /// No new job is claimed, and the running ones are asked to cancel.
    Emergency,
}

impl PauseLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intake => "intake",
            Self::Emergency => "emergency",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "intake" => Some(Self::Intake),
            "emergency" => Some(Self::Emergency),
            _ => None,
        }
    }
}

/// Who paused the instance, when, how far and why.
#[derive(Debug, Clone, PartialEq)]
pub struct InstancePause {
    pub level: PauseLevel,
    pub at: DateTime<Utc>,
    pub by: String,
    pub reason: Option<String>,
}

impl InstancePause {
    /// Why queued jobs wait.
    pub fn waiting_reason(&self) -> String {
        let said = match &self.reason {
            Some(why) => format!("{INSTANCE_PAUSED} by {} ({why})", self.by),
            None => format!("{INSTANCE_PAUSED} by {}", self.by),
        };
        format!("{WAITING}{said}; jobs run again once they are resumed")
    }
}

/// The instance just paused, and how many running jobs were asked to
/// cancel (none for [`PauseLevel::Intake`]).
#[derive(Debug, Clone, PartialEq)]
pub struct InstancePaused {
    pub pause: InstancePause,
    pub cancelled: u64,
}

struct Row {
    level: String,
    paused_at: DateTime<Utc>,
    paused_by: String,
    reason: Option<String>,
}

impl TryFrom<Row> for InstancePause {
    type Error = StoreError;

    fn try_from(row: Row) -> Result<Self, StoreError> {
        let level = PauseLevel::parse(&row.level).ok_or_else(|| StoreError::Decode {
            column: "instance_pause.level",
            value: row.level.clone(),
        })?;
        Ok(Self {
            level,
            at: row.paused_at,
            by: row.paused_by,
            reason: row.reason,
        })
    }
}

async fn read(conn: &mut PgConnection, lock: bool) -> Result<Option<InstancePause>, StoreError> {
    let row = if lock {
        sqlx::query_as!(
            Row,
            "SELECT level, paused_at, paused_by, reason FROM instance_pause FOR UPDATE",
        )
        .fetch_optional(&mut *conn)
        .await?
    } else {
        sqlx::query_as!(
            Row,
            "SELECT level, paused_at, paused_by, reason FROM instance_pause",
        )
        .fetch_optional(&mut *conn)
        .await?
    };
    row.map(InstancePause::try_from).transpose()
}

/// The pause, if any; its reason written on every queued job that does
/// not say it yet, for jobs recorded since. For the claim, under its lock.
pub(crate) async fn hold_all(conn: &mut PgConnection) -> Result<Option<InstancePause>, StoreError> {
    let Some(pause) = read(conn, false).await? else {
        return Ok(None);
    };
    write_waiting(conn, &pause).await?;
    Ok(Some(pause))
}

async fn write_waiting(conn: &mut PgConnection, pause: &InstancePause) -> Result<(), StoreError> {
    sqlx::query!(
        "UPDATE jobs SET reason = $1 WHERE status = 'queued' AND reason IS DISTINCT FROM $1",
        pause.waiting_reason(),
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn checked(reason: Option<&str>) -> Result<Option<&str>, StoreError> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if let Some(r) = reason
        && r.chars().count() > MAX_REASON_CHARS
    {
        return Err(StoreError::Invalid {
            field: "reason",
            message: format!("at most {MAX_REASON_CHARS} characters"),
        });
    }
    Ok(reason)
}

impl Store {
    /// Who paused the instance and how far; `None` while it runs.
    pub async fn instance_pause(&self) -> Result<Option<InstancePause>, StoreError> {
        let mut conn = self.pool.acquire().await?;
        read(&mut conn, false).await
    }

    /// Pauses every agent at `level`, as `actor`, for `reason` (at most
    /// 500 characters). An intake pause may be raised to an emergency one;
    /// `Conflict` when the instance is paused at `level` or further
    /// already.
    pub async fn pause_instance_by(
        &self,
        level: PauseLevel,
        reason: Option<&str>,
        actor: &Actor,
    ) -> Result<InstancePaused, StoreError> {
        let reason = checked(reason)?;
        let mut tx = self.pool.begin().await?;
        let work = async {
            if let Some(was) = read(&mut tx, true).await?
                && was.level >= level
            {
                return Err(StoreError::Conflict(format!(
                    "all agents are paused already ({}, by {})",
                    was.level.as_str(),
                    was.by
                )));
            }
            let pause: InstancePause = sqlx::query_as!(
                Row,
                "INSERT INTO instance_pause (level, paused_by, reason) VALUES ($1, $2, $3)
                 ON CONFLICT (id) DO UPDATE
                     SET level = EXCLUDED.level, paused_at = now(),
                         paused_by = EXCLUDED.paused_by, reason = EXCLUDED.reason
                 RETURNING level, paused_at, paused_by, reason",
                level.as_str(),
                actor.name,
                reason,
            )
            .fetch_one(&mut *tx)
            .await?
            .try_into()?;
            let cancelled = if level == PauseLevel::Emergency {
                // As `request_cancel` asks a running job: its worker stops
                // it when it next renews the lease.
                sqlx::query!(
                    "UPDATE jobs SET cancel_requested = true, updated_at = now()
                     WHERE status = 'running' AND NOT cancel_requested",
                )
                .execute(&mut *tx)
                .await?
                .rows_affected()
            } else {
                0
            };
            // Said at once, not only when a claim next looks.
            write_waiting(&mut tx, &pause).await?;
            let detail = serde_json::json!({
                "level": level.as_str(),
                "reason": reason,
                "cancelled": cancelled,
            });
            audit::record(&mut tx, actor, "instance.pause", Target::default(), detail).await?;
            Ok::<_, StoreError>(InstancePaused { pause, cancelled })
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Resumes every agent, as `actor`: queued jobs are claimed again.
    /// `Conflict` when the instance is not paused.
    pub async fn resume_instance_by(&self, actor: &Actor) -> Result<InstancePause, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let was = read(&mut tx, true)
                .await?
                .ok_or_else(|| StoreError::Conflict("the agents are not paused".into()))?;
            sqlx::query!("DELETE FROM instance_pause")
                .execute(&mut *tx)
                .await?;
            sqlx::query!(
                "UPDATE jobs SET reason = NULL
                 WHERE status = 'queued' AND starts_with(reason, $1)",
                format!("{WAITING}{INSTANCE_PAUSED}"),
            )
            .execute(&mut *tx)
            .await?;
            let detail = serde_json::json!({ "level": was.level.as_str() });
            audit::record(&mut tx, actor, "instance.resume", Target::default(), detail).await?;
            Ok::<_, StoreError>(was)
        }
        .await;
        tx::commit(tx, work).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_round_trip_and_order() {
        for level in [PauseLevel::Intake, PauseLevel::Emergency] {
            assert_eq!(PauseLevel::parse(level.as_str()), Some(level));
        }
        assert_eq!(PauseLevel::parse("stop"), None);
        assert!(PauseLevel::Intake < PauseLevel::Emergency);
    }

    #[test]
    fn a_waiting_job_says_who_paused_the_agents_and_why() {
        let pause = InstancePause {
            level: PauseLevel::Intake,
            at: Utc::now(),
            by: "admin@x.test".into(),
            reason: Some("model acts strangely".into()),
        };
        assert_eq!(
            pause.waiting_reason(),
            "waiting: all agents are paused by admin@x.test (model acts strangely); \
             jobs run again once they are resumed"
        );
    }

    #[test]
    fn reasons_are_trimmed_and_capped() {
        assert_eq!(checked(Some("  ")).unwrap(), None);
        assert_eq!(checked(Some(" why ")).unwrap(), Some("why"));
        assert!(checked(Some(&"x".repeat(501))).is_err());
    }
}
