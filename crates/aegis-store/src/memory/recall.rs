//! Hybrid recall of repository memories (#596). Three channels read every
//! memory the repo may see, through indexes, each giving its best
//! [`CHANNEL_DEPTH`]:
//!
//! 1. paths: the issue's paths, file names and words against each memory's
//!    `path_keys` (GIN);
//! 2. text: the issue's words as an OR full-text query on `search_tsv`
//!    (GIN, `simple` configuration: no stemming, any language);
//! 3. substring: the issue's words, and the 3-character windows of its
//!    Japanese, Chinese or Korean runs, as `ILIKE` patterns on
//!    `search_text` (trigram GIN), for text whose words are not split by
//!    spaces.
//!
//! A mode with an embedding model adds a fourth, [`super::vector`]: the
//! memories nearest the issue's embedding (#655).
//!
//! [`fuse`] merges them by Reciprocal Rank Fusion, weighs the repo's own
//! memories over its tenant's, guidance (runbooks, business rules) a little
//! over history, and lets older memories fade.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use super::vector::NearQuery;
use super::{MemoryId, MemoryKind, MemoryRow, MemoryScope, RepoMemory};
use crate::{JobId, RepoId, Store, StoreError};

/// RRF's constant (Cormack et al., 2009): ranks far down a channel still
/// count, but little.
const RRF_K: f64 = 60.0;
/// Best memories each channel gives the fusion.
const CHANNEL_DEPTH: i64 = 50;
/// Words and patterns of one issue the channels search for.
const MAX_QUERY_TERMS: usize = 64;
/// A memory shared by another repo of the tenant, against the repo's own.
const TENANT_WEIGHT: f64 = 0.8;
/// Runbooks and business rules, against merged PRs and review lessons.
const GUIDANCE_WEIGHT: f64 = 1.1;
/// Age at which a memory's score has halved.
const HALF_LIFE_DAYS: f64 = 180.0;
/// Patterns a memory must contain for the substring channel, when the
/// issue gives that many: one shared 3-character window is chance.
const MIN_SUBSTRING_HITS: i64 = 2;

/// A memory's rank in each channel, 1 the best; `None` where the channel
/// did not find it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecallScores {
    pub path: Option<i64>,
    pub text: Option<i64>,
    pub substring: Option<i64>,
    /// Among the nearest embeddings (#655).
    pub vector: Option<i64>,
}

/// What the channels search for, from an issue's title and body.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecallQuery {
    /// Lowercased paths, file names, stems and words.
    path_keys: Vec<String>,
    /// Lowercased words, quoted into an OR query by the SQL.
    words: Vec<String>,
    /// `ILIKE` patterns, escaped and wrapped in `%`.
    patterns: Vec<String>,
}

impl RecallQuery {
    /// `None` when the issue gives nothing to search for.
    fn of(title: &str, body: &str) -> Option<Self> {
        let text = format!("{title}\n{body}");
        let words = tokenize(&text);
        let paths = extract_path_stems(&text);
        if words.is_empty() && paths.is_empty() {
            return None;
        }
        let path_keys = paths
            .iter()
            .chain(&words)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(2 * MAX_QUERY_TERMS)
            .collect();
        let patterns = substring_patterns(&words);
        Some(Self {
            path_keys,
            words: words.into_iter().take(MAX_QUERY_TERMS).collect(),
            patterns,
        })
    }

    fn min_substring_hits(&self) -> i64 {
        MIN_SUBSTRING_HITS.min(self.patterns.len() as i64)
    }
}

struct RecallRow {
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
    path_rank: Option<i64>,
    text_rank: Option<i64>,
    substring_rank: Option<i64>,
}

impl TryFrom<RecallRow> for (RepoMemory, RecallScores) {
    type Error = StoreError;

    fn try_from(row: RecallRow) -> Result<Self, StoreError> {
        let memory = RepoMemory {
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
        };
        let scores = RecallScores {
            path: row.path_rank,
            text: row.text_rank,
            substring: row.substring_rank,
            vector: None,
        };
        Ok((memory, scores))
    }
}

impl Store {
    /// Recalls up to `limit` memories relevant to an issue of `repo_id`:
    /// its own, and those its tenant's other repos share, from every row
    /// (#596).
    pub async fn recall_repo_memories(
        &self,
        repo_id: RepoId,
        query_title: &str,
        query_body: &str,
        limit: usize,
    ) -> Result<Vec<RepoMemory>, StoreError> {
        self.recall_repo_memories_near(repo_id, query_title, query_body, limit, None)
            .await
    }

