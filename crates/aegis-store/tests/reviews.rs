//! Review rounds (#243): a job that opened a PR gets a few rounds after
//! reviews, one live at a time, each with a ledger of its own.

use std::time::Duration;

use aegis_ledger::{IssueId, Outcome};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, AttemptEnd, Job, JobSource, JobStatus, NewJob, NewRepo, RepoMode, Store, StoreError,
};

const LEASE: Duration = Duration::from_secs(60);
const PR: &str = "https://github.com/acme-bot/app/pull/7";

async fn setup(s: &Store) -> Job {
    let repo = s
        .create_repo(&NewRepo {
            slug: "app".into(),
            url: "https://github.com/acme/app".into(),
            default_branch: "main".into(),
            language: "go".into(),
            mode: RepoMode::Poc,
            max_concurrent_jobs: 5,
            config: serde_json::json!({}),
        })
        .await
        .unwrap();
    s.create_job(&NewJob {
        repo_id: repo.id,
        issue_ref: "acme/app#3".into(),
        ledger_key: IssueId::parse("acme-app-3-1".to_owned()).unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap()
}

/// Claims the next job and ends it as `end`.
async fn run(s: &Store, end: AttemptEnd) -> Job {
    let claim = s.claim_job("w", LEASE, 10).await.unwrap().unwrap();
    s.finish_attempt(&claim.run, "w", &end).await.unwrap()
}

fn pr_opened() -> AttemptEnd {
    AttemptEnd::Done {
        outcome: Outcome::PrOpened,
        reason: None,
        pr_url: Some(PR.into()),
        patch: None,
    }
}

#[tokio::test]
async fn a_pr_gets_its_rounds_one_at_a_time_then_no_more() {
    let Some(db) = test_db().await else { return };
    let s = &db.store.clone().with_max_review_rounds(2);
    let actor = Actor::cli();
    let first = setup(s).await;
    let err = s.review_job_by(first.id, &actor).await.unwrap_err();
    assert!(err.to_string().contains("opened no PR"), "{err}");
    let opened = run(s, pr_opened()).await;
    assert_eq!(opened.id, first.id);

    let round1 = s.review_job_by(first.id, &actor).await.unwrap();
    assert_eq!(round1.source, JobSource::Review);
    assert_eq!((round1.review_of, round1.review_round), (Some(first.id), 1));
    assert_eq!(round1.issue_ref, "acme-bot/app#7");
    assert_eq!(round1.ledger_key.as_str(), "ACME-APP-3-1-V1");
    let err = s.review_job_by(first.id, &actor).await.unwrap_err();
    assert!(
        matches!(&err, StoreError::Conflict(m) if m.contains("live review round")),
        "{err}"
    );
    assert_eq!(s.review_root(&round1).await.unwrap().unwrap().id, first.id);

    // A round that failed is run again as the same round.
    let failed = run(
        s,
        AttemptEnd::Failed {
            error: "push refused".into(),
        },
    )
    .await;
    assert_eq!(failed.status, JobStatus::Failed);
    let again = s.retry_job(round1.id).await.unwrap();
    assert_eq!((again.review_of, again.review_round), (Some(first.id), 1));
    assert_eq!(again.ledger_key.as_str(), "ACME-APP-3-1-V1-R1");
    run(s, pr_opened()).await;

    // Asked from a round, the next round is still the PR's.
    let round2 = s.review_job_by(again.id, &actor).await.unwrap();
    assert_eq!((round2.review_of, round2.review_round), (Some(first.id), 2));
    run(s, pr_opened()).await;
    let err = s.review_job_by(first.id, &actor).await.unwrap_err();
    assert!(
        err.to_string().contains("had its 2 review round(s)"),
        "{err}"
    );

    let actions: Vec<String> = s
        .audit_log(None, aegis_store::Page::first(50))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|e| e.action)
        .filter(|a| a == "job.review")
        .collect();
    assert_eq!(actions.len(), 2);
    db.drop().await;
}
