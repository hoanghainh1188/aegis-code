//! `PgLedger` behaves like `JsonFileLedger`, and a ledger copied between
//! them gives the same records and KPIs (#22).

use aegis_ledger::{
    GateRun, IssueId, IssueKind, IssueRecord, JsonFileLedger, Kpi, LedgerError, LedgerEvent,
    LedgerStore, Outcome, Review, ReviewVerdict, Tier, TokenCounts, ToolCall, copy_ledger,
};
use aegis_store::testing::test_db;

fn id(text: &str) -> IssueId {
    IssueId::parse(text).unwrap()
}

/// One of every event kind, with numbers JSON and jsonb could round
/// differently.
fn run_events(cost: f64) -> Vec<LedgerEvent> {
    vec![
        LedgerEvent::Classified {
            kind: IssueKind::Bug,
        },
        LedgerEvent::ModelCall {
            tier: Tier::Light,
            model: "claude-haiku-5-5".into(),
            tokens: TokenCounts {
                input: 1200,
                output: 40,
                ..TokenCounts::default()
            },
            cost_usd: 0.000_123_456_789_012_345_6,
        },
        LedgerEvent::ModelCall {
            tier: Tier::Core,
            model: "claude-sonnet-5-5".into(),
            tokens: TokenCounts {
                input: 3,
                output: 812,
                cache_write_5m: 0,
                cache_write_1h: 20_480,
                cache_read: u64::from(u32::MAX) * 3,
            },
            cost_usd: cost,
        },
        LedgerEvent::ToolCalls {
            tier: Tier::Core,
            calls: vec![
                ToolCall {
                    name: "view".into(),
                    is_error: false,
                    target: Some("src/lib.rs:10-40".into()),
                },
                ToolCall {
                    name: "run_tests".into(),
                    is_error: true,
                    target: None,
                },
            ],
        },
        LedgerEvent::TestReminded {
            tier: Tier::Core,
            calls: 6,
        },
        LedgerEvent::Escalated {
            reason: "gate failed twice — \"test\" step".into(),
            calls: 9,
        },
        LedgerEvent::GateRun(GateRun {
            passed: false,
            failed_step: Some("test".into()),
            duration_s: 61.25,
        }),
        LedgerEvent::SecretsRedacted { count: 1 },
        LedgerEvent::HiddenTests {
            passed: true,
            duration_s: 1e-7,
        },
        LedgerEvent::Finished {
            outcome: Outcome::PrOpened,
            wall_time_s: 912.000_000_1,
        },
        LedgerEvent::Reviewed(Review {
            verdict: ReviewVerdict::AcceptedWithEdits,
            lines_edited: 4,
            review_minutes: 1.5,
            broke_tests: false,
        }),
    ]
}

#[tokio::test]
async fn pg_ledger_appends_and_reads_back_every_event_kind() {
    let Some(db) = test_db().await else { return };
    let ledger = db.store.ledger();
    let issue = id("typst-8850-r4");
    assert!(ledger.events(&issue).await.unwrap().is_empty());

    let events = run_events(0.1 + 0.2);
    for event in &events {
        ledger.append(&issue, event).await.unwrap();
    }
    assert_eq!(ledger.events(&issue).await.unwrap(), events);
    assert_eq!(ledger.events(&id("TYPST-8850-R4")).await.unwrap(), events);
    db.drop().await;
}

#[tokio::test]
async fn pg_ledger_reads_more_events_than_one_page() {
    let Some(db) = test_db().await else { return };
    let ledger = db.store.ledger();
    let issue = id("long-1");
    let events: Vec<_> = (0..2_345)
        .map(|count| LedgerEvent::SecretsRedacted { count })
        .collect();
    for event in &events {
        ledger.append(&issue, event).await.unwrap();
    }
    assert_eq!(ledger.events(&issue).await.unwrap(), events);
    db.drop().await;
}

#[tokio::test]
async fn pg_ledger_keeps_an_event_whose_text_holds_nul() {
    let Some(db) = test_db().await else { return };
    let ledger = db.store.ledger();
    let event = |reason: &str| LedgerEvent::Escalated {
        reason: reason.into(),
        calls: 3,
    };
    ledger.append(&id("gh-7"), &event("a\0b")).await.unwrap();
    assert_eq!(
        ledger.events(&id("gh-7")).await.unwrap(),
        vec![event("a\u{fffd}b")]
    );
    db.drop().await;
}