    /// [`Self::recall_repo_memories`], with the memories nearest `near` as a
    /// fourth channel when given (#655): the caller checked
    /// [`Self::vector_search_ready`] for its dimensions.
    pub async fn recall_repo_memories_near(
        &self,
        repo_id: RepoId,
        query_title: &str,
        query_body: &str,
        limit: usize,
        near: Option<NearQuery<'_>>,
    ) -> Result<Vec<RepoMemory>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut hits = match RecallQuery::of(query_title, query_body) {
            Some(query) => self.lexical_hits(repo_id, &query).await?,
            None => Vec::new(),
        };
        if let Some(near) = near {
            let nearest = self.near_memories(repo_id, near).await?;
            let missing: Vec<i64> = nearest
                .iter()
                .filter(|(id, _)| !hits.iter().any(|(m, _)| m.id == *id))
                .map(|(id, _)| id.0)
                .collect();
            for memory in self.memories_by_id(&missing).await? {
                hits.push((memory, RecallScores::default()));
            }
            for (memory, scores) in &mut hits {
                scores.vector = nearest
                    .iter()
                    .find(|(id, _)| *id == memory.id)
                    .map(|(_, rank)| *rank);
            }
        }
        Ok(fuse(hits, repo_id, OffsetDateTime::now_utc(), limit))
    }

    async fn memories_by_id(&self, ids: &[i64]) -> Result<Vec<RepoMemory>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as!(
            MemoryRow,
            "SELECT id, repo_id, kind, source_ref, title, summary, paths,
                    keywords, job_id, scope, created_at, updated_at
               FROM repo_memories
              WHERE id = ANY($1) AND obsolete_at IS NULL",
            ids,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(RepoMemory::try_from).collect()
    }

    /// The memories the three lexical channels find, with their ranks.
    async fn lexical_hits(
        &self,
        repo_id: RepoId,
        query: &RecallQuery,
    ) -> Result<Vec<(RepoMemory, RecallScores)>, StoreError> {
        let rows = sqlx::query_as!(
            RecallRow,
            r#"WITH tenant_repos AS (
                   SELECT r.id
                     FROM repos r
                     JOIN repos me ON me.tenant_id = r.tenant_id
                    WHERE me.id = $1
               ),
               by_path AS (
                   SELECT m.id, row_number() OVER (
                              ORDER BY cardinality(ARRAY(
                                  SELECT unnest(m.path_keys)
                                  INTERSECT SELECT unnest($2::text[])
                              )) DESC, m.id DESC
                          ) AS rank
                     FROM repo_memories m
                    WHERE (m.repo_id = $1
                           OR (m.scope = 'tenant' AND m.repo_id IN (SELECT id FROM tenant_repos)))
                      AND m.obsolete_at IS NULL
                      AND m.path_keys && $2::text[]
                    ORDER BY rank
                    LIMIT $5
               ),
               words AS (
                   SELECT to_tsquery('simple', string_agg(quote_literal(w), ' | ')) AS query
                     FROM unnest($3::text[]) AS w
                   HAVING count(*) > 0
               ),
               by_text AS (
                   SELECT m.id, row_number() OVER (
                              ORDER BY ts_rank_cd(m.search_tsv, words.query) DESC, m.id DESC
                          ) AS rank
                     FROM repo_memories m, words
                    WHERE (m.repo_id = $1
                           OR (m.scope = 'tenant' AND m.repo_id IN (SELECT id FROM tenant_repos)))
                      AND m.obsolete_at IS NULL
                      AND m.search_tsv @@ words.query
                    ORDER BY rank
                    LIMIT $5
               ),
               by_substring AS (
                   SELECT id, row_number() OVER (ORDER BY hits DESC, id DESC) AS rank
                     FROM (
                         SELECT m.id,
                                (SELECT count(*) FROM unnest($4::text[]) AS p
                                  WHERE m.search_text ILIKE p) AS hits
                           FROM repo_memories m
                          WHERE (m.repo_id = $1
                                 OR (m.scope = 'tenant'
                                     AND m.repo_id IN (SELECT id FROM tenant_repos)))
                            AND m.obsolete_at IS NULL
                            AND m.search_text ILIKE ANY ($4::text[])
                     ) AS h
                    WHERE hits >= $6
                    ORDER BY rank
                    LIMIT $5
               )
               SELECT m.id, m.repo_id, m.kind, m.source_ref, m.title, m.summary, m.paths,
                      m.keywords, m.job_id, m.scope, m.created_at, m.updated_at,
                      p.rank AS "path_rank?", t.rank AS "text_rank?",
                      s.rank AS "substring_rank?"
                 FROM repo_memories m
                 LEFT JOIN by_path p ON p.id = m.id
                 LEFT JOIN by_text t ON t.id = m.id
                 LEFT JOIN by_substring s ON s.id = m.id
                WHERE m.id IN (SELECT id FROM by_path
                               UNION SELECT id FROM by_text
                               UNION SELECT id FROM by_substring)"#,
            repo_id.0,
            &query.path_keys,
            &query.words,
            &query.patterns,
            CHANNEL_DEPTH,
            query.min_substring_hits(),
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(<(RepoMemory, RecallScores)>::try_from)
            .collect()
    }
}

