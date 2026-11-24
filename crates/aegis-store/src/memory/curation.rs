//! Curation of a repo's memory (#658): the memories each job was given,
//! kept for its page, and a memory a maintainer marked obsolete there,
//! which recall leaves out until its text changes.

use serde_json::json;
use time::OffsetDateTime;

use super::{MemoryId, MemoryRow, RepoMemory};
use crate::audit::{self, Actor, Target};
use crate::tx;
use crate::{JobId, RepoId, Store, StoreError};

/// One memory a job was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecalledMemory {
    pub memory: RepoMemory,
    /// Its place among the job's memories, from 1.
    pub rank: i32,
    /// When, and by whom, it was marked obsolete.
    pub obsolete: Option<(OffsetDateTime, String)>,
}

impl Store {
    /// Keeps `memories`, in order, as what `job` was given; a job given
    /// memories again (a retry) keeps the last ones.
    pub async fn record_recalled_memories(
        &self,
        job: JobId,
        memories: &[MemoryId],
    ) -> Result<(), StoreError> {
        let ids: Vec<i64> = memories.iter().map(|m| m.0).collect();
        let mut tx = self.pool.begin().await?;
        let work = async {
            sqlx::query!("DELETE FROM job_recalled_memories WHERE job_id = $1", job.0)
                .execute(&mut *tx)
                .await?;
            sqlx::query!(
                "INSERT INTO job_recalled_memories (job_id, memory_id, rank)
                 SELECT $1, id, rank::int
                   FROM unnest($2::bigint[]) WITH ORDINALITY AS given (id, rank)
                  WHERE EXISTS (SELECT 1 FROM repo_memories m WHERE m.id = given.id)
                 ON CONFLICT DO NOTHING",
                job.0,
                &ids,
            )
            .execute(&mut *tx)
            .await?;
            Ok::<_, StoreError>(())
        }
        .await;
        tx::commit(tx, work).await
    }

    /// The memories `job` was given, in their order, those since deleted
    /// left out.
    pub async fn recalled_memories(&self, job: JobId) -> Result<Vec<RecalledMemory>, StoreError> {
        let rows = sqlx::query!(
            "SELECT m.id, m.repo_id, m.kind, m.source_ref, m.title, m.summary, m.paths,
                    m.keywords, m.job_id, m.scope, m.created_at, m.updated_at,
                    m.obsolete_at, m.obsolete_by, r.rank
               FROM job_recalled_memories r
               JOIN repo_memories m ON m.id = r.memory_id
              WHERE r.job_id = $1
              ORDER BY r.rank",
            job.0,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                let memory = RepoMemory::try_from(MemoryRow {
                    id: r.id,
                    repo_id: r.repo_id,
                    kind: r.kind,
                    source_ref: r.source_ref,
                    title: r.title,
                    summary: r.summary,
                    paths: r.paths,
                    keywords: r.keywords,
                    job_id: r.job_id,
                    scope: r.scope,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                })?;
                Ok(RecalledMemory {
                    memory,
                    rank: r.rank,
                    obsolete: r
                        .obsolete_at
                        .map(|at| (at, r.obsolete_by.unwrap_or_default())),
                })
            })
            .collect()
    }

    /// Marks `repo`'s memory `id` obsolete by `actor`, audited as
    /// `memory.obsolete`: recall leaves it out until its text changes.
    /// `false` when `repo` has no such memory, or it is obsolete already.
    pub async fn mark_memory_obsolete_by(
        &self,
        repo: RepoId,
        id: MemoryId,
        actor: &Actor,
        job: Option<JobId>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let Some(row) = sqlx::query!(
                "UPDATE repo_memories
                    SET obsolete_at = now(), obsolete_by = $3
                  WHERE id = $1 AND repo_id = $2 AND obsolete_at IS NULL
              RETURNING kind, source_ref, scope",
                id.0,
                repo.0,
                actor.name,
            )
            .fetch_optional(&mut *tx)
            .await?
            else {
                return Ok(tx::Done::Discard(false));
            };
            // The audit's job is the one whose page it was marked from,
            // when that job is of the memory's repo.
            let job = match job {
                Some(j) => sqlx::query_scalar!(
                    "SELECT id FROM jobs WHERE id = $1 AND repo_id = $2",
                    j.0,
                    repo.0
                )
                .fetch_optional(&mut *tx)
                .await?
                .map(JobId),
                None => None,
            };
            audit::record(
                &mut tx,
                actor,
                "memory.obsolete",
                Target {
                    repo: Some(repo),
                    job,
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
}
