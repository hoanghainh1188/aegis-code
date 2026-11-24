//! Incident & Domain Memory storage and relevance recall (#456).

use aegis_store::testing::test_db;
use aegis_store::{Actor, MemoryKind, MemoryScope, ModeName, NewRepo, NewRepoMemory, Page, RepoId};

fn repo(slug: &str) -> NewRepo {
    NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 2,
        config: serde_json::json!({}),
    }
}

fn memory(
    repo_id: RepoId,
    kind: MemoryKind,
    source_ref: &str,
    title: &str,
    summary: &str,
    paths: &[&str],
    keywords: &str,
) -> NewRepoMemory {
    NewRepoMemory {
        repo_id,
        kind,
        source_ref: source_ref.into(),
        title: title.into(),
        summary: summary.into(),
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        keywords: keywords.into(),
        job_id: None,
        scope: None,
    }
}

/// #456, through the store since #596: at least three relevant lessons, and
/// not the unrelated one.
#[tokio::test]
async fn recall_finds_the_relevant_lessons_and_not_the_unrelated_one() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let billing = s.create_repo(&repo("billing")).await.unwrap().id;
    let entries = [
        memory(
            billing,
            MemoryKind::MergedPr,
            "https://github.com/acme/billing/pull/101",
            "Fix JPY invoice rounding in tax_calc.rs",
            "Root cause: tax_calc::round_currency truncated half-yen instead of applying \
             banker's rounding on JPY tax_rate lines.",
            &["src/billing/tax_calc.rs"],
            "jpy invoice rounding tax_calc currency",
        ),
        memory(
            billing,
            MemoryKind::ReviewLesson,
            "https://github.com/acme/billing/pull/101#review-1",
            "Review lesson: preserve zero-decimal currency invariant in tax_calc",
            "Reviewer noted that JPY and KRW amounts must never emit fractional subunits \
             and must update the ledger audit trail.",
            &["src/billing/tax_calc.rs", "src/billing/ledger.rs"],
            "jpy krw zero_decimal currency tax_calc review",
        ),
        memory(
            billing,
            MemoryKind::BusinessRule,
            "rule:jpy-consumption-tax-rounding",
            "Business rule: Japanese consumption tax rounding per qualified invoice",
            "Under Japanese Qualified Invoice System, consumption tax rounding happens once \
             per tax rate bucket (8% / 10%) in tax_calc.rs, never per line item.",
            &["src/billing/tax_calc.rs"],
            "jpy invoice consumption tax bucket rounding",
        ),
        memory(
            billing,
            MemoryKind::Runbook,
            "runbook:billing-tax-mismatch",
            "Runbook: diagnosing invoice tax total mismatch",
            "Check tax_calc::summarize_buckets and verify rounding mode before touching \
             serializer code.",
            &["src/billing/tax_calc.rs"],
            "invoice tax mismatch runbook tax_calc",
        ),
        memory(
            billing,
            MemoryKind::MergedPr,
            "https://github.com/acme/billing/pull/88",
            "Fix OAuth callback cookie SameSite attribute",
            "Updated session cookie flag in src/auth/oauth.rs.",
            &["src/auth/oauth.rs"],
            "oauth cookie samesite session",
        ),
    ];
    for entry in &entries {
        s.upsert_repo_memory(entry).await.unwrap();
    }
    let recalled = s
        .recall_repo_memories(
            billing,
            "JPY invoice tax rounding off by 1 yen in src/billing/tax_calc.rs",
            "When computing 10% consumption tax on a multi-line JPY invoice, \
             tax_calc.rs produces a 1 yen mismatch.",
            5,
        )
        .await
        .unwrap();
    assert_eq!(recalled.len(), 4, "{recalled:#?}");
    assert!(recalled.iter().all(|m| !m.title.contains("OAuth")));
    db.drop().await;
}

