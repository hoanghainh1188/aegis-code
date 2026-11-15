//! Incident & Domain Memory storage and relevance recall (#456).

use aegis_store::testing::test_db;
use aegis_store::{
    Actor, MemoryId, MemoryKind, NewRepo, NewRepoMemory, Page, RepoId, RepoMemory, RepoMode,
    rank_memories, score_repo_memory,
};
use chrono::Utc;

fn sample_memory(
    id: i64,
    kind: MemoryKind,
    source_ref: &str,
    title: &str,
    summary: &str,
    paths: &[&str],
    keywords: &str,
) -> RepoMemory {
    let now = Utc::now();
    RepoMemory {
        id: MemoryId(id),
        repo_id: RepoId(1),
        kind,
        source_ref: source_ref.into(),
        title: title.into(),
        summary: summary.into(),
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        keywords: keywords.into(),
        job_id: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn rank_memories_recalls_at_least_three_relevant_lessons_and_excludes_unrelated() {
    let candidates = vec![
        sample_memory(
            1,
            MemoryKind::MergedPr,
            "https://github.com/acme/billing/pull/101",
            "Fix JPY invoice rounding in tax_calc.rs",
            "Root cause: tax_calc::round_currency truncated half-yen instead of applying \
             banker's rounding on JPY tax_rate lines.",
            &["src/billing/tax_calc.rs"],
            "jpy invoice rounding tax_calc currency",
        ),
        sample_memory(
            2,
            MemoryKind::ReviewLesson,
            "https://github.com/acme/billing/pull/101#review-1",
            "Review lesson: preserve zero-decimal currency invariant in tax_calc",
            "Reviewer noted that JPY and KRW amounts must never emit fractional subunits \
             and must update the ledger audit trail.",
            &["src/billing/tax_calc.rs", "src/billing/ledger.rs"],
            "jpy krw zero_decimal currency tax_calc review",
        ),
        sample_memory(
            3,
            MemoryKind::BusinessRule,
            "rule:jpy-consumption-tax-rounding",
            "Business rule: Japanese consumption tax rounding per qualified invoice",
            "Under Japanese Qualified Invoice System, consumption tax rounding happens once \
             per tax rate bucket (8% / 10%) in tax_calc.rs, never per line item.",
            &["src/billing/tax_calc.rs"],
            "jpy invoice consumption tax bucket rounding",
        ),
        sample_memory(
            4,
            MemoryKind::Runbook,
            "runbook:billing-tax-mismatch",
            "Runbook: diagnosing invoice tax total mismatch",
            "Check tax_calc::summarize_buckets and verify rounding mode before touching \
             serializer code.",
            &["src/billing/tax_calc.rs"],
            "invoice tax mismatch runbook tax_calc",
        ),
        sample_memory(
            5,
            MemoryKind::MergedPr,
            "https://github.com/acme/billing/pull/88",
            "Fix OAuth callback cookie SameSite attribute",
            "Updated session cookie flag in src/auth/oauth.rs.",
            &["src/auth/oauth.rs"],
            "oauth cookie samesite session",
        ),
    ];

    let query_title = "JPY invoice tax rounding off by 1 yen in src/billing/tax_calc.rs";
    let query_body = "When computing 10% consumption tax on a multi-line JPY invoice, \
                      tax_calc.rs produces a 1 yen mismatch.";

    let recalled = rank_memories(candidates.clone(), query_title, query_body, 5);
    assert!(
        recalled.len() >= 3,
        "expected at least 3 relevant historical lessons, got {}",
        recalled.len()
    );
    assert_eq!(recalled.len(), 4);
    assert!(
        recalled.iter().all(|m| m.id != MemoryId(5)),
        "unrelated OAuth PR should have zero overlap and be excluded"
    );
    assert_eq!(
        score_repo_memory(&candidates[4], query_title, query_body),
        0
    );
}

#[tokio::test]
async fn store_upserts_lists_recalls_and_isolates_repo_memories() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;

    let repo_a = s
        .create_repo(&NewRepo {
            slug: "billing".into(),
            url: "https://github.com/acme/billing".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::CustomerJp,
            max_concurrent_jobs: 2,
            config: serde_json::json!({}),
        })
        .await
        .unwrap();
    let repo_b = s
        .create_repo(&NewRepo {
            slug: "portal".into(),
            url: "https://github.com/acme/portal".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::Poc,
            max_concurrent_jobs: 2,
            config: serde_json::json!({}),
        })
        .await
        .unwrap();

    // Upsert all 4 memory kinds in repo_a.
    let m1 = s
        .upsert_repo_memory(&NewRepoMemory {
            repo_id: repo_a.id,
            kind: MemoryKind::MergedPr,
            source_ref: "https://github.com/acme/billing/pull/12".into(),
            title: "Fix retry backoff jitter in webhook_sender.rs".into(),
            summary: "Added bounded exponential backoff in src/webhook_sender.rs.".into(),
            paths: vec!["src/webhook_sender.rs".into()],
            keywords: "webhook retry backoff timeout".into(),
            job_id: None,
        })
        .await
        .unwrap();

    // Idempotent update on the same (repo_id, kind, source_ref).
    let m1_updated = s
        .upsert_repo_memory(&NewRepoMemory {
            repo_id: repo_a.id,
            kind: MemoryKind::MergedPr,
            source_ref: "https://github.com/acme/billing/pull/12".into(),
            title: "Fix retry backoff jitter in webhook_sender.rs".into(),
            summary: "Updated: bounded backoff + dead-letter queue in src/webhook_sender.rs."
                .into(),
            paths: vec!["src/webhook_sender.rs".into()],
            keywords: "webhook retry backoff dead_letter".into(),
            job_id: None,
        })
        .await
        .unwrap();
    assert_eq!(m1.id, m1_updated.id);
    assert!(m1_updated.summary.contains("dead-letter"));

    s.upsert_repo_memory(&NewRepoMemory {
        repo_id: repo_a.id,
        kind: MemoryKind::ReviewLesson,
        source_ref: "https://github.com/acme/billing/pull/12#round-1".into(),
        title: "Review lesson: cap webhook retry attempts at 5".into(),
        summary: "Reviewer required MAX_WEBHOOK_RETRIES = 5 in webhook_sender.rs.".into(),
        paths: vec!["src/webhook_sender.rs".into()],
        keywords: "webhook retry max_webhook_retries review".into(),
        job_id: None,
    })
    .await
    .unwrap();

    s.upsert_repo_memory(&NewRepoMemory {
        repo_id: repo_a.id,
        kind: MemoryKind::Runbook,
        source_ref: "runbook:webhook-delivery-storm".into(),
        title: "Runbook: webhook delivery retry storm".into(),
        summary: "Inspect webhook_sender queue depth and verify retry cap before replaying.".into(),
        paths: vec!["src/webhook_sender.rs".into()],
        keywords: "webhook retry storm queue".into(),
        job_id: None,
    })
    .await
    .unwrap();

    let rule = s
        .upsert_repo_memory(&NewRepoMemory {
            repo_id: repo_a.id,
            kind: MemoryKind::BusinessRule,
            source_ref: "rule:webhook-idempotency-header".into(),
            title: "Business rule: every webhook retry must preserve X-Idempotency-Key".into(),
            summary: "Never regenerate X-Idempotency-Key across retry attempts in webhook_sender."
                .into(),
            paths: vec!["src/webhook_sender.rs".into()],
            keywords: "webhook retry idempotency header".into(),
            job_id: None,
        })
        .await
        .unwrap();

    // Another repo's memory must never leak into repo_a recall.
    s.upsert_repo_memory(&NewRepoMemory {
        repo_id: repo_b.id,
        kind: MemoryKind::BusinessRule,
        source_ref: "rule:other-repo".into(),
        title: "Other repo webhook retry rule".into(),
        summary: "Belongs to repo_b in src/webhook_sender.rs.".into(),
        paths: vec!["src/webhook_sender.rs".into()],
        keywords: "webhook retry idempotency".into(),
        job_id: None,
    })
    .await
    .unwrap();

    let all_a = s
        .list_repo_memories(repo_a.id, None, Page::first(10))
        .await
        .unwrap();
    assert_eq!(all_a.items.len(), 4);

    let rules_a = s
        .list_repo_memories(repo_a.id, Some(MemoryKind::BusinessRule), Page::first(10))
        .await
        .unwrap();
    assert_eq!(rules_a.items.len(), 1);
    assert_eq!(rules_a.items[0].id, rule.id);

    let recalled = s
        .recall_repo_memories(
            repo_a.id,
            "Duplicate charges when webhook retry runs in src/webhook_sender.rs",
            "Retrying failed webhook deliveries in webhook_sender.rs drops X-Idempotency-Key.",
            5,
        )
        .await
        .unwrap();
    assert_eq!(recalled.len(), 4);
    assert!(recalled.iter().all(|m| m.repo_id == repo_a.id));

    let deleted = s
        .delete_repo_memory_by(repo_a.id, rule.id, &Actor::system())
        .await
        .unwrap();
    assert!(deleted);
    let deleted_again = s
        .delete_repo_memory_by(repo_a.id, rule.id, &Actor::system())
        .await
        .unwrap();
    assert!(!deleted_again);
}
