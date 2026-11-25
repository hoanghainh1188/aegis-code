//! Server-side Incident & Domain Memory orchestration (#456):
//! - Automatic extraction of `merged_pr` and `review_lesson` entries when an
//!   agent PR is merged (via `Tier::Light` with deterministic ledger fallback).
//! - Recall of top relevant repository memories into `IssueInput` (`BP3`)
//!   before triage, diagnosis, and fix execution.

use std::collections::BTreeSet;

use aegis_ledger::{AnswerChange, IssueId, IssueRecord, LedgerStore};
use aegis_lifecycle::{
    IssueInput, MemoryLesson, MemorySection, MergedPrFacts, Modes, extract_from_merged_pr,
    fallback_extract_from_merged_pr, redact_for_embedding,
};
use aegis_provider::{EmbedPurpose, Embedder, ModelPolicy, Provider};
use aegis_secrets::Secrets;

use aegis_store::{
    Actor, EmbeddingSpace, JobId, MemoryId, MemoryKind, NearQuery, NewRepoMemory, Repo, RepoId,
    RepoMemory, Store, StoreError,
};

/// Maximum number of relevant repository memories recalled into `BP3` per job.
pub const DEFAULT_RECALL_LIMIT: usize = 5;
/// Candidates recalled before curation (#658): the stale and the
/// near-duplicate ones give their slots to the next.
pub const RECALL_POOL: usize = 15;
/// Two historical memories sharing this much of their paths and keywords
/// (Jaccard) say the same thing: the better ranked one is kept.
pub const DUPLICATE_OVERLAP: f64 = 0.6;

/// The files of `branch` in the clone at `clone`, for [`RecallFor::tree`];
/// `None` (said in the log) when they cannot be read.
pub async fn base_tree(clone: &std::path::Path, branch: &str) -> Option<BTreeSet<String>> {
    let (clone, branch) = (clone.to_owned(), branch.to_owned());
    let listed =
        tokio::task::spawn_blocking(move || aegis_lifecycle::clone::branch_files(&clone, &branch))
            .await;
    match listed {
        Ok(Ok(files)) => Some(files.into_iter().map(|(f, _)| f).collect()),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "reading the base branch's files; memories are not checked for staleness");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "reading the base branch's files");
            None
        }
    }
}

/// What the job a recall is for adds to it (#658).
#[derive(Debug, Clone, Copy, Default)]
pub struct RecallFor<'a> {
    /// The job given the memories, kept for its page.
    pub job: Option<JobId>,
    /// The files of the repo's base branch: a merged PR or a review
    /// lesson none of whose paths is still there is down-ranked.
    pub tree: Option<&'a BTreeSet<String>>,
}

/// What happened on the repo, as opposed to the guidance it keeps
/// (runbooks, business rules): checked for staleness and repeats (#658).
fn historical(kind: MemoryKind) -> bool {
    matches!(
        kind,
        MemoryKind::MergedPr
            | MemoryKind::ReviewLesson
            | MemoryKind::RejectedPattern
            | MemoryKind::ConfirmedDiagnosis
    )
}

/// Whether `path` (a file, maybe with `:line`, or a directory) is still
/// in `tree`.
fn exists(tree: &BTreeSet<String>, path: &str) -> bool {
    let path = path.trim().trim_start_matches("./");
    let file = path.split(':').next().unwrap_or(path).trim_end_matches('/');
    if file.is_empty() {
        return false;
    }
    if tree.contains(file) {
        return true;
    }
    let dir = format!("{file}/");
    tree.range(dir.clone()..)
        .next()
        .is_some_and(|f| f.starts_with(&dir))
}

/// A merged PR or review lesson that names paths, none of them on the
/// base branch any more.
fn stale(memory: &RepoMemory, tree: &BTreeSet<String>) -> bool {
    historical(memory.kind)
        && !memory.paths.is_empty()
        && !memory.paths.iter().any(|p| exists(tree, p))
}

fn tokens(memory: &RepoMemory) -> BTreeSet<String> {
    memory
        .paths
        .iter()
        .map(|p| p.trim().to_lowercase())
        .chain(
            memory
                .keywords
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| w.chars().count() > 2)
                .map(str::to_lowercase),
        )
        .collect()
}