#[tokio::test]
async fn pg_ledger_rejects_non_finite_numbers() {
    let Some(db) = test_db().await else { return };
    let ledger = db.store.ledger();
    let event = LedgerEvent::Finished {
        outcome: Outcome::PrOpened,
        wall_time_s: f64::INFINITY,
    };
    let err = ledger.append(&id("gh-7"), &event).await.unwrap_err();
    assert!(matches!(err, LedgerError::NonFinite { .. }), "{err}");
    assert!(ledger.issues().await.unwrap().is_empty());
    db.drop().await;
}

#[tokio::test]
async fn pg_ledger_lists_issues_in_id_order_across_pages() {
    let Some(db) = test_db().await else { return };
    let ledger = db.store.ledger();
    // Byte order, as `IssueId`'s `Ord`: `-` < digits < `_` < letters.
    let mut ids: Vec<IssueId> = (0..1_234).map(|n| id(&format!("i-{n}"))).collect();
    ids.extend(["a_1", "a-1", "a.1", "B-1", "z-0"].map(id));
    for issue in ids.iter().rev() {
        ledger
            .append(issue, &LedgerEvent::SecretsRedacted { count: 0 })
            .await
            .unwrap();
    }
    ids.sort();
    assert_eq!(ledger.issues().await.unwrap(), ids);
    db.drop().await;
}

#[tokio::test]
async fn pg_ledger_replaces_the_saved_record() {
    let Some(db) = test_db().await else { return };
    let ledger = db.store.ledger();
    let issue = id("gh-9");
    let events = run_events(1.0);
    let first = IssueRecord::from_events(issue.clone(), &events[..3]);
    let last = IssueRecord::from_events(issue.clone(), &events);
    ledger.save_record(&first).await.unwrap();
    ledger.save_record(&last).await.unwrap();
    assert_eq!(db.store.record(&issue).await.unwrap(), Some(last));
    assert_eq!(db.store.record(&id("gh-10")).await.unwrap(), None);
    db.drop().await;
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aegis-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn records<S: LedgerStore>(store: &S) -> Vec<IssueRecord> {
    let mut out = Vec::new();
    for issue in store.issues().await.unwrap() {
        let events = store.events(&issue).await.unwrap();
        out.push(IssueRecord::from_events(issue, &events));
    }
    out
}

/// The issue's "done when": JSON and DB give the same KPI numbers, and a
/// ledger exported from the DB is byte for byte the one imported, so
/// `scripts/eval/tool_usage.py` reads it unchanged.
#[tokio::test]
async fn json_to_pg_and_back_keeps_every_byte_and_kpi() {
    let Some(db) = test_db().await else { return };
    let source = JsonFileLedger::new(temp_dir("source"));
    for (n, cost) in [(1, 0.53), (2, 1.0 / 3.0), (3, 4.000_000_000_000_001)] {
        let issue = id(&format!("scrapy-7604-r{n}"));
        let events = run_events(cost);
        for event in &events {
            source.append(&issue, event).await.unwrap();
        }
        source
            .save_record(&IssueRecord::from_events(issue, &events))
            .await
            .unwrap();
    }
    // A run still going: events but no record yet.
    source
        .append(&id("typst-1-r1"), &run_events(0.2)[0])
        .await
        .unwrap();

    let ledger = db.store.ledger();
    let copied = copy_ledger(&source, &ledger).await.unwrap();
    assert_eq!(copied.copied.len(), 4);
    let again = copy_ledger(&source, &ledger).await.unwrap();
    assert!(again.copied.is_empty(), "a second import changes nothing");
    assert_eq!(again.unchanged.len(), 4);

    let json = records(&source).await;
    let pg = records(&ledger).await;
    assert_eq!(pg, json);
    assert_eq!(Kpi::from_records(&pg), Kpi::from_records(&json));

    let back = JsonFileLedger::new(temp_dir("back"));
    copy_ledger(&ledger, &back).await.unwrap();
    for issue in source.issues().await.unwrap() {
        let before = std::fs::read(source.events_path(&issue)).unwrap();
        let after = std::fs::read(back.events_path(&issue)).unwrap();
        assert_eq!(after, before, "{issue}.events.jsonl");
    }
    db.drop().await;
}
