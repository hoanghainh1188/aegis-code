//! Pausing every agent (#510): while paused no queued job is claimed, and
//! new ones are still recorded; an emergency pause also asks the running
//! ones to cancel. Resumed, every queued job is claimed again. Audited.

use std::time::Duration;

use aegis_ledger::IssueId;
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, ActorKind, JobSource, JobStatus, Lease, ModeName, NewJob, NewRepo, Page, PauseLevel,
    RepoId, Store, StoreError,
};

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

async fn job(s: &Store, repo: RepoId, n: u32) -> aegis_store::Job {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("example/r{}#{n}", repo.0),
        ledger_key: IssueId::parse(format!("r{}-{n}", repo.0)).unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
    .unwrap()
}

fn admin() -> Actor {
    Actor {
        kind: ActorKind::User,
        user: None,
        name: "admin@x.test".into(),
    }
}

async fn reason(s: &Store, job: aegis_store::JobId) -> String {
    s.job(job)
        .await
        .unwrap()
        .unwrap()
        .reason
        .unwrap_or_default()
}

#[tokio::test]
async fn an_intake_pause_holds_every_queued_job_until_resumed() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let b = repo(s, "b").await;
    let running = job(s, a, 1).await;
    s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    let queued_a = job(s, a, 2).await;
    let queued_b = job(s, b, 1).await;

    let paused = s
        .pause_instance_by(PauseLevel::Intake, Some("model acts strangely"), &admin())
        .await
        .unwrap();
    assert_eq!(paused.cancelled, 0);
    let pause = s.instance_pause().await.unwrap().unwrap();
    assert_eq!(pause.level, PauseLevel::Intake);
    assert_eq!(pause.by, "admin@x.test");
    // Said at once, before any claim looks.
    let why = reason(s, queued_b.id).await;
    assert!(
        why.starts_with("waiting: all agents are paused") && why.contains("model acts"),
        "{why}"
    );
    assert!(matches!(
        s.pause_instance_by(PauseLevel::Intake, None, &admin())
            .await,
        Err(StoreError::Conflict(_))
    ));

    // No repo's job is claimed; a job recorded meanwhile waits and says so.
    assert!(s.claim_job("w", LEASE, ANY).await.unwrap().is_none());
    let later = job(s, b, 2).await;
    assert_eq!(later.status, JobStatus::Queued);
    assert!(s.claim_job("w", LEASE, ANY).await.unwrap().is_none());
    assert!(reason(s, later.id).await.contains("all agents are paused"));
    // The running job is left alone: no cancel is asked for.
    assert_eq!(
        s.renew_lease(running.id, "w", LEASE).await.unwrap(),
        Lease::Held {
            cancel_requested: false
        }
    );

    let was = s.resume_instance_by(&admin()).await.unwrap();
    assert_eq!(was.level, PauseLevel::Intake);
    assert!(s.instance_pause().await.unwrap().is_none());
    assert!(matches!(
        s.resume_instance_by(&admin()).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(reason(s, later.id).await, "", "cleared at once on resume");
    // Every queued job runs again, none lost.
    let mut claimed = Vec::new();
    while let Some(c) = s.claim_job("w", LEASE, ANY).await.unwrap() {
        claimed.push(c.job.id);
    }
    claimed.sort_by_key(|j| j.0);
    assert_eq!(claimed, [queued_a.id, queued_b.id, later.id]);

    let actions: Vec<String> = s
        .audit_log(None, Page::first(10))
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|e| e.action.starts_with("instance."))
        .map(|e| e.action)
        .collect();
    assert_eq!(actions, ["instance.resume", "instance.pause"]);
    db.drop().await;
}

#[tokio::test]
async fn an_emergency_pause_asks_every_running_job_to_cancel() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let b = repo(s, "b").await;
    let one = job(s, a, 1).await;
    let two = job(s, b, 1).await;
    s.claim_job("w1", LEASE, ANY).await.unwrap().unwrap();
    s.claim_job("w2", LEASE, ANY).await.unwrap().unwrap();
    let queued = job(s, a, 2).await;

    // An intake pause may be raised to an emergency one, not the reverse.
    s.pause_instance_by(PauseLevel::Intake, None, &Actor::cli())
        .await
        .unwrap();
    let paused = s
        .pause_instance_by(PauseLevel::Emergency, Some("injection"), &admin())
        .await
        .unwrap();
    assert_eq!(paused.cancelled, 2);
    assert_eq!(paused.pause.level, PauseLevel::Emergency);
    assert!(matches!(
        s.pause_instance_by(PauseLevel::Intake, None, &admin())
            .await,
        Err(StoreError::Conflict(_))
    ));
    // Each worker learns it at its next renewal, as with `cancel`.
    for (j, w) in [(one.id, "w1"), (two.id, "w2")] {
        assert_eq!(
            s.renew_lease(j, w, LEASE).await.unwrap(),
            Lease::Held {
                cancel_requested: true
            }
        );
    }
    // The queued job is kept, not cancelled: it waits.
    let kept = s.job(queued.id).await.unwrap().unwrap();
    assert_eq!(kept.status, JobStatus::Queued);
    assert!(!kept.cancel_requested);
    assert!(s.claim_job("w1", LEASE, ANY).await.unwrap().is_none());

    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(entry.action, "instance.pause");
    assert_eq!(entry.detail["level"], "emergency");
    assert_eq!(entry.detail["cancelled"], 2);
    assert_eq!(entry.repo_id, None);

    s.resume_instance_by(&admin()).await.unwrap();
    let next = s.claim_job("w1", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(next.job.id, queued.id);
    db.drop().await;
}

#[tokio::test]
async fn a_reason_too_long_is_refused_before_anything_changes() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    assert!(matches!(
        s.pause_instance_by(PauseLevel::Intake, Some(&"x".repeat(501)), &admin())
            .await,
        Err(StoreError::Invalid { .. })
    ));
    assert!(s.instance_pause().await.unwrap().is_none());
    db.drop().await;
}