/// #596: the store searches every row, not only the 200 newest. The one
/// lesson about the issue is the oldest of more than 500.
#[tokio::test]
async fn recall_reads_past_the_newest_hundreds() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = s.create_repo(&repo("shop")).await.unwrap().id;
    s.upsert_repo_memory(&memory(
        shop,
        MemoryKind::MergedPr,
        "https://github.com/acme/shop/pull/1",
        "Fix double refund when the payment gateway times out",
        "refund_worker retried a refund the gateway had already accepted.",
        &["src/payments/refund_worker.rs"],
        "refund gateway timeout idempotency",
    ))
    .await
    .unwrap();
    for i in 0..520 {
        s.upsert_repo_memory(&memory(
            shop,
            MemoryKind::MergedPr,
            &format!("https://github.com/acme/shop/pull/{}", i + 2),
            &format!("Tweak catalog page layout {i}"),
            "Adjusted product card spacing on the catalog page.",
            &[&format!("web/catalog/card_{i}.css")],
            "catalog layout css",
        ))
        .await
        .unwrap();
    }
    let recalled = s
        .recall_repo_memories(
            shop,
            "Customers refunded twice",
            "The gateway timed out and refund_worker.rs sent the refund again.",
            5,
        )
        .await
        .unwrap();
    assert_eq!(recalled.len(), 1, "{recalled:#?}");
    assert_eq!(
        recalled[0].source_ref,
        "https://github.com/acme/shop/pull/1"
    );
    db.drop().await;
}

/// #596: a Japanese ticket, without spaces between its words, finds a
/// lesson that shares a phrase with it.
#[tokio::test]
async fn recall_finds_japanese_text_without_spaces() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = s.create_repo(&repo("shop")).await.unwrap().id;
    s.upsert_repo_memory(&memory(
        shop,
        MemoryKind::BusinessRule,
        "rule:receipt-reissue",
        "領収書の再発行",
        "領収書を再発行するときは元の領収書番号を必ず無効にする。",
        &[],
        "",
    ))
    .await
    .unwrap();
    s.upsert_repo_memory(&memory(
        shop,
        MemoryKind::BusinessRule,
        "rule:points",
        "ポイントの有効期限",
        "ポイントは付与から一年で失効する。",
        &[],
        "",
    ))
    .await
    .unwrap();
    let recalled = s
        .recall_repo_memories(shop, "領収書を再発行すると番号が重複する", "", 5)
        .await
        .unwrap();
    assert_eq!(recalled.len(), 1, "{recalled:#?}");
    assert_eq!(recalled[0].source_ref, "rule:receipt-reissue");
    db.drop().await;
}

/// #596: a memory shared with the tenant reaches its other repos, after
/// their own memories; never another tenant's repos, and a repo-scoped one
/// stays in its repo.
#[tokio::test]
async fn tenant_memories_reach_the_tenants_other_repos_only() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    let other = s
        .create_tenant_by("other", "Other", &Actor::cli())
        .await
        .unwrap();
    let orders = s.create_repo(&repo("orders")).await.unwrap().id;
    let invoices = s.create_repo(&repo("invoices")).await.unwrap().id;
    let elsewhere = s.create_repo(&repo("elsewhere")).await.unwrap().id;
    for (r, t) in [
        (orders, acme.id),
        (invoices, acme.id),
        (elsewhere, other.id),
    ] {
        s.set_repo_tenant_by(r, t, &Actor::cli()).await.unwrap();
    }
    let shared = s
        .upsert_repo_memory(&NewRepoMemory {
            scope: Some(MemoryScope::Tenant),
            ..memory(
                orders,
                MemoryKind::BusinessRule,
                "rule:customer-id",
                "Customer ids are ULIDs across every service",
                "Never parse customer_id as an integer; services exchange ULID strings.",
                &[],
                "customer_id ulid",
            )
        })
        .await
        .unwrap();
    assert_eq!(shared.scope, MemoryScope::Tenant);
    s.upsert_repo_memory(&memory(
        orders,
        MemoryKind::Runbook,
        "runbook:customer-id-orders",
        "Orders: customer_id parsing",
        "orders keeps customer_id as a ULID string in its own tables.",
        &[],
        "customer_id ulid",
    ))
    .await
    .unwrap();
    s.upsert_repo_memory(&memory(
        invoices,
        MemoryKind::Runbook,
        "runbook:customer-id-invoices",
        "Invoices: customer_id column",
        "invoices stores customer_id ULID strings in invoice headers.",
        &[],
        "customer_id ulid",
    ))
    .await
    .unwrap();
    let issue = (
        "customer_id parsed as integer",
        "A ULID customer_id failed to parse.",
    );

    let refs = |m: Vec<aegis_store::RepoMemory>| -> Vec<String> {
        m.into_iter().map(|m| m.source_ref).collect()
    };
    let in_invoices = s
        .recall_repo_memories(invoices, issue.0, issue.1, 5)
        .await
        .unwrap();
    assert_eq!(
        refs(in_invoices),
        ["runbook:customer-id-invoices", "rule:customer-id"],
        "its own first, then the tenant's; not orders' own runbook"
    );
    let in_elsewhere = s
        .recall_repo_memories(elsewhere, issue.0, issue.1, 5)
        .await
        .unwrap();
    assert!(in_elsewhere.is_empty(), "{in_elsewhere:#?}");

    // Extracted again without a scope, the shared memory stays shared.
    let again = s
        .upsert_repo_memory(&memory(
            orders,
            MemoryKind::BusinessRule,
            "rule:customer-id",
            "Customer ids are ULIDs across every service",
            "Still ULIDs.",
            &[],
            "customer_id ulid",
        ))
        .await
        .unwrap();
    assert_eq!(again.scope, MemoryScope::Tenant);
    assert_eq!(
        s.repo_memory_scope(orders, shared.id).await.unwrap(),
        Some(MemoryScope::Tenant)
    );
    assert_eq!(
        s.repo_memory_scope(invoices, shared.id).await.unwrap(),
        None
    );
    assert_eq!(
        s.repo_memory_scope_of(orders, MemoryKind::BusinessRule, "rule:customer-id")
            .await
            .unwrap(),
        Some(MemoryScope::Tenant)
    );

    // Moved to the other tenant, orders takes its shared memory along.
    s.set_repo_tenant_by(orders, other.id, &Actor::cli())
        .await
        .unwrap();
    let in_elsewhere = s
        .recall_repo_memories(elsewhere, issue.0, issue.1, 5)
        .await
        .unwrap();
    assert_eq!(refs(in_elsewhere), ["rule:customer-id"]);
    db.drop().await;
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
            mode: ModeName::CUSTOMER_JP,
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
            mode: ModeName::POC,
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
            scope: None,
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
            scope: None,
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
        scope: None,
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
        scope: None,
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
            scope: None,
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
        scope: None,
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

