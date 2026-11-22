//! #524: queued jobs are claimed by SLA level, P1 first, then by the
//! sooner deadline, then in order; each `sla_aging` a job has waited lifts
//! it one level, so none waits for ever. Without SLAs the queue is FIFO.

use std::time::Duration;

use aegis_ledger::IssueId;
use aegis_store::testing::test_db;
use aegis_store::{JobId, JobSource, ModeName, NewJob, NewRepo, RepoId, SlaPriority, Store};
use chrono::{TimeDelta, Utc};

const LEASE: Duration = Duration::from_secs(60);
const ANY: u32 = 10;

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 10,
        config: serde_json::json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(
    s: &Store,
    repo: RepoId,
    n: u32,
    priority: Option<SlaPriority>,
    due_in_h: Option<i64>,
) -> JobId {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("example/r{}#{n}", repo.0),
        ledger_key: IssueId::parse(format!("r{}-{n}", repo.0)).unwrap(),
        source: JobSource::Jsm,
        priority,
        sla_deadline: due_in_h.map(|h| Utc::now() + TimeDelta::hours(h)),
    })
    .await
    .unwrap()
    .id
}

async fn order(s: &Store) -> Vec<JobId> {
    let mut claimed = Vec::new();
    while let Some(c) = s.claim_job("w", LEASE, ANY).await.unwrap() {
        claimed.push(c.job.id);
    }
    claimed
}

#[tokio::test]
async fn p1_goes_before_p3_and_the_sooner_deadline_first() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let none = job(s, a, 1, None, None).await;
    let p3 = job(s, a, 2, Some(SlaPriority::P3), Some(48)).await;
    let p2 = job(s, a, 3, Some(SlaPriority::P2), Some(24)).await;
    let p1_late = job(s, a, 4, Some(SlaPriority::P1), Some(8)).await;
    let p1_soon = job(s, a, 5, Some(SlaPriority::P1), Some(2)).await;
    assert_eq!(order(s).await, [p1_soon, p1_late, p2, p3, none]);
    db.drop().await;
}

#[tokio::test]
async fn without_slas_the_queue_stays_fifo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let b = repo(s, "b").await;
    let mut made = Vec::new();
    for n in 0..4 {
        made.push(job(s, if n % 2 == 0 { a } else { b }, n, None, None).await);
    }
    assert_eq!(order(s).await, made);
    db.drop().await;
}

#[tokio::test]
async fn a_job_that_waited_long_enough_goes_before_a_new_p1() {
    let Some(db) = test_db().await else { return };
    let s = db.store.clone().with_sla_aging(Duration::from_secs(3600));
    let a = repo(&s, "a").await;
    let old = job(&s, a, 1, Some(SlaPriority::P3), None).await;
    // Two hours of waiting lift P3 two levels, to P1; it came first.
    db.raw("UPDATE jobs SET created_at = now() - interval '2 hours 1 minute' WHERE issue_ref = 'example/r1#1'")
        .await
        .unwrap();
    let fresh = job(&s, a, 2, Some(SlaPriority::P1), None).await;
    let fresh_p2 = job(&s, a, 3, Some(SlaPriority::P2), None).await;
    assert_eq!(order(&s).await, [old, fresh, fresh_p2]);

    // Aged only one level, a P3 stays behind a new P1.
    let p3 = job(&s, a, 4, Some(SlaPriority::P3), None).await;
    db.raw("UPDATE jobs SET created_at = now() - interval '1 hour 1 minute' WHERE issue_ref = 'example/r1#4'")
        .await
        .unwrap();
    let p1 = job(&s, a, 5, Some(SlaPriority::P1), None).await;
    assert_eq!(order(&s).await, [p1, p3]);
    db.drop().await;
}

#[tokio::test]
async fn the_queue_reports_ready_jobs_by_level() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    job(s, a, 1, Some(SlaPriority::P1), None).await;
    job(s, a, 2, Some(SlaPriority::P1), None).await;
    job(s, a, 3, None, None).await;
    let stats = s.queue_stats().await.unwrap();
    let ready: Vec<(&str, i64)> = stats
        .by_priority
        .iter()
        .map(|l| (l.priority, l.ready))
        .collect();
    assert_eq!(ready, [("P1", 2), ("P2", 0), ("P3", 0), ("none", 1)]);
    db.drop().await;
}
