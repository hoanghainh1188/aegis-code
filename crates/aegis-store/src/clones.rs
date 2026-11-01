//! The clone of a repo made in the background (#330): an admin adds or
//! changes a repo from the pages, the clone is made after, and the repo's
//! page says how it went.

use chrono::{DateTime, Utc};

use crate::{RepoId, Store, StoreError};

/// The end of git's output a failed clone keeps, in bytes.
pub const MAX_CLONE_LOG_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneState {
    Cloning,
    Done,
    Failed,
}

impl CloneState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloning => "cloning",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn decode(value: &str) -> Result<Self, StoreError> {
        match value {
            "cloning" => Ok(Self::Cloning),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => Err(StoreError::Decode {
                column: "repo_clones.state",
                value: other.to_owned(),
            }),
        }
    }
}

/// A repo's latest clone.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoClone {
    pub state: CloneState,
    /// The end of git's output, when it failed.
    pub log_tail: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// The last [`MAX_CLONE_LOG_BYTES`] of `log`, cut at a character.
fn tail(log: &str) -> &str {
    let mut start = log.len().saturating_sub(MAX_CLONE_LOG_BYTES);
    while !log.is_char_boundary(start) {
        start += 1;
    }
    &log[start..]
}

impl Store {
    /// `repo`'s clone is being made, in place of what was said before.
    pub async fn start_clone(&self, repo: RepoId) -> Result<(), StoreError> {
        sqlx::query!(
            "INSERT INTO repo_clones (repo_id, state) VALUES ($1, 'cloning')
             ON CONFLICT (repo_id) DO UPDATE
             SET state = 'cloning', log_tail = NULL, started_at = now(), finished_at = NULL",
            repo.0,
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::from_db)?;
        Ok(())
    }

    /// `repo`'s clone is made, or failed with `log`, git's output; only
    /// its end is kept.
    pub async fn finish_clone(
        &self,
        repo: RepoId,
        result: Result<(), &str>,
    ) -> Result<(), StoreError> {
        let (state, log) = match result {
            Ok(()) => (CloneState::Done, None),
            Err(log) => (CloneState::Failed, Some(tail(log))),
        };
        sqlx::query!(
            "UPDATE repo_clones SET state = $2, log_tail = $3, finished_at = now()
             WHERE repo_id = $1",
            repo.0,
            state.as_str(),
            log,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `repo`'s latest clone made from the pages; `None` for one made on
    /// the command line or never.
    pub async fn repo_clone(&self, repo: RepoId) -> Result<Option<RepoClone>, StoreError> {
        let row = sqlx::query!(
            "SELECT state, log_tail, started_at, finished_at FROM repo_clones WHERE repo_id = $1",
            repo.0,
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(RepoClone {
                state: CloneState::decode(&r.state)?,
                log_tail: r.log_tail,
                started_at: r.started_at,
                finished_at: r.finished_at,
            })
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_keeps_the_end_and_whole_characters() {
        let log = format!("{}é{}", "a".repeat(10), "b".repeat(MAX_CLONE_LOG_BYTES - 1));
        let kept = tail(&log);
        assert!(kept.len() <= MAX_CLONE_LOG_BYTES);
        assert!(kept.ends_with('b') && kept.starts_with('b'));
        assert_eq!(tail("short"), "short");
    }
}
