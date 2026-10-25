//! The records the dashboard's KPIs come from (#170): the saved summary
//! records, read in one query with their job and repo, give what folding
//! every ledger's events gives, even when a record was not saved.

use aegis_ledger::{
    IssueId, IssueRecord, Kpi, LedgerEvent, LedgerStore, Outcome, Review, ReviewVerdict, Tier,
    TokenCounts,
};
use aegis_store::testing::test_db;
use aegis_store::{JobEnd, JobSource, KpiRecord, NewJob, NewRepo, RepoId, RepoMode, Store};
use serde_json::json;

fn id(text: &str) -> IssueId {
    IssueId::parse(text).unwrap()
}

fn call(cost_usd: f64) -> LedgerEvent {
    LedgerEvent::ModelCall {
        tier: Tier::Core,
        model: "core".into(),
        tokens: TokenCounts {
            input: 10,
            cache_read: 90,
            ..TokenCounts::default()
        },
        cost_usd,
    }
}

fn finished(outcome: Outcome) -> LedgerEvent {
    LedgerEvent::Finished {
        outcome,
        wall_time_s: 60.0,
    }
}

fn accepted() -> LedgerEvent {
    LedgerEvent::Reviewed(Review {
        verdict: ReviewVerdict::Accepted,
        lines_edited: 0,
        review_minutes: 3.0,
        broke_tests: false,
    })
}

async fn repo(s: &Store, slug: &str, language: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: language.into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(s: &Store, repo: RepoId, key: &str) {
    let job = s
        .create_job(&NewJob {
            repo_id: repo,
            issue_ref: format!("x/y#{key}"),
            ledger_key: id(key),
            source: JobSource::Cli,
        })
        .await
        .unwrap();
    if key.ends_with("queued") {
        return;
    }
    s.end_job(job.id, JobEnd::Done(Outcome::PrOpened))
        .await
        .unwrap();
}

/// Appends `events` to `key`'s ledger, then saves the record of the first
/// `saved` of them, as `IssueLedger` does after `finished` and `reviewed`.
async fn ledger(s: &Store, key: &str, events: &[LedgerEvent], saved: Option<usize>) {
    let ledger = s.ledger();
    let issue = id(key);
    for event in events {
        ledger.append(&issue, event).await.unwrap();
    }
    if let Some(n) = saved {
        let record = IssueRecord::from_events(issue, &events[..n]);
        ledger.save_record(&record).await.unwrap();
    }
}

/// Saving the record before appending the rest, as when the last save
/// failed: the record is older than the events it misses.
async fn ledger_with_stale_record(s: &Store, key: &str, events: &[LedgerEvent], saved: usize) {
    let ledger = s.ledger();
    let issue = id(key);
    for event in &events[..saved] {
        ledger.append(&issue, event).await.unwrap();
    }
    let record = IssueRecord::from_events(issue.clone(), &events[..saved]);
    ledger.save_record(&record).await.unwrap();
    for event in &events[saved..] {
        ledger.append(&issue, event).await.unwrap();
    }
}

/// Every ledger's record folded from its events, as `aegis report` does.
async fn folded(s: &Store) -> Vec<IssueRecord> {
    let ledger = s.ledger();
    let mut out = Vec::new();
    for issue in ledger.issues().await.unwrap() {
        let events = ledger.events(&issue).await.unwrap();
        out.push(IssueRecord::from_events(issue, &events));
    }
    out
}

fn keyed(records: &[KpiRecord]) -> Vec<(String, Option<String>)> {
    let mut out: Vec<_> = records
        .iter()
        .map(|r| {
            (
                r.record.issue_id.to_string(),
                r.job
                    .as_ref()
                    .map(|j| format!("{}/{}", j.repo_slug, j.language)),
            )
        })
        .collect();
    out.sort();
    out
}

async fn seed(s: &Store) -> (RepoId, RepoId) {
    let typst = repo(s, "typst", "Rust").await;
    let scrapy = repo(s, "scrapy", "python").await;
    // Finished and saved, as a worker leaves a run.
    job(s, typst, "typst-1").await;
    let done = [call(1.0), finished(Outcome::PrOpened)];
    ledger(s, "typst-1", &done, Some(2)).await;
    // Finished, but the record's save failed.
    job(s, typst, "typst-2").await;
    let unsaved = [call(2.0), finished(Outcome::StoppedSteps)];
    ledger(s, "typst-2", &unsaved, None).await;
    // Reviewed after the record was saved, the new record not saved.
    job(s, scrapy, "scrapy-1").await;
    let reviewed = [call(0.5), finished(Outcome::PrOpened), accepted()];
    ledger_with_stale_record(s, "scrapy-1", &reviewed, 2).await;
    // Still running: events, no record.
    job(s, scrapy, "scrapy-2").await;
    ledger(s, "scrapy-2", &[call(4.0)], None).await;
    // Queued: no event yet, so not in the ledger.
    job(s, scrapy, "scrapy-queued").await;
    // A CLI run outside any job.
    let cli = [call(3.0), finished(Outcome::StoppedBudget)];
    ledger(s, "eval-7-r1", &cli, Some(2)).await;
    (typst, scrapy)
}

#[tokio::test]
async fn kpi_records_match_the_folded_ledger() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    seed(s).await;

    let got = s.kpi_records(None).await.unwrap();
    let ledger = folded(s).await;
    assert_eq!(
        Kpi::from_records(got.iter().map(|r| &r.record)),
        Kpi::from_records(&ledger)
    );
    // Each finished record is the folded one, the stale ones included.
    for record in ledger.iter().filter(|r| r.outcome.is_some()) {
        let mine = got.iter().find(|r| r.record.issue_id == record.issue_id);
        assert_eq!(mine.map(|r| &r.record), Some(record));
    }
    assert_eq!(
        keyed(&got),
        [
            ("EVAL-7-R1".to_owned(), None),
            ("SCRAPY-1".into(), Some("scrapy/python".into())),
            ("SCRAPY-2".into(), Some("scrapy/python".into())),
            ("TYPST-1".into(), Some("typst/Rust".into())),
            ("TYPST-2".into(), Some("typst/Rust".into())),
        ]
    );
    // The running job counts for nothing, but its repo shows.
    let running = got.iter().find(|r| r.record.issue_id == id("scrapy-2"));
    assert_eq!(running.unwrap().record.outcome, None);
    db.drop().await;
}

#[tokio::test]
async fn kpi_records_within_repos_leave_the_rest_out() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (typst, _) = seed(s).await;

    let got = s.kpi_records(Some(&[typst])).await.unwrap();
    assert_eq!(
        keyed(&got),
        [
            ("TYPST-1".to_owned(), Some("typst/Rust".to_owned())),
            ("TYPST-2".into(), Some("typst/Rust".into())),
        ]
    );
    assert!(s.kpi_records(Some(&[])).await.unwrap().is_empty());
    db.drop().await;
}
