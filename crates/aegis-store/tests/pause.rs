//! Pausing a repo (#374): its queued jobs are not claimed but wait, saying
//! why, and new ones are still recorded; running jobs end as they would.
//! Resumed, its jobs are claimed again. Both are audited.

use std::time::Duration;

use aegis_ledger::IssueId;
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, ActorKind, JobSource, JobStatus, NewJob, NewRepo, Page, RepoId, RepoMode, Store,
    StoreError,
};

const LEASE: Duration = Duration::from_secs(60);
const ANY: u32 = 10;

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 10,
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
async fn a_paused_repos_jobs_wait_and_run_again_once_resumed() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let b = repo(s, "b").await;
    let running = job(s, a, 1).await;
    let claimed = s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(claimed.job.id, running.id);
    let queued = job(s, a, 2).await;
    let other = job(s, b, 1).await;
    let admin = Actor {
        kind: ActorKind::User,
        user: None,
        name: "admin@x.test".into(),
    };

    let paused = s
        .pause_repo_by(a, Some("runs wild"), false, &admin)
        .await
        .unwrap();
    assert_eq!(paused.cancelled, 0);
    let pause = s.repo_pause(a).await.unwrap().unwrap();
    assert_eq!(
        (pause.by.as_str(), pause.reason.as_deref()),
        ("admin@x.test", Some("runs wild"))
    );
    assert!(s.repo_pause(b).await.unwrap().is_none());
    assert_eq!(s.paused_repos().await.unwrap().len(), 1);
    // Twice is refused: it says it is paused already.
    assert!(matches!(
        s.pause_repo_by(a, None, false, &admin).await,
        Err(StoreError::Conflict(_))
    ));

    // Only the other repo's job is claimed; this one waits, saying why.
    let next = s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(next.job.id, other.id);
    assert!(s.claim_job("w", LEASE, ANY).await.unwrap().is_none());
    let waiting = s.job(queued.id).await.unwrap().unwrap();
    assert_eq!(waiting.status, JobStatus::Queued);
    let why = waiting.reason.unwrap_or_default();
    assert!(why.contains("paused") && why.contains("runs wild"), "{why}");
    // New jobs are still recorded; the running one is left alone.
    let later = job(s, a, 3).await;
    assert_eq!(later.status, JobStatus::Queued);
    let still = s.job(running.id).await.unwrap().unwrap();
    assert_eq!(still.status, JobStatus::Running);

    s.resume_repo_by(a, &admin).await.unwrap();
    assert!(s.repo_pause(a).await.unwrap().is_none());
    assert!(matches!(
        s.resume_repo_by(a, &admin).await,
        Err(StoreError::Conflict(_))
    ));
    let resumed = s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(resumed.job.id, queued.id);
    assert_eq!(resumed.job.reason, None, "the waiting reason goes");
    let after = s.job(later.id).await.unwrap().unwrap();
    assert_eq!(after.reason, None, "cleared at once on resume");

    let actions: Vec<(String, Option<RepoId>)> = s
        .audit_log(None, Page::first(10))
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|e| ["repo.pause", "repo.resume"].contains(&e.action.as_str()))
        .map(|e| (e.action, e.repo_id))
        .collect();
    assert_eq!(
        actions,
        [
            ("repo.resume".to_owned(), Some(a)),
            ("repo.pause".to_owned(), Some(a))
        ]
    );
    db.drop().await;
}

#[tokio::test]
async fn pausing_can_cancel_the_queued_jobs_at_once() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let one = job(s, a, 1).await;
    let two = job(s, a, 2).await;
    let paused = s.pause_repo_by(a, None, true, &Actor::cli()).await.unwrap();
    assert_eq!(paused.cancelled, 2);
    for j in [one.id, two.id] {
        let j = s.job(j).await.unwrap().unwrap();
        assert_eq!(j.status, JobStatus::Cancelled);
        assert!(j.reason.unwrap_or_default().contains("paused"));
    }
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(entry.action, "repo.pause");
    assert_eq!(entry.detail["cancelled"], 2);
    // A reason too long is refused before anything changes.
    let b = repo(s, "b").await;
    assert!(matches!(
        s.pause_repo_by(b, Some(&"x".repeat(501)), false, &Actor::cli())
            .await,
        Err(StoreError::Invalid { .. })
    ));
    assert!(matches!(
        s.pause_repo_by(RepoId(9999), None, false, &Actor::cli())
            .await,
        Err(StoreError::NotFound(_))
    ));
    db.drop().await;
}
