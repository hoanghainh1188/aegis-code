//! Error spikes from an APM source (#706): one job per spike, a
//! fingerprint diagnosed once per dedupe period, a cap per repo per day.

use std::time::Duration;

use aegis_ledger::{IssueId, Outcome};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, AnomalyLimits, AnomalySkipped, AttemptEnd, Delivered, JobSource, JobStatus, ModeName,
    NewAnomaly, NewJob, NewRepo, RepoId, Store, StoreError,
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

/// #706 (3/3): a spike's diagnosis is turned into one fix job at a time,
/// and the ticket its report was posted to finds it.
#[tokio::test]
async fn a_diagnosed_spike_becomes_a_fix_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let app = repo(s, "app").await;
    s.deliver_anomaly(&job(app, "apm-1"), &spike("sentry:A"), LIMITS)
        .await
        .unwrap()
        .unwrap();
    let actor = Actor::cli();
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let diagnosis = claim.job.id;
    // Not before it is diagnosed.
    assert!(matches!(
        s.fix_anomaly_by(diagnosis, &actor).await,
        Err(StoreError::Conflict(_))
    ));
    let end = AttemptEnd::Done {
        outcome: Outcome::Diagnosed,
        reason: None,
        pr_url: None,
        patch: None,
        diagnosis: Some("**Diagnosis**: code_bug (confidence 0.80).".into()),
    };
    s.finish_attempt(&claim.run, "w", &end).await.unwrap();
    assert_eq!(
        s.anomaly_report(app, "apm-1").await.unwrap().as_deref(),
        Some("**Diagnosis**: code_bug (confidence 0.80).")
    );

    s.set_anomaly_ticket(app, "apm-1", "acme/app#12")
        .await
        .unwrap();
    assert_eq!(
        s.anomaly_ticket(app, "apm-1").await.unwrap().as_deref(),
        Some("acme/app#12")
    );
    assert_eq!(
        s.anomaly_job_by_ticket(app, "acme/app#12").await.unwrap(),
        Some(diagnosis)
    );
    assert_eq!(
        s.anomaly_job_by_ticket(app, "acme/app#13").await.unwrap(),
        None
    );

    let fix = s.fix_anomaly_by(diagnosis, &actor).await.unwrap();
    assert_eq!(
        (fix.source, fix.status, fix.issue_ref.as_str()),
        (JobSource::Apm, JobStatus::Queued, "apm-1-fix")
    );
    assert_eq!(fix.ledger_key.as_str(), format!("APP-APM-1-{}-FIX", app.0));
    // One at a time, and a fix job is not a spike to fix.
    assert!(matches!(
        s.fix_anomaly_by(diagnosis, &actor).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        s.fix_anomaly_by(fix.id, &actor).await,
        Err(StoreError::Conflict(_))
    ));
    // Once that fix failed, another may be asked for, under a new key.
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.job.id, fix.id);
    let failed = AttemptEnd::Failed {
        error: "the gate failed".into(),
    };
    s.finish_attempt(&claim.run, "w", &failed).await.unwrap();
    let again = s.fix_anomaly_by(diagnosis, &actor).await.unwrap();
    assert_eq!(
        again.ledger_key.as_str(),
        format!("APP-APM-1-{}-FIX2", app.0)
    );
    db.drop().await;
}
