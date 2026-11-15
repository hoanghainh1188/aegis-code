//! Server-side Incident & Domain Memory orchestration (#456):
//! - Automatic extraction of `merged_pr` and `review_lesson` entries when an
//!   agent PR is merged (via `Tier::Light` with deterministic ledger fallback).
//! - Recall of top relevant repository memories into `IssueInput` (`BP3`)
//!   before triage, diagnosis, and fix execution.

use aegis_ledger::{AnswerChange, IssueId, IssueRecord, LedgerStore};
use aegis_lifecycle::Modes;
use aegis_lifecycle::{
    IssueInput, MemoryLesson, extract_from_merged_pr, fallback_extract_from_merged_pr,
};
use aegis_provider::Provider;
use aegis_secrets::Secrets;
use aegis_store::{
    Actor, MemoryKind, NewRepoMemory, Repo, RepoId, RepoMemory, RepoMode, Store, StoreError,
};

/// Maximum number of relevant repository memories recalled into `BP3` per job.
pub const DEFAULT_RECALL_LIMIT: usize = 5;

/// Recalls relevant historical lessons (`merged_pr`, `review_lesson`,
/// `runbook`, `business_rule`) for `repo_id` and attaches them to `issue`
/// (`BP3` and triage inputs) while leaving `BP2` untouched (#456).
pub async fn enrich_with_memory(store: &Store, repo_id: RepoId, issue: &mut IssueInput) {
    let recalled = match store
        .recall_repo_memories(repo_id, &issue.title, &issue.body, DEFAULT_RECALL_LIMIT)
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
async fn light_of(
    modes: Option<&Modes>,
    secrets: &Secrets,
    repo: &Repo,
) -> Option<(Provider, String)> {
    let mode = match repo.mode {
        RepoMode::Poc => modes?.poc.as_ref(),
        RepoMode::CustomerJp => modes?.customer_jp.as_ref(),
    }?;
    match Provider::from_config_with_secrets(&mode.provider, secrets).await {
        Ok(provider) => Some((provider, mode.models.light.id.clone())),
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

    let extracted = match light {
        Some((provider, model)) => {
            let (ext, _) = extract_from_merged_pr(
                provider,
                model,
                &issue_title,
                &root.issue_ref,
                answer_cause.as_deref(),
                answer_changes.as_deref(),
                &review_notes,
                &changed_paths,
            )
            .await;
            ext
        }
        None => fallback_extract_from_merged_pr(
            &issue_title,
            &root.issue_ref,
            answer_cause.as_deref(),
            answer_changes.as_deref(),
            &review_notes,
            &changed_paths,
        ),
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
                },
                actor,
            )
            .await?;
        saved.push(lesson_entry);
    }

    Ok(saved)
}

async fn read_record(store: &Store, key: &IssueId) -> Result<Option<IssueRecord>, StoreError> {
    let events = store.ledger().events(key).await?;
    if events.is_empty() {
        return Ok(None);
    }
    Ok(Some(IssueRecord::from_events(key.clone(), &events)))
}

fn summarize_root_record(
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

fn summarize_review_rounds(rounds: &[IssueRecord]) -> (Vec<String>, Vec<String>) {
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
