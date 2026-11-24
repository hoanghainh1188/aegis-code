//! The semantic channel of memory recall (#655): memories' embeddings, kept
//! as `real[]` in `repo_memory_embeddings`, and the nearest of them to an
//! issue's embedding through pgvector, where the extension is installed.

use std::collections::BTreeMap;
use std::sync::Mutex;

use sqlx::AssertSqlSafe;

use super::MemoryId;
use crate::{RepoId, Store, StoreError};

/// Nearest memories the vector channel gives the fusion, as the other
/// channels do.
pub(super) const VECTOR_DEPTH: i64 = 50;

/// An embedding model and the length of its vectors: vectors of two
/// spaces are never compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingSpace<'a> {
    pub model: &'a str,
    pub dimensions: u32,
}

impl EmbeddingSpace<'_> {
    fn check(&self, vector: &[f32]) -> Result<(), StoreError> {
        if self.dimensions == 0 || self.dimensions > 2_000 {
            return Err(StoreError::Invalid {
                field: "dimensions",
                message: "must be 1 to 2000".into(),
            });
        }
        if vector.len() != self.dimensions as usize {
            return Err(StoreError::Invalid {
                field: "embedding",
                message: format!("{} values for {} dimensions", vector.len(), self.dimensions),
            });
        }
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(StoreError::Invalid {
                field: "embedding",
                message: "not a finite number".into(),
            });
        }
        Ok(())
    }
}

/// An issue's embedding to search with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NearQuery<'a> {
    pub space: EmbeddingSpace<'a>,
    pub vector: &'a [f32],
    /// Cosine similarity a memory needs, 0 to 1.
    pub min_similarity: f32,
}

/// Per store: for each number of dimensions, whether pgvector searches it
/// (the extension is there, and its HNSW index made). Read once: an
/// extension installed later counts from the next start.
#[derive(Debug, Default)]
pub(crate) struct VectorIndexes {
    ready: Mutex<BTreeMap<u32, bool>>,
    /// pgvector 0.8 or later: its HNSW scan goes on past filtered rows.
    iterative: Mutex<Option<bool>>,
}

/// Whether pgvector `version` (`0.8.0`) has iterative index scans.
fn iterative_scans(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    (major, minor) >= (0, 8)
}

/// A memory that needs an embedding in some space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryToEmbed {
    pub id: MemoryId,
    /// Title, keywords and summary, one per line.
    pub text: String,
}

