//! Error spikes from an APM source (#706): one job per spike, a
//! fingerprint diagnosed once per dedupe period, a cap per repo per day.

use std::time::Duration;

use aegis_ledger::IssueId;
use aegis_store::testing::test_db;
use aegis_store::{
    AnomalyLimits, AnomalySkipped, Delivered, JobSource, JobStatus, ModeName, NewAnomaly, NewJob,
    NewRepo, RepoId, Store, StoreError,
};
use serde_json::json;

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

fn job(repo: RepoId, name: &str) -> NewJob {
    NewJob {
        repo_id: repo,
        issue_ref: name.into(),
        ledger_key: IssueId::parse(format!("app-{name}-{}", repo.0)).unwrap(),
        source: JobSource::Apm,
        priority: None,
        sla_deadline: None,
    }
}

fn spike(fingerprint: &str) -> NewAnomaly {
    NewAnomaly {
        fingerprint: fingerprint.into(),
        title: format!("Error spike: {fingerprint}"),
        body: format!("An error spike found in sentry: `{fingerprint}`"),
    }
}

const LIMITS: AnomalyLimits = AnomalyLimits {
    dedupe: Duration::from_secs(7 * 24 * 3600),
    per_day: 2,
};

#[tokio::test]
async fn a_spike_is_diagnosed_once_and_a_repo_gets_its_cap_a_day() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let app = repo(s, "app").await;
    let other = repo(s, "other").await;

    let first = s
        .deliver_anomaly(&job(app, "apm-1"), &spike("sentry:A"), LIMITS)
        .await
        .unwrap();
    let Ok(Delivered::Queued(queued)) = first else {
        panic!("{first:?}")
    };
    assert_eq!(
        (queued.source, queued.status),
        (JobSource::Apm, JobStatus::Queued)
    );
    assert_eq!(
        s.anomaly(app, "apm-1").await.unwrap(),
        Some((
            "Error spike: sentry:A".into(),
            "An error spike found in sentry: `sentry:A`".into()
        ))
    );

    // The same fingerprint again: seen, nothing written.
    assert_eq!(
        s.deliver_anomaly(&job(app, "apm-2"), &spike("sentry:A"), LIMITS)
            .await
            .unwrap()
            .unwrap_err(),
        AnomalySkipped::Seen
    );
    assert_eq!(s.anomaly(app, "apm-2").await.unwrap(), None);
    // A second spike fits the cap of 2; a third does not.
    assert!(
        s.deliver_anomaly(&job(app, "apm-3"), &spike("sentry:B"), LIMITS)
            .await
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        s.deliver_anomaly(&job(app, "apm-4"), &spike("sentry:C"), LIMITS)
            .await
            .unwrap()
            .unwrap_err(),
        AnomalySkipped::Capped
    );
    // Another repo has fingerprints and a cap of its own.
    assert!(
        s.deliver_anomaly(&job(other, "apm-1"), &spike("sentry:A"), LIMITS)
            .await
            .unwrap()
            .is_ok()
    );
    // With no dedupe period, a fingerprint can be diagnosed again.
    let again = AnomalyLimits {
        dedupe: Duration::ZERO,
        per_day: 10,
    };
    assert!(
        s.deliver_anomaly(&job(other, "apm-2"), &spike("sentry:A"), again)
            .await
            .unwrap()
            .is_ok()
    );

    let err = s
        .deliver_anomaly(&job(app, "apm-5"), &spike(""), LIMITS)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::Invalid {
                field: "fingerprint",
                ..
            }
        ),
        "{err}"
    );
    db.drop().await;
}