fn overlap(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

/// The `limit` memories a job is given out of `recalled` (best first):
/// stale ones after the others, and a historical memory that repeats a
/// better ranked one of its kind and module left out (#658): a merged PR
/// and its review lesson say different things. Runbooks and business
/// rules are never stale nor merged.
pub fn curate(
    recalled: Vec<RepoMemory>,
    tree: Option<&BTreeSet<String>>,
    limit: usize,
) -> Vec<RepoMemory> {
    let (fresh, old): (Vec<_>, Vec<_>) = recalled
        .into_iter()
        .partition(|m| tree.is_none_or(|t| !stale(m, t)));
    let mut kept: Vec<(RepoMemory, BTreeSet<String>)> = Vec::new();
    for memory in fresh.into_iter().chain(old) {
        if kept.len() == limit {
            break;
        }
        let words = tokens(&memory);
        let repeats = historical(memory.kind)
            && kept
                .iter()
                .any(|(k, w)| k.kind == memory.kind && overlap(w, &words) >= DUPLICATE_OVERLAP);
        if !repeats {
            kept.push((memory, words));
        }
    }
    kept.into_iter().map(|(m, _)| m).collect()
}
/// Memories without an embedding embedded before one recall (#655): those
/// written before the model was set, or after it changed, catch up a few
/// at a time.
pub const EMBED_BACKLOG_PER_RECALL: i64 = 16;

/// The embedding model of `memory`, refusing what `policy` refuses; `None`
/// (said in the log) keeps recall lexical (#655).
pub async fn embedder_of(
    memory: &MemorySection,
    policy: ModelPolicy,
    secrets: &Secrets,
) -> Option<Embedder> {
    let config = memory.embedding.as_ref()?;
    match Embedder::from_config_with_secrets(config, secrets).await {
        Ok(embedder) => Some(embedder.with_policy(policy)),
        Err(e) => {
            tracing::warn!(error = %e, "no embedding model for the memory; recall stays lexical");
            None
        }
    }
}

/// The issue's embedding to search with, once the repo's memories without
/// one are embedded; `None` (said in the log) keeps recall lexical.
async fn near_vector(
    store: &Store,
    repo_id: RepoId,
    issue: &IssueInput,
    embedder: &Embedder,
) -> Option<Vec<f32>> {
    let space = EmbeddingSpace {
        model: embedder.model(),
        dimensions: embedder.dimensions(),
    };
    if !store.vector_search_ready(space.dimensions).await {
        return None;
    }
    let fail = |what: &str, e: &dyn std::fmt::Display| {
        tracing::warn!(repo_id = repo_id.0, issue = %issue.id, error = %e, "{what}; recall stays lexical");
    };
    match store
        .memories_to_embed(repo_id, space, EMBED_BACKLOG_PER_RECALL)
        .await
    {
        Ok(todo) if !todo.is_empty() => {
            let texts: Vec<String> = todo.iter().map(|m| redact_for_embedding(&m.text)).collect();
            match embedder.embed(&texts, EmbedPurpose::Document).await {
                Ok(vectors) => {
                    let pairs: Vec<_> = todo.iter().map(|m| m.id).zip(vectors).collect();
                    if let Err(e) = store.save_memory_embeddings(space, &pairs).await {
                        fail("storing memory embeddings failed", &e);
                    }
                }
                Err(e) => fail("embedding memories failed", &e),
            }
        }
        Ok(_) => {}
        Err(e) => fail("reading memories to embed failed", &e),
    }
    let query = redact_for_embedding(&format!("{}\n{}", issue.title, issue.body));
    match embedder.embed(&[query], EmbedPurpose::Query).await {
        Ok(mut vectors) => vectors.pop(),
        Err(e) => {
            fail("embedding the issue failed", &e);
            None
        }
    }
}

/// Recalls relevant historical lessons (`merged_pr`, `review_lesson`,
/// `runbook`, `business_rule`) for `repo_id` and attaches them to `issue`
/// (`BP3` and triage inputs) while leaving `BP2` untouched (#456). With
/// `embedder`, the memories nearest the issue's meaning count too (#655).
pub async fn enrich_with_memory(
    store: &Store,
    repo_id: RepoId,
    issue: &mut IssueInput,
    embedder: Option<&Embedder>,
) {
    enrich_with_memory_for(store, repo_id, issue, embedder, RecallFor::default()).await;
}

/// [`enrich_with_memory`] for a job: curated against its base branch's
/// files, and what it was given kept for its page (#658).
pub async fn enrich_with_memory_for(
    store: &Store,
    repo_id: RepoId,
    issue: &mut IssueInput,
    embedder: Option<&Embedder>,
    recall: RecallFor<'_>,
) {
    let vector = match embedder {
        Some(embedder) => near_vector(store, repo_id, issue, embedder).await,
        None => None,
    };
    let near = embedder.zip(vector.as_deref()).map(|(e, v)| NearQuery {
        space: EmbeddingSpace {
            model: e.model(),
            dimensions: e.dimensions(),
        },
        vector: v,
        min_similarity: e.min_similarity(),
    });
    let recalled = match store
        .recall_repo_memories_near(repo_id, &issue.title, &issue.body, RECALL_POOL, near)
        .await
    {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(
                repo_id = repo_id.0,
                issue = %issue.id,
                error = %e,
                "repo memory recall failed"
            );
            return;
        }
    };
    let recalled = curate(recalled, recall.tree, DEFAULT_RECALL_LIMIT);
    if let Some(job) = recall.job {
        let ids: Vec<MemoryId> = recalled.iter().map(|m| m.id).collect();
        if let Err(e) = store.record_recalled_memories(job, &ids).await {
            tracing::warn!(job = job.0, error = %e, "keeping the job's recalled memories failed");
        }
    }
    if recalled.is_empty() {
        return;
    }
    let lessons: Vec<MemoryLesson> = recalled
        .into_iter()
        .map(|m| MemoryLesson {
            kind: m.kind.as_str().to_owned(),
            source_ref: m.source_ref,
            title: m.title,
            summary: m.summary,
            paths: m.paths,
        })
        .collect();
    issue.attach_memories(&lessons);
}

