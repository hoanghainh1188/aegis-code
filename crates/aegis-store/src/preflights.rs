//! The pre-flight check of a repo run from its page (#599): one row per
//! repo, its latest check. Items are appended as the checks give them; the
//! check ends `passed`, `warned` (no blocking failure, some warnings) or
//! `failed` (a blocking failure, or the check could not run).

use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::audit::{self, Actor, Target};
use crate::tx;
use crate::{RepoId, Store, StoreError};

/// Items one check keeps: the CLI's report has about fifteen.
pub const MAX_PREFLIGHT_ITEMS: usize = 64;
/// A check still `running` this long after it started died with its
/// server, and another may start.
pub const PREFLIGHT_STALE_AFTER: time::Duration = time::Duration::hours(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightState {
    Running,
    Passed,
    Warned,
    Failed,
}

impl PreflightState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Warned => "warned",
            Self::Failed => "failed",
        }
    }

    fn decode(value: &str) -> Result<Self, StoreError> {
        match value {
            "running" => Ok(Self::Running),
            "passed" => Ok(Self::Passed),
            "warned" => Ok(Self::Warned),
            "failed" => Ok(Self::Failed),
            other => Err(StoreError::Decode {
                column: "repo_preflights.state",
                value: other.to_owned(),
            }),
        }
    }

    /// Whether the repo may be turned on after it.
    pub fn ready(self) -> bool {
        matches!(self, Self::Passed | Self::Warned)
    }
}

/// One item of a check, as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreflightItem {
    pub category: String,
    pub name: String,
    /// `pass`, `fail`, `warn` or `skip`.
    pub status: String,
    pub detail: String,
    pub is_blocking: bool,
}

/// A repo's latest check.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoPreflight {
    pub state: PreflightState,
    pub items: Vec<PreflightItem>,
    pub started_by: String,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

impl Store {
    /// Starts a check of `repo` by `actor`, in place of the last one, and
    /// audits it. `false` when one is running (and not stale): the page
    /// shows that one.
    pub async fn start_preflight_by(
        &self,
        repo: RepoId,
        actor: &Actor,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let started = sqlx::query_scalar!(
                "INSERT INTO repo_preflights (repo_id, state, started_by)
                 VALUES ($1, 'running', $2)
                 ON CONFLICT (repo_id) DO UPDATE
                 SET state = 'running', items = '[]'::jsonb, started_by = EXCLUDED.started_by,
                     started_at = now(), finished_at = NULL
                 WHERE repo_preflights.state <> 'running'
                    OR repo_preflights.started_at < now() - make_interval(secs => $3)
                 RETURNING repo_id",
                repo.0,
                actor.name,
                PREFLIGHT_STALE_AFTER.as_seconds_f64(),
            )
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::from_db)?;
            if started.is_none() {
                return Ok(tx::Done::Discard(false));
            }
            audit::record(
                &mut tx,
                actor,
                "repo.preflight",
                Target {
                    repo: Some(repo),
                    job: None,
                },
                json!({}),
            )
            .await?;
            Ok::<_, StoreError>(tx::Done::Commit(true))
        }
        .await;
        tx::end(tx, work).await
    }

    /// Appends `item` to `repo`'s running check; past
    /// [`MAX_PREFLIGHT_ITEMS`] the rest are dropped.
    pub async fn add_preflight_item(
        &self,
        repo: RepoId,
        item: &PreflightItem,
    ) -> Result<(), StoreError> {
        let value: Value = serde_json::to_value(item).map_err(|e| StoreError::Invalid {
            field: "item",
            message: e.to_string(),
        })?;
        sqlx::query!(
            "UPDATE repo_preflights SET items = items || jsonb_build_array($2::jsonb)
              WHERE repo_id = $1 AND state = 'running' AND jsonb_array_length(items) < $3",
            repo.0,
            value,
            MAX_PREFLIGHT_ITEMS as i32,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ends `repo`'s running check as `state`.
    pub async fn finish_preflight(
        &self,
        repo: RepoId,
        state: PreflightState,
    ) -> Result<(), StoreError> {
        if state == PreflightState::Running {
            return Err(StoreError::Invalid {
                field: "state",
                message: "a check ends passed, warned or failed".into(),
            });
        }
        sqlx::query!(
            "UPDATE repo_preflights SET state = $2, finished_at = now()
              WHERE repo_id = $1 AND state = 'running'",
            repo.0,
            state.as_str(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `repo`'s latest check, if it had one.
    pub async fn repo_preflight(&self, repo: RepoId) -> Result<Option<RepoPreflight>, StoreError> {
        let Some(row) = sqlx::query!(
            "SELECT state, items, started_by, started_at, finished_at
               FROM repo_preflights WHERE repo_id = $1",
            repo.0,
        )
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let items: Vec<PreflightItem> =
            serde_json::from_value(row.items).map_err(|e| StoreError::Decode {
                column: "repo_preflights.items",
                value: e.to_string(),
            })?;
        Ok(Some(RepoPreflight {
            state: PreflightState::decode(&row.state)?,
            items,
            started_by: row.started_by,
            started_at: row.started_at,
            finished_at: row.finished_at,
        }))
    }
}
