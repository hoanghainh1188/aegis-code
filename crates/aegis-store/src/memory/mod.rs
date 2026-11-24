//! Per-repository Incident & Domain Memory (#456): merged PR summaries,
//! review lessons from `/aegis fix` rounds, operational runbooks, and
//! business rules. Recall is in [`recall`] (#596).

mod recall;
mod vector;

pub use recall::{RecallScores, fuse};
pub(crate) use vector::VectorIndexes;
pub use vector::{EmbeddingSpace, MemoryToEmbed, NearQuery};

use serde_json::json;
use time::OffsetDateTime;

use crate::audit::{self, Actor, Target};
use crate::tx;
use crate::{JobId, Page, PageOf, RepoId, Store, StoreError};

const MAX_SOURCE_REF_CHARS: usize = 256;
const MAX_TITLE_CHARS: usize = 500;
const MAX_SUMMARY_CHARS: usize = 4000;
const MAX_KEYWORDS_CHARS: usize = 2000;

/// Primary key of a row in `repo_memories`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemoryId(pub i64);

/// Category of repository memory stored in `repo_memories.kind` (#456).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryKind {
    /// Postmortem / fix summary extracted after an agent PR is merged.
    MergedPr,
    /// Lesson extracted from `/aegis fix` review feedback rounds.
    ReviewLesson,
    /// Operational runbook for recurring incident classes.
    Runbook,
    /// Repository or domain business rule / invariant.
    BusinessRule,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MergedPr => "merged_pr",
            Self::ReviewLesson => "review_lesson",
            Self::Runbook => "runbook",
            Self::BusinessRule => "business_rule",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "merged_pr" => Some(Self::MergedPr),
            "review_lesson" => Some(Self::ReviewLesson),
            "runbook" => Some(Self::Runbook),
            "business_rule" => Some(Self::BusinessRule),
            _ => None,
        }
    }

    pub(crate) fn decode(column: &'static str, text: &str) -> Result<Self, StoreError> {
        Self::parse(text).ok_or_else(|| StoreError::Decode {
            column,
            value: text.to_owned(),
        })
    }
}

/// Which repos recall a memory (#596).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryScope {
    /// Its own repo only.
    Repo,
    /// Every repo of the tenant its repo belongs to: runbooks and business
    /// rules shared by the services of one customer's system.
    Tenant,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Tenant => "tenant",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "repo" => Some(Self::Repo),
            "tenant" => Some(Self::Tenant),
            _ => None,
        }
    }

    pub(crate) fn decode(column: &'static str, text: &str) -> Result<Self, StoreError> {
        Self::parse(text).ok_or_else(|| StoreError::Decode {
            column,
            value: text.to_owned(),
        })
    }
}

/// Input to create or update a repository memory entry (#456).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRepoMemory {
    pub repo_id: RepoId,
    pub kind: MemoryKind,
    /// Stable source identifier within `(repo_id, kind)`, such as a PR URL,
    /// review job reference, runbook slug, or business rule key.
    pub source_ref: String,
    pub title: String,
    pub summary: String,
    pub paths: Vec<String>,
    pub keywords: String,
    pub job_id: Option<JobId>,
    /// `None`: a new entry is [`MemoryScope::Repo`], an existing one keeps
    /// its scope (a merged PR extracted again stays shared).
    pub scope: Option<MemoryScope>,
}

/// Stored row in `repo_memories` (#456).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMemory {
    pub id: MemoryId,
    pub repo_id: RepoId,
    pub kind: MemoryKind,
    pub source_ref: String,
    pub title: String,
    pub summary: String,
    pub paths: Vec<String>,
    pub keywords: String,
    pub job_id: Option<JobId>,
    pub scope: MemoryScope,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

