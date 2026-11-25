//! What memory learns beyond merged PRs (#657):
//! - an agent PR a reviewer rejected, with their reasons, becomes a
//!   `rejected_pattern`: a pitfall the agent is shown apart from the fixes
//!   it may follow;
//! - a read-only diagnosis (#451) an operator confirmed becomes a
//!   `confirmed_diagnosis`;
//! - the design of a feature a person approved (#677) becomes a
//!   `business_rule` once its code is merged: the contracts it set and the
//!   corrections people asked of its plan.
//!
//! Both are extracted by the repo's mode's Light model when it has one
//! (with its residency, ADR 0018), else from the ledger alone, and are
//! idempotent on their source: extracted again, they are updated.

use aegis_ledger::{IssueRecord, LedgerEvent, LedgerStore, Outcome};
use aegis_lifecycle::{
    DiagnosisFacts, FeatureDesignFacts, FeaturePlan, LearnedMemory, Modes, RejectedPrFacts,
    extract_confirmed_diagnosis, extract_feature_design, extract_rejected_pattern,
    fallback_confirmed_diagnosis, fallback_feature_design, fallback_rejected_pattern,
};
use aegis_provider::Provider;
use aegis_secrets::Secrets;
use aegis_store::{Actor, Job, MemoryKind, NewRepoMemory, RepoMemory, Store, StoreError};

use crate::memory::{light_of, read_record, summarize_review_rounds, summarize_root_record};

/// The most of a reviewer's reason kept: what a form or an event carries
/// is bounded before it is read.
pub const MAX_REJECTION_NOTE_CHARS: usize = 2000;

/// The source of the pitfall learned from the rejected PR `pr_url`.
pub fn rejected_ref(pr_url: &str) -> String {
    format!("{pr_url}#rejected")
}

/// The source of the memory learned from the diagnosis of `job`.
pub fn diagnosis_ref(job: &Job) -> String {
    format!("diagnosis:job/{}", job.id.0)
}

/// The source of the business rule learned from the design of the feature
/// `root` planned.
pub fn design_ref(root: &Job) -> String {
    format!("feature:job/{}#design", root.id.0)
}

/// The Light model of `job`'s repo's mode, when the modes have one.
async fn light_for(
    store: &Store,
    job: &Job,
    modes: Option<&Modes>,
    secrets: &Secrets,
) -> Result<Option<(Provider, String)>, StoreError> {
    Ok(match store.repo(job.repo_id).await? {
        Some(repo) => light_of(modes, secrets, &repo).await,
        None => None,
    })
}

/// [`learn_rejected_pr`] with the Light model of the PR's repo's mode.
pub async fn extract_and_store_rejected_pr(
    store: &Store,
    pr_url: &str,
    note: Option<&str>,
    modes: Option<&Modes>,
    secrets: &Secrets,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    let Some(root) = store.job_by_pr_url(pr_url).await? else {
        return Ok(None);
    };
    let light = light_for(store, &root, modes, secrets).await?;
    let light = light.as_ref().map(|(p, m)| (p, m.as_str()));
    learn_rejected_pr(store, pr_url, note, light, actor).await
}

