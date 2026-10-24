//! The job queue (#23): claims under limits, leases, reclaim, cancel, retry.

use std::time::Duration;

use aegis_ledger::{IssueId, LedgerEvent, LedgerStore, Outcome};
use aegis_store::testing::test_db;
use aegis_store::{
    AttemptEnd, JobSource, JobStatus, Lease, NewJob, NewRepo, Page, RepoId, RepoMode, RunStatus,
    Store, StoreError,
};

const LEASE: Duration = Duration::from_secs(60);

/// `max_workers` high enough not to matter.
const ANY: u32 = 10;

async fn repo(s: &Store, slug: &str) -> RepoId {
    limited(s, slug, 10).await
}

/// A repo that runs at most `max` of its jobs at once.
async fn limited(s: &Store, slug: &str, max: u32) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: max,
        config: serde_json::json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(s: &Store, repo: RepoId, n: u32) -> aegis_store::Job {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("example/r{}#{n}", repo.0),
        ledger_key: IssueId::parse(format!("r{}-{n}", repo.0)).unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn claims_take_the_oldest_job_within_the_repo_and_worker_limits() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // Repo a takes one job at a time.
    let a = limited(s, "a", 1).await;
    let b = repo(s, "b").await;
    let a1 = job(s, a, 1).await;
    let a2 = job(s, a, 2).await;
    let b1 = job(s, b, 1).await;

    let first = s.claim_job("w1", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(first.job.id, a1.id);
    assert_eq!(
        (first.job.status, first.job.attempts),
        (JobStatus::Running, 1)
    );
    assert_eq!(first.job.locked_by.as_deref(), Some("w1"));
    assert_eq!(
        (first.run.attempt, first.run.status),
        (1, RunStatus::Running)
    );
    let second = s.claim_job("w2", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(second.job.id, b1.id, "a2 waits for a1: a is at its limit");
    assert!(s.claim_job("w2", LEASE, ANY).await.unwrap().is_none());

    // A worker at `max_workers` claims nothing more.
    s.finish_attempt(&first.run, "w1", &AttemptEnd::Released)
        .await
        .unwrap();
    let one = 1;
    assert!(s.claim_job("w2", LEASE, one).await.unwrap().is_none());
    let again = s.claim_job("w1", LEASE, one).await.unwrap().unwrap();
    assert_eq!((again.job.id, again.run.attempt), (a1.id, 2));
    assert_ne!(again.job.id, a2.id);
    db.drop().await;
}

#[tokio::test]
async fn concurrent_claims_never_share_a_job_or_break_a_repo_limit() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = limited(s, "a", 2).await;
    let b = limited(s, "b", 3).await;
    for n in 0..10 {
        job(s, if n % 2 == 0 { a } else { b }, n).await;
    }
    let claims = futures_join(s, 12).await;
    let mut ids: Vec<_> = claims.iter().map(|c| c.job.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), claims.len(), "no job claimed twice");
    let of = |r: RepoId| claims.iter().filter(|c| c.job.repo_id == r).count();
    assert_eq!((of(a), of(b)), (2, 3));
    db.drop().await;
}

async fn futures_join(s: &Store, workers: usize) -> Vec<aegis_store::Claim> {
    let tasks: Vec<_> = (0..workers)
        .map(|w| {
            let s = s.clone();
            tokio::spawn(async move { s.claim_job(&format!("w{w}"), LEASE, ANY).await.unwrap() })
        })
        .collect();
    let mut claims = Vec::new();
    for t in tasks {
        claims.extend(t.await.unwrap());
    }
    claims
}

#[tokio::test]
async fn only_the_lease_holder_renews_or_ends_a_run() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    job(s, a, 1).await;
    let c = s.claim_job("w1", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(
        s.renew_lease(c.job.id, "w1", LEASE).await.unwrap(),
        Lease::Held {
            cancel_requested: false
        }
    );
    assert_eq!(
        s.renew_lease(c.job.id, "w2", LEASE).await.unwrap(),
        Lease::Lost
    );
    let done = AttemptEnd::Done {
        outcome: Outcome::PrOpened,
        reason: None,
        pr_url: Some("https://github.com/example/a/pull/1".into()),
    };
    let err = s.finish_attempt(&c.run, "w2", &done).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");

    let job = s.finish_attempt(&c.run, "w1", &done).await.unwrap();
    assert_eq!(
        (job.status, job.outcome, job.locked_by),
        (JobStatus::Done, Some(Outcome::PrOpened), None)
    );
    assert_eq!(
        job.pr_url.as_deref(),
        Some("https://github.com/example/a/pull/1")
    );
    assert_eq!(
        s.renew_lease(c.job.id, "w1", LEASE).await.unwrap(),
        Lease::Lost
    );
    let err = s.finish_attempt(&c.run, "w1", &done).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "ended once: {err}");
    db.drop().await;
}