/// Extracts `merged_pr` and optional `review_lesson` memories from the root
/// job and review rounds of `pr_url` and stores them in `repo_memories`.
/// Returns the stored memory rows (empty when `pr_url` does not belong to any
/// tracked job).
/// The Light model of `repo`'s mode and its id, when the modes are known
/// and the provider can be made; `None` keeps the ledger's summary.
pub(crate) async fn light_of(
    modes: Option<&Modes>,
    secrets: &Secrets,
    repo: &Repo,
) -> Option<(Provider, String)> {
    let mode = modes?.config(&repo.mode)?;
    // The mode's residency (#592): the memory's text is the repo's too.
    let policy = match mode.check(&repo.mode) {
        Ok(policy) => policy,
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "the mode breaks its residency; using the ledger's summary");
            return None;
        }
    };
    match Provider::from_config_with_secrets(&mode.provider, secrets).await {
        Ok(provider) => Some((provider.with_policy(policy), mode.models.light.id.clone())),
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "no Light model for the memory; using the ledger's summary");
            None
        }
    }
}

/// [`extract_and_store`], with the Light model of the PR's repo's mode
/// when `modes` has it (#456).
pub async fn extract_and_store_merged_pr(
    store: &Store,
    pr_url: &str,
    modes: Option<&Modes>,
    secrets: &Secrets,
    actor: &Actor,
) -> Result<Vec<RepoMemory>, StoreError> {
    let Some(root) = store.job_by_pr_url(pr_url).await? else {
        return Ok(Vec::new());
    };
    let light = match store.repo(root.repo_id).await? {
        Some(repo) => light_of(modes, secrets, &repo).await,
        None => None,
    };
    let light = light.as_ref().map(|(p, m)| (p, m.as_str()));
    extract_and_store(store, pr_url, light, actor).await
}