/// Stores the pitfall of the rejected PR `pr_url`: what the agent tried,
/// and why reviewers turned it down, from `note` (the reviewer's reason)
/// and the PR's review rounds, which only people who can write to the
/// repo can ask for (#244). `None` when no job opened the PR, or when no
/// one said why: a PR closed without a word teaches nothing. Without a
/// `note`, a pitfall already kept for the PR is left as it is.
pub async fn learn_rejected_pr(
    store: &Store,
    pr_url: &str,
    note: Option<&str>,
    light: Option<(&Provider, &str)>,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    let Some(root) = store.job_by_pr_url(pr_url).await? else {
        return Ok(None);
    };
    let note: Option<String> = note
        .map(|n| n.trim().chars().take(MAX_REJECTION_NOTE_CHARS).collect())
        .filter(|n: &String| !n.is_empty());
    // A PR closed on its forge says nothing of why: what a reviewer wrote
    // when they rejected it on the dashboard is kept.
    if note.is_none()
        && store
            .repo_memory_scope_of(
                root.repo_id,
                MemoryKind::RejectedPattern,
                &rejected_ref(pr_url),
            )
            .await?
            .is_some()
    {
        return Ok(None);
    }
    let mut round_records = Vec::new();
    for round in store.review_rounds_of(root.id).await? {
        if let Some(rec) = read_record(store, &round.ledger_key).await? {
            round_records.push(rec);
        }
    }
    let (notes, round_paths) = summarize_review_rounds(&round_records);
    if note.is_none() && notes.is_empty() {
        return Ok(None);
    }
    let root_rec = read_record(store, &root.ledger_key).await?;
    let (title, cause, changes, mut paths) =
        summarize_root_record(&root.issue_ref, root_rec.as_ref());
    paths.extend(round_paths);
    paths.sort();
    paths.dedup();
    let facts = RejectedPrFacts {
        issue_title: &title,
        issue_body: &root.issue_ref,
        attempted_cause: cause.as_deref(),
        attempted_changes: changes.as_deref(),
        review_notes: &notes,
        rejection_note: note.as_deref(),
        changed_paths: &paths,
    };
    let learned = match light {
        Some((provider, model)) => extract_rejected_pattern(provider, model, &facts).await.0,
        None => fallback_rejected_pattern(&facts),
    };
    store_learned(
        store,
        &root,
        MemoryKind::RejectedPattern,
        rejected_ref(pr_url),
        learned,
        actor,
    )
    .await
    .map(Some)
}

/// [`learn_confirmed_diagnosis`] with the Light model of the job's repo's
/// mode.
pub async fn extract_and_store_confirmed_diagnosis(
    store: &Store,
    job: &Job,
    modes: Option<&Modes>,
    secrets: &Secrets,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    let light = light_for(store, job, modes, secrets).await?;
    let light = light.as_ref().map(|(p, m)| (p, m.as_str()));
    learn_confirmed_diagnosis(store, job, light, actor).await
}

/// The category the diagnosis lane took the ticket as.
fn category_of(events: &[LedgerEvent], record: Option<&IssueRecord>) -> Option<&'static str> {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            LedgerEvent::Diagnosed { category, .. } => Some(category.as_str()),
            _ => None,
        })
        .or_else(|| record?.triage.as_ref().map(|t| t.category.as_str()))
}

/// Stores what the confirmed diagnosis of `job` teaches: the symptom, its
/// operational root cause and the remediation, from the report written
/// back to the ticket. `None` when `job` did not end diagnosed or kept no
/// report.
pub async fn learn_confirmed_diagnosis(
    store: &Store,
    job: &Job,
    light: Option<(&Provider, &str)>,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    if job.outcome != Some(Outcome::Diagnosed) {
        return Ok(None);
    }
    let Some(report) = store.diagnosis(job.id).await? else {
        return Ok(None);
    };
    let events = store.ledger().events(&job.ledger_key).await?;
    let record =
        (!events.is_empty()).then(|| IssueRecord::from_events(job.ledger_key.clone(), &events));
    let title = record
        .as_ref()
        .and_then(|r| r.issue_title.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| job.issue_ref.clone());
    let facts = DiagnosisFacts {
        issue_title: &title,
        issue_body: &job.issue_ref,
        category: category_of(&events, record.as_ref()),
        report: &report,
    };
    let learned = match light {
        Some((provider, model)) => extract_confirmed_diagnosis(provider, model, &facts).await.0,
        None => fallback_confirmed_diagnosis(&facts),
    };
    store_learned(
        store,
        job,
        MemoryKind::ConfirmedDiagnosis,
        diagnosis_ref(job),
        learned,
        actor,
    )
    .await
    .map(Some)
}

async fn store_learned(
    store: &Store,
    job: &Job,
    kind: MemoryKind,
    source_ref: String,
    learned: LearnedMemory,
    actor: &Actor,
) -> Result<RepoMemory, StoreError> {
    store
        .upsert_repo_memory_by(
            &NewRepoMemory {
                repo_id: job.repo_id,
                kind,
                source_ref,
                title: learned.title,
                summary: learned.summary,
                paths: learned.paths,
                keywords: learned.keywords,
                job_id: Some(job.id),
                scope: None,
            },
            actor,
        )
        .await
}

