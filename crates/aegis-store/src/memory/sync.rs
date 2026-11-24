//! A repo's memories kept in its own files (#660): runbooks and business
//! rules synced from its main branch, `source_ref` `git:<path>#<slug>`.
//! A sync writes the sections that are new or changed, leaves the others,
//! and removes the `git:` memories whose section is gone, in one
//! transaction, one sync of a repo at a time.

use std::collections::HashSet;

use serde_json::json;

use super::{MemoryKind, NewRepoMemory, upsert_in};
use crate::audit::{self, Actor, Target};
use crate::tx;
use crate::{RepoId, Store, StoreError};

/// What every synced memory's `source_ref` starts with; no other write
/// takes one.
pub const GIT_SOURCE_PREFIX: &str = "git:";

/// What a sync did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GitSync {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
}

impl Store {
    /// Makes `repo`'s `git:` memories exactly `entries`: each new or
    /// changed one written (`memory.upsert`), each other `git:` memory
    /// deleted (`memory.delete`), the whole audited as `memory.sync`.
    /// An entry whose `source_ref` is not `git:` or of another repo is
    /// refused; of two entries with one `(kind, source_ref)`, the first
    /// counts.
    pub async fn sync_git_memories(
        &self,
        repo: RepoId,
        entries: &[NewRepoMemory],
        actor: &Actor,
    ) -> Result<GitSync, StoreError> {
        if let Some(bad) = entries.iter().find(|e| {
            e.repo_id != repo
                || !synced_kind(e.kind)
                || !e.source_ref.trim().starts_with(GIT_SOURCE_PREFIX)
        }) {
            return Err(StoreError::Invalid {
                field: "source_ref",
                message: format!(
                    "`{}` is not a `{GIT_SOURCE_PREFIX}` runbook or business rule of repo {}",
                    bad.source_ref, repo.0
                ),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            sqlx::query!(
                "SELECT pg_advisory_xact_lock(hashtextextended('aegis-memory-sync:' || $1::text, 0))",
                repo.0.to_string(),
            )
            .execute(&mut *tx)
            .await?;
            let existing = sqlx::query!(
                "SELECT id, kind, source_ref, title, summary, keywords
                   FROM repo_memories
                  WHERE repo_id = $1 AND starts_with(source_ref, $2)",
                repo.0,
                GIT_SOURCE_PREFIX,
            )
            .fetch_all(&mut *tx)
            .await?;
            let mut done = GitSync::default();
            let mut kept: HashSet<(String, String)> = HashSet::new();
            for entry in entries {
                let key = (
                    entry.kind.as_str().to_owned(),
                    entry.source_ref.trim().to_owned(),
                );
                if !kept.insert(key.clone()) {
                    continue;
                }
                let old = existing
                    .iter()
                    .find(|row| row.kind == key.0 && row.source_ref == key.1);
                let same = old.is_some_and(|row| {
                    row.title == entry.title.trim()
                        && row.summary == entry.summary.trim()
                        && row.keywords == entry.keywords.trim()
                });
                if same {
                    done.unchanged += 1;
                    continue;
                }
                upsert_in(&mut tx, entry, actor).await?;
                if old.is_some() {
                    done.updated += 1;
                } else {
                    done.added += 1;
                }
            }
            for row in &existing {
                if kept.contains(&(row.kind.clone(), row.source_ref.clone())) {
                    continue;
                }
                sqlx::query!("DELETE FROM repo_memories WHERE id = $1", row.id)
                    .execute(&mut *tx)
                    .await?;
                audit::record(
                    &mut tx,
                    actor,
                    "memory.delete",
                    Target {
                        repo: Some(repo),
                        job: None,
                    },
                    json!({
                        "memory_id": row.id,
                        "kind": row.kind,
                        "source_ref": row.source_ref,
                        "why": "its section is gone from the main branch",
                    }),
                )
                .await?;
                done.removed += 1;
            }
            audit::record(
                &mut tx,
                actor,
                "memory.sync",
                Target {
                    repo: Some(repo),
                    job: None,
                },
                json!({
                    "added": done.added,
                    "updated": done.updated,
                    "unchanged": done.unchanged,
                    "removed": done.removed,
                }),
            )
            .await?;
            Ok::<_, StoreError>(done)
        }
        .await;
        tx::commit(tx, work).await
    }
}

/// Whether `kind` is one a sync writes.
fn synced_kind(kind: MemoryKind) -> bool {
    matches!(kind, MemoryKind::Runbook | MemoryKind::BusinessRule)
}