#[tokio::test]
async fn an_infrastructure_failure_waits_its_backoff_then_runs_again() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    job(s, a, 1).await;
    let c = s.claim_job("w1", LEASE, ANY).await.unwrap().unwrap();
    let retry = AttemptEnd::Retry {
        error: "docker: container died".into(),
        after: Duration::from_millis(300),
    };
    let queued = s.finish_attempt(&c.run, "w1", &retry).await.unwrap();
    assert_eq!((queued.status, queued.retries), (JobStatus::Queued, 1));
    assert_eq!(queued.reason.as_deref(), Some("docker: container died"));
    assert!(
        s.claim_job("w1", LEASE, ANY).await.unwrap().is_none(),
        "still in its backoff"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    let again = s.claim_job("w1", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(again.run.attempt, 2);
    let runs = s.list_runs(c.job.id, Page::first(10)).await.unwrap();
    assert_eq!(runs.items[0].status, RunStatus::Failed);
    assert_eq!(
        runs.items[0].error.as_deref(),
        Some("docker: container died")
    );
    db.drop().await;
}

#[tokio::test]
async fn an_expired_lease_interrupts_the_run_and_requeues_until_retries_are_spent() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let j = job(s, a, 1).await;
    let short = Duration::from_millis(50);
    for attempt in 1..=3 {
        let c = s.claim_job("dead", short, ANY).await.unwrap().unwrap();
        assert_eq!(c.run.attempt, attempt);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(s.reclaim_expired(2).await.unwrap(), [j.id]);
        let err = s
            .finish_attempt(&c.run, "dead", &AttemptEnd::Released)
            .await;
        assert!(err.is_err(), "the dead worker no longer holds it");
    }
    let failed = s.job(j.id).await.unwrap().unwrap();
    assert_eq!((failed.status, failed.retries), (JobStatus::Failed, 3));
    assert_eq!(
        failed.reason.as_deref(),
        Some("lease expired with no retry left")
    );
    let runs = s.list_runs(j.id, Page::first(10)).await.unwrap();
    assert!(
        runs.items
            .iter()
            .all(|r| r.status == RunStatus::Interrupted)
    );
    assert!(s.reclaim_expired(2).await.unwrap().is_empty());
    db.drop().await;
}

#[tokio::test]
async fn a_cancel_ends_a_queued_job_and_marks_a_running_one() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let queued = job(s, a, 1).await;
    let running = job(s, a, 2).await;
    let cancelled = s.request_cancel(queued.id).await.unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    let c = s.claim_job("w1", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(c.job.id, running.id, "a cancelled job is never claimed");

    let marked = s.request_cancel(running.id).await.unwrap();
    assert_eq!(
        (marked.status, marked.cancel_requested),
        (JobStatus::Running, true)
    );
    assert_eq!(
        s.renew_lease(running.id, "w1", LEASE).await.unwrap(),
        Lease::Held {
            cancel_requested: true
        }
    );
    let end = AttemptEnd::Cancelled {
        reason: "cancelled by request".into(),
    };
    s.finish_attempt(&c.run, "w1", &end).await.unwrap();
    let err = s.request_cancel(running.id).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");
    let err = s
        .request_cancel(aegis_store::JobId(9_999))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
    db.drop().await;
}

#[tokio::test]
async fn a_retry_is_a_new_job_with_its_own_ledger() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let first = job(s, a, 1).await;
    let err = s.retry_job(first.id).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "a live job: {err}");
    s.request_cancel(first.id).await.unwrap();

    let second = s.retry_job(first.id).await.unwrap();
    assert_eq!(second.retry_of, Some(first.id));
    assert_eq!(second.ledger_key.as_str(), format!("R{}-1-R1", a.0));
    assert_eq!(
        (second.issue_ref, second.status),
        (first.issue_ref, JobStatus::Queued)
    );
    let err = s.retry_job(first.id).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(_)),
        "one live job per issue: {err}"
    );

    s.request_cancel(second.id).await.unwrap();
    let third = s.retry_job(second.id).await.unwrap();
    assert_eq!(third.ledger_key.as_str(), format!("R{}-1-R2", a.0));
    db.drop().await;
}

#[tokio::test]
async fn a_jobs_spend_is_summed_over_its_ledger() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let key = IssueId::parse("typst-1").unwrap();
    assert_eq!(s.job_spent(&key).await.unwrap(), 0.0);
    let ledger = s.ledger();
    let call = |cost_usd: f64| LedgerEvent::ModelCall {
        tier: aegis_ledger::Tier::Core,
        model: "m".into(),
        tokens: aegis_ledger::TokenCounts::default(),
        cost_usd,
    };
    ledger.append(&key, &call(1.25)).await.unwrap();
    ledger.append(&key, &call(0.5)).await.unwrap();
    assert!((s.job_spent(&key).await.unwrap() - 1.75).abs() < 1e-9);
    db.drop().await;
}