struct MemoryRow {
    id: i64,
    repo_id: i64,
    kind: String,
    source_ref: String,
    title: String,
    summary: String,
    paths: Vec<String>,
    keywords: String,
    job_id: Option<i64>,
    scope: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<MemoryRow> for RepoMemory {
    type Error = StoreError;

    fn try_from(row: MemoryRow) -> Result<Self, StoreError> {
        Ok(RepoMemory {
            id: MemoryId(row.id),
            repo_id: RepoId(row.repo_id),
            kind: MemoryKind::decode("repo_memories.kind", &row.kind)?,
            source_ref: row.source_ref,
            title: row.title,
            summary: row.summary,
            paths: row.paths,
            keywords: row.keywords,
            job_id: row.job_id.map(JobId),
            scope: MemoryScope::decode("repo_memories.scope", &row.scope)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn validate_non_empty(
    field: &'static str,
    raw: &str,
    max_chars: usize,
) -> Result<String, StoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StoreError::Invalid {
            field,
            message: "must not be empty".into(),
        });
    }
    let count = trimmed.chars().count();
    if count > max_chars {
        return Err(StoreError::Invalid {
            field,
            message: format!("{count} chars exceeds max {max_chars}"),
        });
    }
    Ok(trimmed.to_owned())
}

impl Store {
    /// Creates or updates a repository memory entry as [`Actor::system`].
    pub async fn upsert_repo_memory(
        &self,
        entry: &NewRepoMemory,
    ) -> Result<RepoMemory, StoreError> {
        self.upsert_repo_memory_by(entry, &Actor::system()).await
    }

    /// Creates or updates a repository memory entry idempotently on
    /// `(repo_id, kind, source_ref)` and records the write in `audit_log`.
    pub async fn upsert_repo_memory_by(
        &self,
        entry: &NewRepoMemory,
        actor: &Actor,
    ) -> Result<RepoMemory, StoreError> {
        let source_ref = validate_non_empty("source_ref", &entry.source_ref, MAX_SOURCE_REF_CHARS)?;
        let title = validate_non_empty("title", &entry.title, MAX_TITLE_CHARS)?;
        let summary = validate_non_empty("summary", &entry.summary, MAX_SUMMARY_CHARS)?;
        let keywords: String = entry
            .keywords
            .trim()
            .chars()
            .take(MAX_KEYWORDS_CHARS)
            .collect();
        let paths: Vec<String> = entry
            .paths
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        let mut tx = self.pool.begin().await?;
        let work = async {
            // A memory names only a job of its own repo (#525: never another
            // tenant's).
            if let Some(job) = entry.job_id {
                let owner = sqlx::query_scalar!("SELECT repo_id FROM jobs WHERE id = $1", job.0)
                    .fetch_optional(&mut *tx)
                    .await?;
                if owner != Some(entry.repo_id.0) {
                    return Err(StoreError::Invalid {
                        field: "job_id",
                        message: format!("job {} is not a job of repo {}", job.0, entry.repo_id.0),
                    });
                }
            }
            let saved: RepoMemory = sqlx::query_as!(
                MemoryRow,
                "INSERT INTO repo_memories (
                     repo_id, kind, source_ref, title, summary, paths, keywords, job_id,
                     scope
                 )
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, coalesce($9, 'repo'))
                 ON CONFLICT (repo_id, kind, source_ref) DO UPDATE SET
                     scope = coalesce($9, repo_memories.scope),
                     title = EXCLUDED.title,
                     summary = EXCLUDED.summary,
                     paths = EXCLUDED.paths,
                     keywords = EXCLUDED.keywords,
                     job_id = COALESCE(EXCLUDED.job_id, repo_memories.job_id),
                     updated_at = now()
                 RETURNING id, repo_id, kind, source_ref, title, summary, paths,
                           keywords, job_id, scope, created_at, updated_at",
                entry.repo_id.0,
                entry.kind.as_str(),
                source_ref,
                title,
                summary,
                &paths,
                keywords,
                entry.job_id.map(|j| j.0),
                entry.scope.map(MemoryScope::as_str) as Option<&str>,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?
            .try_into()?;
            // Its text may have changed: embedded again before its next
            // recall (#655).
            sqlx::query!(
                "DELETE FROM repo_memory_embeddings WHERE memory_id = $1",
                saved.id.0
            )
            .execute(&mut *tx)
            .await?;
            audit::record(
                &mut tx,
                actor,
                "memory.upsert",
                Target {
                    repo: Some(entry.repo_id),
                    job: entry.job_id,
                },
                json!({
                    "memory_id": saved.id.0,
                    "kind": saved.kind.as_str(),
                    "source_ref": saved.source_ref,
                    "title": saved.title,
                    "scope": saved.scope.as_str(),
                }),
            )
            .await?;
            Ok::<_, StoreError>(saved)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Lists memories for `repo_id`, newest first (`id DESC`), optionally
    /// filtered by `kind`.
    pub async fn list_repo_memories(
        &self,
        repo_id: RepoId,
        kind: Option<MemoryKind>,
        page: Page,
    ) -> Result<PageOf<RepoMemory>, StoreError> {
        let rows = sqlx::query_as!(
            MemoryRow,
            "SELECT id, repo_id, kind, source_ref, title, summary, paths,
                    keywords, job_id, scope, created_at, updated_at
               FROM repo_memories
              WHERE repo_id = $1
                AND ($2::text IS NULL OR kind = $2)
                AND ($3::bigint IS NULL OR id < $3)
              ORDER BY id DESC
              LIMIT $4",
            repo_id.0,
            kind.map(MemoryKind::as_str),
            page.cursor(),
            page.fetch(),
        )
        .fetch_all(&self.pool)
        .await?;

        let items = rows
            .into_iter()
            .map(RepoMemory::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PageOf::new(items, page, |m| m.id.0))
    }

    /// Deletes memory `id` belonging to `repo_id` and records `memory.delete`.
    /// Returns `false` if no such row existed in `repo_id`.
    pub async fn delete_repo_memory_by(
        &self,
        repo_id: RepoId,
        id: MemoryId,
        actor: &Actor,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let Some(row) = sqlx::query!(
                "DELETE FROM repo_memories
                  WHERE id = $1 AND repo_id = $2
              RETURNING kind, source_ref, scope",
                id.0,
                repo_id.0,
            )
            .fetch_optional(&mut *tx)
            .await?
            else {
                return Ok(tx::Done::Discard(false));
            };
            audit::record(
                &mut tx,
                actor,
                "memory.delete",
                Target {
                    repo: Some(repo_id),
                    job: None,
                },
                json!({
                    "memory_id": id.0,
                    "kind": row.kind,
                    "source_ref": row.source_ref,
                    "scope": row.scope,
                }),
            )
            .await?;
            Ok::<_, StoreError>(tx::Done::Commit(true))
        }
        .await;
        tx::end(tx, work).await
    }

    /// The scope of memory `id` of `repo_id`, `None` when it has no such
    /// memory: deleting a shared one takes a tenant's admin (#596).
    pub async fn repo_memory_scope(
        &self,
        repo_id: RepoId,
        id: MemoryId,
    ) -> Result<Option<MemoryScope>, StoreError> {
        let scope = sqlx::query_scalar!(
            "SELECT scope FROM repo_memories WHERE id = $1 AND repo_id = $2",
            id.0,
            repo_id.0,
        )
        .fetch_optional(&self.pool)
        .await?;
        scope
            .map(|s| MemoryScope::decode("repo_memories.scope", &s))
            .transpose()
    }

    /// The scope of `repo_id`'s memory `(kind, source_ref)`, `None` when it
    /// has none: an upsert that would change a shared one takes a tenant's
    /// admin (#596).
    pub async fn repo_memory_scope_of(
        &self,
        repo_id: RepoId,
        kind: MemoryKind,
        source_ref: &str,
    ) -> Result<Option<MemoryScope>, StoreError> {
        let scope = sqlx::query_scalar!(
            "SELECT scope FROM repo_memories
              WHERE repo_id = $1 AND kind = $2 AND source_ref = $3",
            repo_id.0,
            kind.as_str(),
            source_ref.trim(),
        )
        .fetch_optional(&self.pool)
        .await?;
        scope
            .map(|s| MemoryScope::decode("repo_memories.scope", &s))
            .transpose()
    }
}
