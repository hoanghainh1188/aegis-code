//! #705 (3/3): a repo's deprecation batch is queued on its schedule, only
//! inside its off-hours window and one at a time, and its issue skips the
//! warnings earlier batches took while they are being fixed or were fixed
//! in a PR.

use std::time::Duration;

use aegis_ledger::{IssueKind, LedgerEvent, LedgerStore, Outcome};
use aegis_server::deprecations::{check_due_at, deprecation_issue};
use aegis_store::testing::test_db;
use aegis_store::{
    AttemptEnd, Job, JobFilter, JobSource, JobStatus, ModeName, NewRepo, Page, Repo, Store,
};
use serde_json::{Value, json};
use time::macros::datetime;

async fn repo(s: &Store, slug: &str, poll: Value) -> Repo {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 1,
        config: json!({ "deprecation_poll": poll }),
    })
    .await
    .unwrap()
}

async fn jobs(s: &Store) -> Vec<Job> {
    let mut jobs = s
        .list_jobs(&JobFilter::default(), Page::first(100))
        .await
        .unwrap()
        .items;
    jobs.sort_by_key(|j| j.id.0);
    jobs
}

/// The repo's batch claimed, its keys recorded, and ended with `outcome`
/// (or failed when `None`).
async fn ran(s: &Store, keys: &[&str], outcome: Option<Outcome>) -> Job {
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let taken = LedgerEvent::DeprecationsTaken {
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
    };
    s.ledger()
        .append(&claim.job.ledger_key, &taken)
        .await
        .unwrap();
    let end = match outcome {
        Some(outcome) => AttemptEnd::Done {
            outcome,
            reason: None,
            pr_url: None,
            patch: None,
            diagnosis: None,
        },
        None => AttemptEnd::Failed {
            error: "the gate failed".into(),
        },
    };
    s.finish_attempt(&claim.run, "w", &end).await.unwrap()
}

#[tokio::test]
async fn a_batch_is_queued_in_the_window_one_at_a_time() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // 20:00 to 06:00 at UTC+7, three groups at most.
    let shop = repo(
        s,
        "shop",
        json!({ "utc_offset_minutes": 420, "max_groups": 3 }),
    )
    .await;
    repo(s, "bad", json!({ "every_s": 1 })).await;

    // 10:00 local: outside the window, nothing, and the schedule waits.
    assert!(
        check_due_at(s, datetime!(2026-11-28 03:00 UTC))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(jobs(s).await.is_empty());
    // 21:00 local: one batch.
    check_due_at(s, datetime!(2026-11-28 14:00 UTC))
        .await
        .unwrap();
    let queued = jobs(s).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(
        (queued[0].source, queued[0].status, queued[0].repo_id),
        (JobSource::Deprecation, JobStatus::Queued, shop.id)
    );
    assert!(queued[0].issue_ref.starts_with("deprecations-"));
    // Claimed for the period: not again.
    check_due_at(s, datetime!(2026-11-28 14:05 UTC))
        .await
        .unwrap();
    assert_eq!(jobs(s).await.len(), 1);
    // Due again while the first is still queued: one live batch at most.
    db.raw("UPDATE polls SET next_at = now() - interval '1 second'")
        .await
        .unwrap();
    check_due_at(s, datetime!(2026-11-28 15:00 UTC))
        .await
        .unwrap();
    assert_eq!(jobs(s).await.len(), 1);

    let issue = deprecation_issue(&queued[0], &shop, s).await.unwrap();
    assert_eq!(issue.kind, Some(IssueKind::Deprecation));
    assert_eq!(issue.source.as_deref(), Some("deprecation"));
    assert_eq!(issue.id, queued[0].ledger_key.as_str());
    assert!(
        issue.body.contains(r#"{"max_groups":3,"skip":[]}"#),
        "{}",
        issue.body
    );
    db.drop().await;
}

#[tokio::test]
async fn the_next_batch_skips_what_earlier_ones_took_unless_they_failed() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "shop", json!({ "from_hour": 0, "to_hour": 23 })).await;
    let night = datetime!(2026-11-28 01:00 UTC);
    let again = || async {
        db.raw("UPDATE polls SET next_at = now() - interval '1 second'")
            .await
            .unwrap();
        check_due_at(s, night).await.unwrap();
    };

    check_due_at(s, night).await.unwrap();
    ran(
        s,
        &["deprecation:a", "deprecation:b"],
        Some(Outcome::PrOpened),
    )
    .await;
    again().await;
    ran(s, &["deprecation:c"], None).await;
    again().await;
    let next = jobs(s).await.pop().unwrap();
    assert_eq!(next.status, JobStatus::Queued);
    // The PR's warnings are skipped; the failed batch's are free again.
    assert_eq!(
        s.deprecations_taken(shop.id).await.unwrap(),
        ["deprecation:a", "deprecation:b"]
    );
    let issue = deprecation_issue(&next, &shop, s).await.unwrap();
    assert!(
        issue
            .body
            .contains(r#""skip":["deprecation:a","deprecation:b"]"#),
        "{}",
        issue.body
    );
    db.drop().await;
}
