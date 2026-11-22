//! #526: the AMS report, per tenant and SLA level, this month: MTTR, SLA
//! on time and overdue, triage or PR, and each contract's spend.

use std::time::Duration;

use aegis_ledger::{IssueId, LedgerEvent, LedgerStore, Outcome, Tier, TokenCounts};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, AttemptEnd, JobId, JobSource, ModeName, NewJob, NewRepo, RepoId, SlaPriority, Store,
    TenantId,
};
use time::OffsetDateTime;

const LEASE: Duration = Duration::from_secs(60);

async fn repo_in(s: &Store, slug: &str, tenant: TenantId) -> RepoId {
    s.create_repo_in_by(
        &NewRepo {
            slug: slug.into(),
            url: format!("https://github.com/example/{slug}"),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: ModeName::POC,
            max_concurrent_jobs: 10,
            config: serde_json::json!({}),
        },
        tenant,
        &Actor::cli(),
    )
    .await
    .unwrap()
    .id
}

async fn job(
    s: &Store,
    repo: RepoId,
    n: u32,
    priority: Option<SlaPriority>,
    due_in_min: Option<i64>,
) -> JobId {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("example/r{}#{n}", repo.0),
        ledger_key: IssueId::parse(format!("r{}-{n}", repo.0)).unwrap(),
        source: JobSource::Jsm,
        priority,
        sla_deadline: due_in_min.map(|m| OffsetDateTime::now_utc() + time::Duration::minutes(m)),
    })
    .await
    .unwrap()
    .id
}

/// Claims the next job and ends it with `outcome`.
async fn end_next(s: &Store, outcome: Outcome) -> JobId {
    let c = s.claim_job("w", LEASE, 10).await.unwrap().unwrap();
    let end = AttemptEnd::Done {
        outcome,
        reason: None,
        pr_url: None,
        patch: None,
        diagnosis: None,
    };
    s.finish_attempt(&c.run, "w", &end).await.unwrap();
    c.job.id
}

#[tokio::test]
async fn the_report_counts_mttr_sla_and_triage_per_tenant_and_level() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    s.set_tenant_limits_by(acme.id, Some(100.0), None, &Actor::cli())
        .await
        .unwrap();
    let a = repo_in(s, "a", acme.id).await;
    let b = repo_in(s, "b", TenantId::DEFAULT).await;

    // Acme P1: one due in an hour, one already past its deadline; claimed
    // by the sooner deadline, they end as a PR and a diagnosis.
    job(s, a, 1, Some(SlaPriority::P1), Some(60)).await;
    job(s, a, 2, Some(SlaPriority::P1), Some(-5)).await;
    // Default, no level: a stop.
    job(s, b, 1, None, None).await;
    let pr = end_next(s, Outcome::PrOpened).await;
    end_next(s, Outcome::Diagnosed).await;
    end_next(s, Outcome::StoppedSteps).await;
    // The first one's ticket came in two hours ago.
    db.raw("UPDATE jobs SET created_at = now() - interval '2 hours' WHERE issue_ref LIKE 'example/r%#1' AND source = 'jsm' AND id IN (SELECT id FROM jobs ORDER BY id LIMIT 1)")
        .await
        .unwrap();
    // Still open and already past its deadline.
    job(s, a, 3, Some(SlaPriority::P2), Some(-1)).await;
    // What Acme spent this month.
    let spent = LedgerEvent::ModelCall {
        tier: Tier::Core,
        model: "m".into(),
        tokens: TokenCounts::default(),
        cost_usd: 12.5,
    };
    let key = s.job(pr).await.unwrap().unwrap().ledger_key;
    s.ledger().append(&key, &spent).await.unwrap();

    let report = s.ams_report(None).await.unwrap();
    let row = |t: &str, p: &str| {
        report
            .rows
            .iter()
            .find(|r| r.tenant == t && r.priority == p)
            .cloned()
            .unwrap_or_else(|| panic!("no row {t} {p}: {report:?}"))
    };
    let p1 = row("acme", "P1");
    assert_eq!((p1.done, p1.prs, p1.diagnosed), (2, 1, 1));
    assert_eq!(
        (p1.with_deadline, p1.on_time),
        (2, 1),
        "the late one is counted late"
    );
    assert_eq!(p1.on_time_ratio(), Some(0.5));
    // Two hours and a few seconds: about an hour on average.
    let mttr = p1.mttr_s.unwrap();
    assert!((3500.0..3700.0).contains(&mttr), "{mttr}");
    let p2 = row("acme", "P2");
    assert_eq!((p2.done, p2.overdue_open), (0, 1));
    let none = row("default", "none");
    assert_eq!(
        (none.done, none.prs, none.diagnosed, none.with_deadline),
        (1, 0, 0, 0)
    );
    let total = report.tenant_total("acme");
    assert_eq!((total.done, total.overdue_open), (2, 1));

    let contract = report.contracts.iter().find(|c| c.slug == "acme").unwrap();
    assert_eq!(
        (contract.monthly_usd, contract.spent_usd),
        (Some(100.0), 12.5)
    );

    // Within a repo: only its tenant, its rows and its contract.
    let mine = s.ams_report(Some(&[b])).await.unwrap();
    assert!(mine.rows.iter().all(|r| r.tenant == "default"), "{mine:?}");
    assert_eq!(mine.contracts.len(), 1);
    db.drop().await;
}