impl Store {
    /// Up to `limit` memories `repo_id` may recall (its own and its
    /// tenant's shared ones) that have no embedding in `space`, newest
    /// first: embedded before a recall, so memories written before the
    /// model was set, or after it changed, catch up.
    pub async fn memories_to_embed(
        &self,
        repo_id: RepoId,
        space: EmbeddingSpace<'_>,
        limit: i64,
    ) -> Result<Vec<MemoryToEmbed>, StoreError> {
        let rows = sqlx::query!(
            r#"WITH tenant_repos AS (
                   SELECT r.id
                     FROM repos r
                     JOIN repos me ON me.tenant_id = r.tenant_id
                    WHERE me.id = $1
               )
               SELECT m.id, m.title, m.keywords, m.summary
                 FROM repo_memories m
                 LEFT JOIN repo_memory_embeddings e
                        ON e.memory_id = m.id AND e.model = $2 AND e.dimensions = $3
                WHERE (m.repo_id = $1
                       OR (m.scope = 'tenant' AND m.repo_id IN (SELECT id FROM tenant_repos)))
                  AND e.memory_id IS NULL
                ORDER BY m.updated_at DESC, m.id DESC
                LIMIT $4"#,
            repo_id.0,
            space.model,
            space.dimensions as i32,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| MemoryToEmbed {
                id: MemoryId(r.id),
                text: [r.title, r.keywords, r.summary]
                    .into_iter()
                    .filter(|t| !t.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
            .collect())
    }

    /// Stores the embeddings of memories in `space`, replacing any other.
    /// A memory deleted meanwhile is skipped.
    pub async fn save_memory_embeddings(
        &self,
        space: EmbeddingSpace<'_>,
        embeddings: &[(MemoryId, Vec<f32>)],
    ) -> Result<(), StoreError> {
        for (_, vector) in embeddings {
            space.check(vector)?;
        }
        let mut tx = self.pool.begin().await?;
        for (id, vector) in embeddings {
            sqlx::query!(
                "INSERT INTO repo_memory_embeddings (memory_id, model, dimensions, embedding)
                 SELECT id, $2, $3, $4 FROM repo_memories WHERE id = $1
                 ON CONFLICT (memory_id) DO UPDATE SET
                     model = EXCLUDED.model,
                     dimensions = EXCLUDED.dimensions,
                     embedding = EXCLUDED.embedding,
                     created_at = now()",
                id.0,
                space.model,
                space.dimensions as i32,
                vector.as_slice(),
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Whether pgvector can search vectors of `dimensions`: the `vector`
    /// extension is installed (or this user may install it), and the HNSW
    /// index of those dimensions exists (made here if not). `false`, said
    /// once in the log, keeps recall lexical.
    pub async fn vector_search_ready(&self, dimensions: u32) -> bool {
        if !(1..=2_000).contains(&dimensions) {
            return false;
        }
        if let Some(ready) = self
            .vectors
            .ready
            .lock()
            .ok()
            .and_then(|m| m.get(&dimensions).copied())
        {
            return ready;
        }
        let ready = match self.prepare_vector_index(dimensions).await {
            Ok(ready) => ready,
            Err(e) => {
                tracing::warn!(dimensions, error = %e, "pgvector search unavailable; recall stays lexical");
                false
            }
        };
        if let Ok(mut known) = self.vectors.ready.lock() {
            known.insert(dimensions, ready);
        }
        ready
    }

    async fn prepare_vector_index(&self, dimensions: u32) -> Result<bool, StoreError> {
        let installed = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector') AS "installed!""#
        )
        .fetch_one(&self.pool)
        .await?;
        if !installed {
            let available = sqlx::query_scalar!(
                r#"SELECT EXISTS (
                       SELECT 1 FROM pg_available_extensions WHERE name = 'vector'
                   ) AS "available!""#
            )
            .fetch_one(&self.pool)
            .await?;
            if !available {
                tracing::info!(
                    "the `vector` extension (pgvector) is not available; recall stays lexical"
                );
                return Ok(false);
            }
            // pgvector is not a trusted extension: a database owner without
            // superuser rights may not create it.
            if let Err(e) = sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS vector")
                .execute(&self.pool)
                .await
            {
                tracing::warn!(error = %e, "cannot create the `vector` extension; recall stays lexical");
                return Ok(false);
            }
        }
        let version =
            sqlx::query_scalar!("SELECT extversion FROM pg_extension WHERE extname = 'vector'")
                .fetch_one(&self.pool)
                .await?;
        if let Ok(mut iterative) = self.vectors.iterative.lock() {
            *iterative = Some(iterative_scans(&version));
        }
        // `dimensions` is a number checked above, not input text.
        let index = format!(
            "CREATE INDEX IF NOT EXISTS repo_memory_embeddings_hnsw_{dimensions}
                 ON repo_memory_embeddings
              USING hnsw ((embedding::vector({dimensions})) vector_cosine_ops)
              WHERE dimensions = {dimensions}"
        );
        sqlx::raw_sql(AssertSqlSafe(index))
            .execute(&self.pool)
            .await?;
        Ok(true)
    }

    /// The memories `repo_id` may recall whose embedding in `near.space`
    /// is nearest `near.vector`, at least `near.min_similarity` alike,
    /// with their rank (1 the nearest). Needs [`Self::vector_search_ready`].
    pub(super) async fn near_memories(
        &self,
        repo_id: RepoId,
        near: NearQuery<'_>,
    ) -> Result<Vec<(MemoryId, i64)>, StoreError> {
        near.space.check(near.vector)?;
        let d = near.space.dimensions;
        // `d` is a number checked above; the rest are bound.
        let sql = format!(
            "WITH tenant_repos AS (
                 SELECT r.id
                   FROM repos r
                   JOIN repos me ON me.tenant_id = r.tenant_id
                  WHERE me.id = $1
             )
             SELECT id, row_number() OVER (ORDER BY distance, id DESC) AS rank
               FROM (
                   SELECT e.memory_id AS id,
                          (e.embedding::vector({d})) <=> ($2::real[]::vector({d})) AS distance
                     FROM repo_memory_embeddings e
                     JOIN repo_memories m ON m.id = e.memory_id
                    WHERE e.dimensions = {d}
                      AND e.model = $3
                      AND (m.repo_id = $1
                           OR (m.scope = 'tenant'
                               AND m.repo_id IN (SELECT id FROM tenant_repos)))
                    ORDER BY distance
                    LIMIT $4
               ) AS near
              WHERE distance <= $5"
        );
        let max_distance = f64::from(1.0 - near.min_similarity.clamp(0.0, 1.0));
        let mut tx = self.pool.begin().await?;
        // The index finds the nearest vectors of every repo first, and the
        // repo's filter runs after: from pgvector 0.8 the scan goes on until
        // enough of this repo's are found. An older one has no such setting.
        let iterative = self
            .vectors
            .iterative
            .lock()
            .ok()
            .and_then(|i| *i)
            .unwrap_or(false);
        if iterative {
            sqlx::raw_sql("SET LOCAL hnsw.iterative_scan = relaxed_order")
                .execute(&mut *tx)
                .await?;
        }
        let rows: Vec<(i64, i64)> = sqlx::query_as(AssertSqlSafe(sql))
            .bind(repo_id.0)
            .bind(near.vector)
            .bind(near.space.model)
            .bind(VECTOR_DEPTH)
            .bind(max_distance)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|(id, rank)| (MemoryId(id), rank))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::iterative_scans;

    #[test]
    fn iterative_scans_came_with_pgvector_0_8() {
        assert!(iterative_scans("0.8.0"));
        assert!(iterative_scans("0.10.1"));
        assert!(iterative_scans("1.0"));
        assert!(!iterative_scans("0.7.4"));
        assert!(!iterative_scans("garbage"));
    }
}