/// Extracts and stores the memory of the merged PR `pr_url`, with `light`
/// when given, else from the ledger's summary alone.
pub async fn extract_and_store(
    store: &Store,
    pr_url: &str,
    light: Option<(&Provider, &str)>,
    actor: &Actor,
) -> Result<Vec<RepoMemory>, StoreError> {
    let Some(root) = store.job_by_pr_url(pr_url).await? else {
        return Ok(Vec::new());
    };
    let root_rec = read_record(store, &root.ledger_key).await?;
    let rounds = store.review_rounds_of(root.id).await?;

    let mut round_records = Vec::with_capacity(rounds.len());
    for round in &rounds {
        if let Some(rec) = read_record(store, &round.ledger_key).await? {
            round_records.push(rec);
        }
    }

    let (issue_title, answer_cause, answer_changes, mut changed_paths) =
        summarize_root_record(&root.issue_ref, root_rec.as_ref());
    let (review_notes, round_paths) = summarize_review_rounds(&round_records);
    changed_paths.extend(round_paths);
    changed_paths.sort();
    changed_paths.dedup();

    let facts = MergedPrFacts {
        issue_title: &issue_title,
        issue_body: &root.issue_ref,
        answer_cause: answer_cause.as_deref(),
        answer_changes: answer_changes.as_deref(),
        review_notes: &review_notes,
        changed_paths: &changed_paths,
    };
    let extracted = match light {
        Some((provider, model)) => extract_from_merged_pr(provider, model, &facts).await.0,
        None => fallback_extract_from_merged_pr(&facts),
    };

    let mut saved = Vec::with_capacity(2);
    let merged_entry = store
        .upsert_repo_memory_by(
            &NewRepoMemory {
                repo_id: root.repo_id,
                kind: MemoryKind::MergedPr,
                source_ref: pr_url.to_owned(),
                title: extracted.title.clone(),
                summary: extracted.summary.clone(),
                paths: extracted.paths.clone(),
                keywords: extracted.keywords.clone(),
                job_id: Some(root.id),
                scope: None,
            },
            actor,
        )
        .await?;
    saved.push(merged_entry);

    if let Some(lesson) = extracted.review_lesson {
        let last_round_id = rounds.last().map(|r| r.id).or(Some(root.id));
        let lesson_entry = store
            .upsert_repo_memory_by(
                &NewRepoMemory {
                    repo_id: root.repo_id,
                    kind: MemoryKind::ReviewLesson,
                    source_ref: format!("{pr_url}#reviews"),
                    title: format!("Review lesson: {}", extracted.title),
                    summary: lesson,
                    paths: extracted.paths,
                    keywords: extracted.keywords,
                    job_id: last_round_id,
                    scope: None,
                },
                actor,
            )
            .await?;
        saved.push(lesson_entry);
    }

    Ok(saved)
}

pub(crate) async fn read_record(
    store: &Store,
    key: &IssueId,
) -> Result<Option<IssueRecord>, StoreError> {
    let events = store.ledger().events(key).await?;
    if events.is_empty() {
        return Ok(None);
    }
    Ok(Some(IssueRecord::from_events(key.clone(), &events)))
}

pub(crate) fn summarize_root_record(
    issue_ref: &str,
    record: Option<&IssueRecord>,
) -> (String, Option<String>, Option<String>, Vec<String>) {
    let Some(rec) = record else {
        return (issue_ref.to_owned(), None, None, Vec::new());
    };
    let title = rec
        .issue_title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| issue_ref.to_owned());

    let Some(ans) = &rec.answer else {
        return (title, None, None, Vec::new());
    };
    let cause = (!ans.cause.trim().is_empty()).then(|| ans.cause.clone());
    let changes_text = format_changes(&ans.changes);
    let paths = extract_paths_from_changes(&ans.changes);
    (title, cause, changes_text, paths)
}

pub(crate) fn summarize_review_rounds(rounds: &[IssueRecord]) -> (Vec<String>, Vec<String>) {
    let mut notes = Vec::new();
    let mut paths = Vec::new();
    for rec in rounds {
        let Some(ans) = &rec.answer else { continue };
        for reply in &ans.review_replies {
            let trimmed = reply.reply.trim();
            if !trimmed.is_empty() {
                notes.push(trimmed.to_owned());
            }
        }
        if !ans.cause.trim().is_empty() {
            notes.push(ans.cause.trim().to_owned());
        }
        if let Some(ch) = format_changes(&ans.changes) {
            notes.push(ch);
        }
        paths.extend(extract_paths_from_changes(&ans.changes));
    }
    (notes, paths)
}

