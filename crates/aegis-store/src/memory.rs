//! Per-repository Incident & Domain Memory (#456): merged PR summaries,
//! review lessons from `/aegis fix` rounds, operational runbooks, and
//! business rules.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::audit::{self, Actor, Target};
use crate::tx;
use crate::{JobId, Page, PageOf, RepoId, Store, StoreError};

const MAX_SOURCE_REF_CHARS: usize = 256;
const MAX_TITLE_CHARS: usize = 500;
const MAX_SUMMARY_CHARS: usize = 4000;
const MAX_KEYWORDS_CHARS: usize = 2000;
const MAX_RECALL_CANDIDATES: i64 = 200;

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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
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
                     repo_id, kind, source_ref, title, summary, paths, keywords, job_id
                 )
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT (repo_id, kind, source_ref) DO UPDATE SET
                     title = EXCLUDED.title,
                     summary = EXCLUDED.summary,
                     paths = EXCLUDED.paths,
                     keywords = EXCLUDED.keywords,
                     job_id = COALESCE(EXCLUDED.job_id, repo_memories.job_id),
                     updated_at = now()
                 RETURNING id, repo_id, kind, source_ref, title, summary, paths,
                           keywords, job_id, created_at, updated_at",
                entry.repo_id.0,
                entry.kind.as_str(),
                source_ref,
                title,
                summary,
                &paths,
                keywords,
                entry.job_id.map(|j| j.0),
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?
            .try_into()?;
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
                    keywords, job_id, created_at, updated_at
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
              RETURNING kind, source_ref",
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
                }),
            )
            .await?;
            Ok::<_, StoreError>(tx::Done::Commit(true))
        }
        .await;
        tx::end(tx, work).await
    }

    /// Recalls up to `limit` relevant historical lessons for `repo_id` matching
    /// `query_title` and `query_body`, ranked by token and file-path overlap.
    pub async fn recall_repo_memories(
        &self,
        repo_id: RepoId,
        query_title: &str,
        query_body: &str,
        limit: usize,
    ) -> Result<Vec<RepoMemory>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as!(
            MemoryRow,
            "SELECT id, repo_id, kind, source_ref, title, summary, paths,
                    keywords, job_id, created_at, updated_at
               FROM repo_memories
              WHERE repo_id = $1
              ORDER BY updated_at DESC, id DESC
              LIMIT $2",
            repo_id.0,
            MAX_RECALL_CANDIDATES,
        )
        .fetch_all(&self.pool)
        .await?;

        let candidates = rows
            .into_iter()
            .map(RepoMemory::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rank_memories(candidates, query_title, query_body, limit))
    }
}

/// Ranks `candidates` against `(query_title, query_body)` and returns up to
/// `limit` items with positive relevance score.
pub fn rank_memories(
    candidates: Vec<RepoMemory>,
    query_title: &str,
    query_body: &str,
    limit: usize,
) -> Vec<RepoMemory> {
    if limit == 0 || candidates.is_empty() {
        return Vec::new();
    }
    let combined_query = format!("{query_title}\n{query_body}");
    let query_tokens = tokenize(&combined_query);
    let query_paths = extract_path_stems(&combined_query);
    if query_tokens.is_empty() && query_paths.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(u32, RepoMemory)> = candidates
        .into_iter()
        .filter_map(|entry| {
            let score = score_with(&entry, &query_tokens, &query_paths);
            (score > 0).then_some((score, entry))
        })
        .collect();

    scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then_with(|| b.id.0.cmp(&a.id.0)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry)
        .collect()
}

/// Computes the deterministic relevance score of `entry` for `(query_title, query_body)`.
pub fn score_repo_memory(entry: &RepoMemory, query_title: &str, query_body: &str) -> u32 {
    let combined = format!("{query_title}\n{query_body}");
    let tokens = tokenize(&combined);
    let paths = extract_path_stems(&combined);
    score_with(entry, &tokens, &paths)
}

fn score_with(
    entry: &RepoMemory,
    query_tokens: &BTreeSet<String>,
    query_paths: &BTreeSet<String>,
) -> u32 {
    let mut score = 0u32;

    for path in &entry.paths {
        let lower = path.to_ascii_lowercase();
        if query_paths.contains(&lower) {
            score += 8;
        }
        for stem in extract_path_stems(&lower) {
            if query_paths.contains(&stem) || query_tokens.contains(&stem) {
                score += 5;
            }
        }
    }

    for token in tokenize(&entry.title) {
        if query_tokens.contains(&token) {
            score += 3;
        }
    }

    for token in tokenize(&entry.keywords) {
        if query_tokens.contains(&token) {
            score += 3;
        }
    }

    for token in tokenize(&entry.summary) {
        if query_tokens.contains(&token) {
            score += 1;
        }
    }

    if score > 0 && matches!(entry.kind, MemoryKind::BusinessRule | MemoryKind::Runbook) {
        score += 2;
    }

    score
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|w| w.trim_matches('_').to_ascii_lowercase())
        .filter(|w| w.chars().count() >= 3 && !is_stopword(w))
        .collect()
}

fn extract_path_stems(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in text.split_whitespace() {
        let cleaned = raw.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '.'
            )
        });
        if (cleaned.contains('/') || cleaned.contains('.')) && cleaned.len() >= 3 {
            let lower = cleaned.to_ascii_lowercase();
            out.insert(lower.clone());
            if let Some(last) = lower.rsplit('/').next() {
                if last.len() >= 3 {
                    out.insert(last.to_owned());
                }
                if let Some((stem, _)) = last.split_once('.')
                    && stem.len() >= 3
                    && !is_stopword(stem)
                {
                    out.insert(stem.to_owned());
                }
            }
        }
    }
    out
}

fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "the"
            | "and"
            | "for"
            | "with"
            | "that"
            | "this"
            | "from"
            | "when"
            | "where"
            | "what"
            | "which"
            | "while"
            | "into"
            | "onto"
            | "after"
            | "before"
            | "under"
            | "over"
            | "have"
            | "has"
            | "had"
            | "does"
            | "did"
            | "not"
            | "are"
            | "was"
            | "were"
            | "been"
            | "being"
            | "should"
            | "would"
            | "could"
            | "can"
            | "will"
            | "just"
            | "more"
            | "some"
            | "such"
            | "than"
            | "then"
            | "them"
            | "they"
            | "their"
            | "there"
            | "here"
            | "issue"
            | "bug"
            | "fix"
            | "error"
            | "fails"
            | "failed"
            | "test"
            | "tests"
            | "code"
            | "file"
            | "files"
            | "src"
            | "lib"
            | "mod"
            | "main"
            | "app"
            | "pkg"
            | "cmd"
            | "bin"
            | "internal"
            | "crate"
            | "crates"
    )
}