/// [`extract_and_store_rejected_pr`] with what `sync` reads modes and
/// secrets with; without it, from the ledger alone.
pub async fn rejected_pr_with(
    store: &Store,
    pr_url: &str,
    note: Option<&str>,
    sync: Option<&crate::memory_sync::MemorySync>,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    match sync {
        Some(s) => {
            extract_and_store_rejected_pr(store, pr_url, note, Some(&s.modes), &s.secrets, actor)
                .await
        }
        None => learn_rejected_pr(store, pr_url, note, None, actor).await,
    }
}

/// [`extract_and_store_confirmed_diagnosis`] with what `sync` reads modes
/// and secrets with; without it, from the ledger alone.
pub async fn confirmed_diagnosis_with(
    store: &Store,
    job: &Job,
    sync: Option<&crate::memory_sync::MemorySync>,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    match sync {
        Some(s) => {
            extract_and_store_confirmed_diagnosis(store, job, Some(&s.modes), &s.secrets, actor)
                .await
        }
        None => learn_confirmed_diagnosis(store, job, None, actor).await,
    }
}

/// Whether the diagnosis of `job` is kept as a memory already.
pub async fn diagnosis_confirmed(store: &Store, job: &Job) -> Result<bool, StoreError> {
    Ok(store
        .repo_memory_scope_of(
            job.repo_id,
            MemoryKind::ConfirmedDiagnosis,
            &diagnosis_ref(job),
        )
        .await?
        .is_some())
}

/// [`learn_feature_design`] for the merged PR `pr_url`, with the Light
/// model of its repo's mode. `None` when no job opened it or it is not a
/// feature's.
pub async fn extract_and_store_feature_design(
    store: &Store,
    pr_url: &str,
    modes: Option<&Modes>,
    secrets: &Secrets,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    let Some(job) = store.job_by_pr_url(pr_url).await? else {
        return Ok(None);
    };
    let light = light_for(store, &job, modes, secrets).await?;
    let light = light.as_ref().map(|(p, m)| (p, m.as_str()));
    learn_feature_design(store, &job, light, actor).await
}

/// Stores what the design of the feature `job` belongs to teaches, once
/// code of it is merged (#677): the job that planned it, or one of its
/// sub-jobs. Its approved plan's contracts and criteria, and the feedback
/// its revisions were asked with, become one `business_rule`, updated if
/// learned again. `None` when `job` is no approved feature's.
pub async fn learn_feature_design(
    store: &Store,
    job: &Job,
    light: Option<(&Provider, &str)>,
    actor: &Actor,
) -> Result<Option<RepoMemory>, StoreError> {
    let root = match store.job_links(job.id).await?.parent {
        Some(parent) => match store.job(parent).await? {
            Some(root) => root,
            None => return Ok(None),
        },
        None => job.clone(),
    };
    if store.approval(root.id).await?.is_none() {
        return Ok(None);
    }
    let Some(kept) = store.feature_plan(root.id).await? else {
        return Ok(None);
    };
    let Ok(plan) = serde_json::from_value::<FeaturePlan>(kept.plan) else {
        tracing::warn!(job = root.id.0, "the approved plan cannot be read");
        return Ok(None);
    };
    let corrections: Vec<String> = store
        .revision_requests(root.id)
        .await?
        .into_iter()
        .map(|r| r.feedback)
        .collect();
    let criteria: Vec<String> = plan
        .criteria
        .iter()
        .map(|c| format!("Given {}, when {}, then {}.", c.given, c.when, c.then))
        .collect();
    let mut paths: Vec<String> = plan.sub_jobs.iter().flat_map(|j| j.scope.clone()).collect();
    paths.sort();
    paths.dedup();
    let record = read_record(store, &root.ledger_key).await?;
    let title = record
        .and_then(|r| r.issue_title)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| root.issue_ref.clone());
    let facts = FeatureDesignFacts {
        issue_title: &title,
        summary: &plan.summary,
        contracts: &plan.contracts,
        criteria: &criteria,
        corrections: &corrections,
        paths: &paths,
    };
    let learned = match light {
        Some((provider, model)) => extract_feature_design(provider, model, &facts).await.0,
        None => fallback_feature_design(&facts),
    };
    store_learned(
        store,
        &root,
        MemoryKind::BusinessRule,
        design_ref(&root),
        learned,
        actor,
    )
    .await
    .map(Some)
}