fn format_changes(changes: &[AnswerChange]) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let joined = changes
        .iter()
        .map(|c| format!("{}: {}", c.location.trim(), c.note.trim()))
        .collect::<Vec<_>>()
        .join("; ");
    (!joined.trim().is_empty()).then_some(joined)
}

fn extract_paths_from_changes(changes: &[AnswerChange]) -> Vec<String> {
    changes
        .iter()
        .filter_map(|c| {
            let loc = c.location.trim();
            let file = loc.split(':').next().unwrap_or(loc).trim();
            (!file.is_empty()).then(|| file.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_store::{MemoryId, MemoryScope};

    fn memory(id: i64, kind: MemoryKind, paths: &[&str], keywords: &str) -> RepoMemory {
        RepoMemory {
            id: MemoryId(id),
            repo_id: RepoId(1),
            kind,
            source_ref: format!("ref-{id}"),
            title: format!("memory {id}"),
            summary: "s".into(),
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            keywords: keywords.into(),
            job_id: None,
            scope: MemoryScope::Repo,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn ids(kept: &[RepoMemory]) -> Vec<i64> {
        kept.iter().map(|m| m.id.0).collect()
    }

    fn tree(files: &[&str]) -> BTreeSet<String> {
        files.iter().map(|f| (*f).to_owned()).collect()
    }

    #[test]
    fn a_lesson_on_files_gone_from_the_base_goes_last() {
        let base = tree(&["src/pay/charge.rs", "src/pay/refund/mod.rs"]);
        let recalled = vec![
            memory(
                1,
                MemoryKind::MergedPr,
                &["src/old/billing.rs"],
                "billing retry",
            ),
            memory(
                2,
                MemoryKind::ReviewLesson,
                &["src/pay/charge.rs:42"],
                "charge",
            ),
            memory(3, MemoryKind::MergedPr, &["src/pay/refund"], "refund dir"),
            // A runbook is never stale, paths or not.
            memory(4, MemoryKind::Runbook, &["ops/gone.md"], "runbook"),
            // Names no path: nothing to check.
            memory(5, MemoryKind::MergedPr, &[], "no paths"),
        ];
        let kept = curate(recalled.clone(), Some(&base), 5);
        assert_eq!(ids(&kept), [2, 3, 4, 5, 1]);
        assert_eq!(ids(&curate(recalled.clone(), Some(&base), 4)), [2, 3, 4, 5]);
        // Without the base's files, the order is the recall's.
        assert_eq!(ids(&curate(recalled, None, 5)), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_repeated_fix_of_one_module_gives_its_slot_to_the_next() {
        let recalled = vec![
            memory(
                1,
                MemoryKind::MergedPr,
                &["src/pay/charge.rs"],
                "charge retry idempotency",
            ),
            memory(
                2,
                MemoryKind::MergedPr,
                &["src/pay/charge.rs"],
                "charge retry idempotency key",
            ),
            memory(
                3,
                MemoryKind::ReviewLesson,
                &["src/pay/charge.rs"],
                "charge retry idempotency",
            ),
            memory(
                4,
                MemoryKind::MergedPr,
                &["src/auth/session.rs"],
                "session expiry",
            ),
            // Rules and runbooks of the same words are kept: curated by hand.
            memory(
                5,
                MemoryKind::BusinessRule,
                &["src/pay/charge.rs"],
                "charge retry idempotency",
            ),
            memory(
                6,
                MemoryKind::MergedPr,
                &["src/report/csv.rs"],
                "csv export",
            ),
        ];
        // The review lesson of the same fix says something else: kept.
        assert_eq!(ids(&curate(recalled, None, 4)), [1, 3, 4, 5]);
    }

    #[test]
    fn a_path_is_matched_as_a_file_or_a_directory() {
        let base = tree(&["src/a.rs", "src/lib/b.rs", "src/library.rs"]);
        assert!(exists(&base, "src/a.rs"));
        assert!(exists(&base, "./src/a.rs:10"));
        assert!(exists(&base, "src/lib"));
        assert!(exists(&base, "src/lib/"));
        assert!(!exists(&base, "src/li"));
        assert!(!exists(&base, "src/b.rs"));
        assert!(!exists(&base, ""));
    }
}
