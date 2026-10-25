//! Polled Sonar warnings (#27): a warning is taken by one batch.

use aegis_ledger::IssueId;
use aegis_store::testing::test_db;
use aegis_store::{
    Delivered, JobSource, JobStatus, NewJob, NewRepo, RepoId, RepoMode, Store, StoreError,
};
use serde_json::json;

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

fn batch(repo: RepoId, name: &str) -> NewJob {
    NewJob {
        repo_id: repo,
        issue_ref: name.into(),
        ledger_key: IssueId::parse(format!("app-{name}-{}", repo.0)).unwrap(),
        source: JobSource::Sonar,
    }
}

fn keys(k: &[&str]) -> Vec<String> {
    k.iter().map(|k| (*k).to_owned()).collect()
}

#[tokio::test]
async fn a_warning_is_taken_by_one_batch() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let app = repo(s, "app").await;
    let other = repo(s, "other").await;

    let (first, taken) = s
        .deliver_sonar_batch(&batch(app, "sonar-1"), &keys(&["B", "A"]))
        .await
        .unwrap()
        .unwrap();
    let Delivered::Queued(job) = first else {
        panic!("{first:?}")
    };
    assert_eq!(
        (job.source, job.status),
        (JobSource::Sonar, JobStatus::Queued)
    );
    assert_eq!(taken, keys(&["A", "B"]));

    // The same warnings again: nothing.
    assert!(
        s.deliver_sonar_batch(&batch(app, "sonar-2"), &keys(&["A", "B"]))
            .await
            .unwrap()
            .is_none()
    );
    // One new among them: a batch of that one.
    let (second, taken) = s
        .deliver_sonar_batch(&batch(app, "sonar-3"), &keys(&["A", "C"]))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(second, Delivered::Queued(_)));
    assert_eq!(taken, keys(&["C"]));
    // Another repo's project has keys of its own.
    assert!(
        s.deliver_sonar_batch(&batch(other, "sonar-1"), &keys(&["A"]))
            .await
            .unwrap()
            .is_some()
    );

    assert_eq!(
        s.sonar_batch_warnings(app, "sonar-1").await.unwrap(),
        keys(&["A", "B"])
    );
    assert_eq!(
        s.sonar_batch_warnings(app, "sonar-3").await.unwrap(),
        keys(&["C"])
    );
    assert!(
        s.sonar_batch_warnings(app, "sonar-2")
            .await
            .unwrap()
            .is_empty()
    );

    let err = s
        .deliver_sonar_batch(&batch(app, "sonar-4"), &keys(&[""]))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid { .. }), "{err}");
    db.drop().await;
}