/// A memory names only a job of its own repo: never another repo's, so
/// never another tenant's (#525). Refused before anything is written.
#[tokio::test]
async fn a_memory_cannot_name_another_repos_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = |slug: &'static str| NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 2,
        config: serde_json::json!({}),
    };
    let mine = s.create_repo(&repo("mine")).await.unwrap();
    let theirs = s.create_repo(&repo("theirs")).await.unwrap();
    let their_job = s
        .create_job(&aegis_store::NewJob {
            repo_id: theirs.id,
            issue_ref: "acme/theirs#1".into(),
            ledger_key: aegis_ledger::IssueId::parse("theirs-1").unwrap(),
            source: aegis_store::JobSource::Github,
            priority: None,
            sla_deadline: None,
        })
        .await
        .unwrap();
    let entry = |job| NewRepoMemory {
        repo_id: mine.id,
        kind: MemoryKind::Runbook,
        source_ref: "runbooks/restart".into(),
        title: "Restart the worker".into(),
        summary: "Drain, then restart.".into(),
        paths: vec![],
        keywords: String::new(),
        job_id: job,
        scope: None,
    };
    let refused = s
        .upsert_repo_memory_by(&entry(Some(their_job.id)), &Actor::cli())
        .await
        .unwrap_err();
    assert!(
        matches!(
            &refused,
            aegis_store::StoreError::Invalid {
                field: "job_id",
                ..
            }
        ),
        "{refused}"
    );
    let missing = s
        .upsert_repo_memory_by(&entry(Some(aegis_store::JobId(9999))), &Actor::cli())
        .await;
    assert!(missing.is_err(), "a job that does not exist is refused too");
    let none = s
        .list_repo_memories(mine.id, None, Page::first(10))
        .await
        .unwrap();
    assert!(none.items.is_empty(), "nothing written");
    assert!(
        s.upsert_repo_memory_by(&entry(None), &Actor::cli())
            .await
            .is_ok()
    );
    // Deleting a memory of another repo finds nothing and records nothing.
    let saved = s
        .list_repo_memories(mine.id, None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert!(
        !s.delete_repo_memory_by(theirs.id, saved.id, &Actor::cli())
            .await
            .unwrap()
    );
    db.drop().await;
}