/// The `limit` best of `hits` for an issue of `repo`, by Reciprocal Rank
/// Fusion of their channel ranks, weighed by scope, kind and age at `now`.
/// Ties go to the newer memory.
pub fn fuse(
    hits: Vec<(RepoMemory, RecallScores)>,
    repo: RepoId,
    now: OffsetDateTime,
    limit: usize,
) -> Vec<RepoMemory> {
    let mut scored: Vec<(f64, RepoMemory)> = hits
        .into_iter()
        .map(|(memory, scores)| (score(&memory, scores, repo, now), memory))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| sb.total_cmp(sa).then_with(|| b.id.0.cmp(&a.id.0)));
    scored.into_iter().take(limit).map(|(_, m)| m).collect()
}

fn score(memory: &RepoMemory, scores: RecallScores, repo: RepoId, now: OffsetDateTime) -> f64 {
    let rrf: f64 = [scores.path, scores.text, scores.substring, scores.vector]
        .into_iter()
        .flatten()
        .map(|rank| 1.0 / (RRF_K + rank as f64))
        .sum();
    let scope = if memory.repo_id == repo {
        1.0
    } else {
        TENANT_WEIGHT
    };
    let kind = match memory.kind {
        MemoryKind::Runbook | MemoryKind::BusinessRule => GUIDANCE_WEIGHT,
        MemoryKind::MergedPr | MemoryKind::ReviewLesson => 1.0,
    };
    let age_days = ((now - memory.updated_at).whole_seconds().max(0) as f64) / 86_400.0;
    let decay = 0.5_f64.powf(age_days / HALF_LIFE_DAYS);
    rrf * scope * kind * decay
}

/// `ILIKE` patterns for `words`: each word, and each 3-character window of
/// a word with Japanese, Chinese or Korean in it (such text has no spaces
/// between words). Escaped, deduplicated, at most [`MAX_QUERY_TERMS`].
fn substring_patterns(words: &BTreeSet<String>) -> Vec<String> {
    let plain = words.iter().filter(|w| !w.chars().any(is_cjk)).cloned();
    let windows = words
        .iter()
        .filter(|w| w.chars().any(is_cjk))
        .flat_map(|w| {
            let chars: Vec<char> = w.chars().collect();
            match chars.len() {
                0..=3 => vec![w.clone()],
                n => (0..=n - 3)
                    .map(|i| chars[i..i + 3].iter().collect())
                    .collect(),
            }
        });
    let mut seen = BTreeSet::new();
    plain
        .chain(windows)
        .filter(|p| seen.insert(p.clone()))
        .take(MAX_QUERY_TERMS)
        .map(|p| format!("%{}%", escape_like(&p)))
        .collect()
}

fn escape_like(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

/// Hiragana, katakana (and its half-width forms), CJK ideographs and
/// Hangul syllables.
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{3040}'..='\u{30FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{FF66}'..='\u{FF9F}'
    )
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|w| w.trim_matches('_').to_lowercase())
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
        if (cleaned.contains('/') || cleaned.contains('.')) && cleaned.chars().count() >= 3 {
            let lower = cleaned.to_lowercase();
            out.insert(lower.clone());
            if let Some(last) = lower.rsplit('/').next() {
                if last.chars().count() >= 3 {
                    out.insert(last.to_owned());
                }
                if let Some((stem, _)) = last.split_once('.')
                    && stem.chars().count() >= 3
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

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn memory(id: i64, repo: i64, kind: MemoryKind, age_days: i64) -> RepoMemory {
        let now = OffsetDateTime::now_utc();
        RepoMemory {
            id: MemoryId(id),
            repo_id: RepoId(repo),
            kind,
            source_ref: format!("ref-{id}"),
            title: format!("memory {id}"),
            summary: "summary".into(),
            paths: vec![],
            keywords: String::new(),
            job_id: None,
            scope: MemoryScope::Repo,
            created_at: now - Duration::days(age_days),
            updated_at: now - Duration::days(age_days),
        }
    }

    fn ranks(path: Option<i64>, text: Option<i64>, substring: Option<i64>) -> RecallScores {
        RecallScores {
            path,
            text,
            substring,
            vector: None,
        }
    }

    fn ids(memories: &[RepoMemory]) -> Vec<i64> {
        memories.iter().map(|m| m.id.0).collect()
    }

    #[test]
    fn a_memory_found_by_several_channels_beats_one_found_by_one() {
        let now = OffsetDateTime::now_utc();
        let hits = vec![
            (
                memory(1, 1, MemoryKind::MergedPr, 0),
                ranks(Some(1), None, None),
            ),
            (
                memory(2, 1, MemoryKind::MergedPr, 0),
                ranks(Some(2), Some(1), Some(1)),
            ),
            (
                memory(3, 1, MemoryKind::MergedPr, 0),
                ranks(None, Some(2), None),
            ),
        ];
        assert_eq!(ids(&fuse(hits, RepoId(1), now, 5)), vec![2, 1, 3]);
    }

    #[test]
    fn the_repos_own_memory_beats_its_tenants_and_old_ones_fade() {
        let now = OffsetDateTime::now_utc();
        let same = ranks(Some(1), Some(1), None);
        let hits = vec![
            (memory(1, 2, MemoryKind::MergedPr, 0), same),
            (memory(2, 1, MemoryKind::MergedPr, 0), same),
            (memory(3, 1, MemoryKind::MergedPr, 720), same),
        ];
        assert_eq!(ids(&fuse(hits, RepoId(1), now, 5)), vec![2, 1, 3]);
    }

    #[test]
    fn guidance_weighs_a_little_more_and_the_limit_holds() {
        let now = OffsetDateTime::now_utc();
        let same = ranks(None, Some(3), None);
        let hits = vec![
            (memory(1, 1, MemoryKind::MergedPr, 0), same),
            (memory(2, 1, MemoryKind::BusinessRule, 0), same),
            (memory(3, 1, MemoryKind::Runbook, 0), same),
        ];
        assert_eq!(ids(&fuse(hits, RepoId(1), now, 2)), vec![3, 2]);
        assert!(fuse(Vec::new(), RepoId(1), now, 2).is_empty());
    }

    #[test]
    fn the_query_has_paths_words_and_japanese_windows() {
        let q = RecallQuery::of(
            "Webhook retry drops the key in src/Webhook_Sender.rs",
            "冪等キーを再生成 100% done",
        )
        .unwrap();
        for key in [
            "src/webhook_sender.rs",
            "webhook_sender.rs",
            "webhook_sender",
            "webhook",
            "retry",
        ] {
            assert!(q.path_keys.iter().any(|k| k == key), "{key}: {q:?}");
        }
        assert!(!q.words.iter().any(|w| w == "the"), "stopwords dropped");
        assert!(
            q.patterns.contains(&"%webhook\\_sender%".to_owned()),
            "{q:?}"
        );
        assert!(q.patterns.contains(&"%冪等キ%".to_owned()), "{q:?}");
        assert!(q.patterns.contains(&"%再生成%".to_owned()), "{q:?}");
        assert!(q.patterns.contains(&"%100%".to_owned()), "{q:?}");
        assert_eq!(q.min_substring_hits(), 2);
        assert!(RecallQuery::of("a b", "").is_none());
    }

    #[test]
    fn patterns_are_escaped_and_capped() {
        let words: BTreeSet<String> = (0..200).map(|i| format!("w{i:03}")).collect();
        assert_eq!(substring_patterns(&words).len(), MAX_QUERY_TERMS);
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
    }
}
