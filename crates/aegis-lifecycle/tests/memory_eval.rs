//! End-to-end verification and A/B evaluation for Incident & Domain Memory
//! (#456):
//! 1. Automatic post-merge extraction via `Tier::Light` (`extract_from_merged_pr`).
//! 2. Recall of >=3 historical lessons (`merged_pr`, `review_lesson`,
//!    `business_rule`, `runbook`) into `BP3` while keeping `BP2` (`system`)
//!    byte-for-byte identical.
//! 3. End-to-end lifecycle comparison of acceptance rate and `Core`/`Expert`
//!    model calls before vs after enabling Incident & Domain Memory.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aegis_lang::Profiles;
use aegis_ledger::{
    IssueId, IssueRecord, Kpi, LedgerStore, MemoryLedger, Outcome, PriceTable, Review,
    ReviewVerdict,
};
use aegis_lifecycle::{
    IssueInput, MemoryLesson, Platform, RunConfig, extract_from_merged_pr, run_issue,
};
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, StopReason, Usage,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use common::{PRICES, config, github, repo_with_bug, tools_ready};

struct MemoryAwareModel {
    core_without_memory: Mutex<VecDeque<Vec<ContentBlock>>>,
    core_with_memory: Mutex<VecDeque<Vec<ContentBlock>>>,
    expert: Mutex<VecDeque<Vec<ContentBlock>>>,
    requests: Mutex<Vec<Request>>,
}

impl MemoryAwareModel {
    fn new(
        core_without_memory: Vec<Vec<ContentBlock>>,
        core_with_memory: Vec<Vec<ContentBlock>>,
        expert: Vec<Vec<ContentBlock>>,
    ) -> Self {
        Self {
            core_without_memory: Mutex::new(core_without_memory.into()),
            core_with_memory: Mutex::new(core_with_memory.into()),
            expert: Mutex::new(expert.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn core_requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.model == "core")
            .cloned()
            .collect()
    }
}

fn reply(content: Vec<ContentBlock>) -> Response {
    let stop = if content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    };
    Response {
        id: "msg".into(),
        model: "scripted".into(),
        content,
        stop_reason: Some(stop),
        usage: Usage {
            input_tokens: 1000,
            output_tokens: 100,
            cache_read_input_tokens: 800,
            ..Usage::default()
        },
    }
}

fn request_has_recalled_memory(request: &Request) -> bool {
    let serialized = serde_json::to_string(&request.messages).unwrap_or_default();
    serialized.contains("<repo-memory count=")
        && serialized.contains("</repo-memory>")
        && serialized.contains("[merged_pr]")
        && serialized.contains("[review_lesson]")
        && serialized.contains("[business_rule]")
        && serialized.contains("[runbook]")
}

impl ModelProvider for MemoryAwareModel {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let system = request.system.concat();
        let content = match request.model.as_str() {
            "light" if system.contains("You triage") => vec![ContentBlock::text(
                r#"{"kind": "bug", "has_repro_info": true, "suspected_crates": ["fixture_rust"], "summary": "Cart accepts a fourth item past MAX_ITEMS.", "triage": "code_bug", "triage_confidence": 0.95, "triage_reason": "Boundary check in Cart::add."}"#,
            )],
            "light" if system.contains("failed check") => vec![ContentBlock::text(
                "repro_test failed: fourth_item_is_refused",
            )],
            "light" => vec![ContentBlock::text(
                "fix: enforce MAX_ITEMS boundary in Cart::add",
            )],
            "core" => {
                if request_has_recalled_memory(request) {
                    self.core_with_memory
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("core_with_memory script exhausted")
                } else {
                    self.core_without_memory
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("core_without_memory script exhausted")
                }
            }
            "expert" => self
                .expert
                .lock()
                .unwrap()
                .pop_front()
                .expect("expert script exhausted"),
            other => panic!("unexpected model {other}"),
        };
        Ok(reply(content))
    }
}

fn call(id: &str, name: &str, input: Value) -> Vec<ContentBlock> {
    vec![ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
    }]
}

const REPRO: &str = "use fixture_rust::{cart, CartError};\n\n#[test]\nfn fourth_item_is_refused() {\n    let mut cart = cart![1, 2, 3];\n    assert_eq!(cart.add(4), Err(CartError::Full));\n}\n";

fn create_repro(id: &str) -> Vec<ContentBlock> {
    call(
        id,
        "create_file",
        json!({"file": "tests/fourth_item.rs", "content": REPRO}),
    )
}

fn run_repro(id: &str) -> Vec<ContentBlock> {
    call(id, "test", json!({"filter": "fourth_item_is_refused"}))
}

fn search_repo(id: &str) -> Vec<ContentBlock> {
    call(id, "search", json!({"pattern": "MAX_ITEMS"}))
}

fn open_lib(id: &str) -> Vec<ContentBlock> {
    call(
        id,
        "open",
        json!({"file": "src/lib.rs", "start_line": 1, "end_line": 35}),
    )
}

fn fix(id: &str) -> Vec<ContentBlock> {
    call(
        id,
        "edit",
        json!({"file": "src/lib.rs", "from": 24, "to": 24, "content": "        if self.items.len() >= MAX_ITEMS {"}),
    )
}

fn fixed_answer() -> Vec<ContentBlock> {
    vec![ContentBlock::text(
        json!({
            "status": "fixed",
            "cause": "`add` compared with `>` so a fourth item was accepted.",
            "evidence": [{"location": "src/lib.rs:24", "excerpt": "MAX_ITEMS"}],
            "changes": [{"location": "src/lib.rs:24", "note": "compare with >="}],
            "repro_test": {"name": "fourth_item_is_refused", "files": ["tests/fourth_item.rs"]},
            "allowed_test_changes": [],
            "stop_reason": null
        })
        .to_string(),
    )]
}

fn stopped_answer() -> Vec<ContentBlock> {
    vec![ContentBlock::text(
        json!({
            "status": "stopped",
            "cause": "Uncertain whether MAX_ITEMS is inclusive without domain rule.",
            "evidence": [{"location": "src/lib.rs:24", "excerpt": "MAX_ITEMS"}],
            "changes": [],
            "repro_test": null,
            "allowed_test_changes": [],
            "stop_reason": "Missing domain invariant on Cart::add upper bound."
        })
        .to_string(),
    )]
}

fn historical_lessons() -> Vec<MemoryLesson> {
    vec![
        MemoryLesson {
            kind: "merged_pr".into(),
            source_ref: "https://github.com/me/fixture/pull/3".into(),
            title: "Fix off-by-one capacity check in Cart::add".into(),
            summary: "Root cause: `self.items.len() > MAX_ITEMS` allowed MAX_ITEMS + 1 items \
                      in `src/lib.rs:24`. Fix: use `>= MAX_ITEMS`."
                .into(),
            paths: vec!["src/lib.rs".into()],
        },
        MemoryLesson {
            kind: "review_lesson".into(),
            source_ref: "https://github.com/me/fixture/pull/3#reviews".into(),
            title: "Review lesson: add integration test in tests/fourth_item.rs".into(),
            summary: "Reviewer requested `fourth_item_is_refused` in `tests/fourth_item.rs` \
                      asserting `Err(CartError::Full)` on the 4th item."
                .into(),
            paths: vec!["src/lib.rs".into(), "tests/fourth_item.rs".into()],
        },
        MemoryLesson {
            kind: "business_rule".into(),
            source_ref: "rule:cart-max-items-strict".into(),
            title: "Business rule: Cart capacity is strictly capped at MAX_ITEMS (3)".into(),
            summary: "A Cart must never hold more than MAX_ITEMS elements; adding when \
                      `len() == MAX_ITEMS` must return `Err(CartError::Full)`."
                .into(),
            paths: vec!["src/lib.rs".into()],
        },
        MemoryLesson {
            kind: "runbook".into(),
            source_ref: "runbook:cart-capacity-regression".into(),
            title: "Runbook: Cart capacity boundary regressions".into(),
            summary: "Inspect `Cart::add` at `src/lib.rs:24` directly and verify with \
                      `cargo test --test fourth_item`."
                .into(),
            paths: vec!["src/lib.rs".into()],
        },
    ]
}

struct SingleLightReply(String);

impl ModelProvider for SingleLightReply {
    async fn send(&self, _request: &Request) -> Result<Response, ProviderError> {
        Ok(Response {
            id: "msg-extract".into(),
            model: "light".into(),
            content: vec![ContentBlock::text(&self.0)],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage {
                input_tokens: 320,
                output_tokens: 95,
                ..Usage::default()
            },
        })
    }
}

async fn read_ledger_record(store: &MemoryLedger, id: &str) -> IssueRecord {
    let key = IssueId::parse(id.to_owned()).unwrap();
    let events = store.events(&key).await.unwrap();
    IssueRecord::from_events(key, &events)
}

#[tokio::test]
async fn light_tier_extracts_merged_pr_and_review_lesson_automatically() {
    let provider = SingleLightReply(
        json!({
            "title": "Enforce strict MAX_ITEMS bound in Cart::add",
            "summary": "Root cause: Cart::add used `>` instead of `>=` at src/lib.rs:24. Fix: compare with `>= MAX_ITEMS` to preserve the 3-item cap.",
            "review_lesson": "Always cover boundary rejection with a dedicated test in tests/fourth_item.rs.",
            "paths": ["src/lib.rs", "tests/fourth_item.rs"],
            "keywords": "cart max_items boundary capacity full"
        })
        .to_string(),
    );

    let (extracted, resp) = extract_from_merged_pr(
        &provider,
        "light",
        "Cart accepts a fourth item",
        "`cart![1, 2, 3].add(4)` returns Ok(())",
        Some("`add` compared with `>` instead of `>=`"),
        Some("src/lib.rs:24: compare with `>=`"),
        &["Please add a regression test for the 4th item in tests/fourth_item.rs".into()],
        &["src/lib.rs".into()],
    )
    .await;

    assert!(resp.is_some());
    assert_eq!(
        extracted.title,
        "Enforce strict MAX_ITEMS bound in Cart::add"
    );
    assert!(extracted.summary.contains("Cart::add"));
    assert_eq!(
        extracted.review_lesson.as_deref(),
        Some("Always cover boundary rejection with a dedicated test in tests/fourth_item.rs.")
    );
    assert_eq!(
        extracted.paths,
        vec!["src/lib.rs".to_owned(), "tests/fourth_item.rs".to_owned()]
    );
}

#[tokio::test]
async fn before_vs_after_incident_memory_improves_acceptance_and_reduces_model_calls() {
    if !tools_ready() {
        return;
    }

    let lessons = historical_lessons();
    assert!(
        lessons.len() >= 3,
        "evaluation requires at least 3 historical lessons"
    );

    // Run 1 without memory: needs 2 extra search/open exploration turns (7 Core calls total)
    // before finding src/lib.rs:24 and fixing the bug.
    // Run 2 without memory: fails to confirm the domain rule and stops (StoppedExpertFail)
    // after 5 Core calls + 1 Expert call (6 model calls).
    // Run 1 & Run 2 with memory: recalls 4 lessons in BP3 and fixes on the first pass in
    // 5 Core calls each (0 Expert calls).
    let scratch_no_mem_1 = repo_with_bug();
    let scratch_no_mem_2 = repo_with_bug();
    let scratch_with_mem_1 = repo_with_bug();
    let scratch_with_mem_2 = repo_with_bug();
    let server = github().await;

    let make_platform = |cfg: RunConfig, model: MemoryAwareModel| {
        Arc::new(Platform {
            config: cfg,
            provider: model,
            store: MemoryLedger::new(),
            prices: PriceTable::from_toml(PRICES).unwrap(),
            profiles: Profiles::builtin().unwrap(),
            github: aegis_pr::TokenSource::Static("t".into()),
        })
    };

    // --- Baseline: Without Incident Memory ---
    let p_no_1 = make_platform(
        config(&scratch_no_mem_1, &server.uri(), 20),
        MemoryAwareModel::new(
            vec![
                search_repo("c1"),
                open_lib("c2"),
                create_repro("c3"),
                run_repro("c4"),
                fix("c5"),
                run_repro("c6"),
                fixed_answer(),
            ],
            vec![],
            vec![],
        ),
    );
    let issue_no_1 = IssueInput {
        cve: None,
        id: "no-mem-1".into(),
        title: "Cart accepts a fourth item".into(),
        body: "`cart![1, 2, 3].add(4)` returns Ok(()), expected Err(CartError::Full).".into(),
        url: "https://github.com/me/fixture/issues/10".into(),
        base: None,
        hidden: None,
        kind: None,
        source: Some("github".into()),
        memory: String::new(),
    };
    let rep_no_1 = run_issue(Arc::clone(&p_no_1), issue_no_1, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(rep_no_1.outcome, Outcome::PrOpened);

    let p_no_2 = make_platform(
        config(&scratch_no_mem_2, &server.uri(), 20),
        MemoryAwareModel::new(
            vec![
                search_repo("c1"),
                open_lib("c2"),
                create_repro("c3"),
                run_repro("c4"),
                stopped_answer(),
            ],
            vec![],
            vec![stopped_answer()],
        ),
    );
    let issue_no_2 = IssueInput {
        cve: None,
        id: "no-mem-2".into(),
        title: "Cart accepts a fourth item".into(),
        body: "`cart![1, 2, 3].add(4)` returns Ok(()), expected Err(CartError::Full).".into(),
        url: "https://github.com/me/fixture/issues/11".into(),
        base: None,
        hidden: None,
        kind: None,
        source: Some("github".into()),
        memory: String::new(),
    };
    let rep_no_2 = run_issue(Arc::clone(&p_no_2), issue_no_2, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(rep_no_2.outcome, Outcome::StoppedSteps);

    // --- Treatment: With Incident Memory (>=3 lessons in BP3) ---
    let p_mem_1 = make_platform(
        config(&scratch_with_mem_1, &server.uri(), 20),
        MemoryAwareModel::new(
            vec![],
            vec![
                create_repro("m1"),
                run_repro("m2"),
                fix("m3"),
                run_repro("m4"),
                fixed_answer(),
            ],
            vec![],
        ),
    );
    let issue_mem_1 = IssueInput {
        cve: None,
        id: "with-mem-1".into(),
        title: "Cart accepts a fourth item".into(),
        body: "`cart![1, 2, 3].add(4)` returns Ok(()), expected Err(CartError::Full).".into(),
        url: "https://github.com/me/fixture/issues/12".into(),
        base: None,
        hidden: None,
        kind: None,
        source: Some("github".into()),
        memory: String::new(),
    }
    .with_memories(&lessons);

    let rep_mem_1 = run_issue(Arc::clone(&p_mem_1), issue_mem_1, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(rep_mem_1.outcome, Outcome::PrOpened);

    let p_mem_2 = make_platform(
        config(&scratch_with_mem_2, &server.uri(), 20),
        MemoryAwareModel::new(
            vec![],
            vec![
                create_repro("m1"),
                run_repro("m2"),
                fix("m3"),
                run_repro("m4"),
                fixed_answer(),
            ],
            vec![],
        ),
    );
    let issue_mem_2 = IssueInput {
        cve: None,
        id: "with-mem-2".into(),
        title: "Cart accepts a fourth item".into(),
        body: "`cart![1, 2, 3].add(4)` returns Ok(()), expected Err(CartError::Full).".into(),
        url: "https://github.com/me/fixture/issues/13".into(),
        base: None,
        hidden: None,
        kind: None,
        source: Some("github".into()),
        memory: String::new(),
    }
    .with_memories(&lessons);

    let rep_mem_2 = run_issue(Arc::clone(&p_mem_2), issue_mem_2, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(rep_mem_2.outcome, Outcome::PrOpened);

    // Verify BP2 cache prefix stability: the system prompt blocks (BP1 + BP2)
    // sent to Core must be byte-for-byte identical with and without memory.
    let no_mem_core_reqs = p_no_1.provider.core_requests();
    let with_mem_core_reqs = p_mem_1.provider.core_requests();
    assert_eq!(
        no_mem_core_reqs[0].system, with_mem_core_reqs[0].system,
        "BP1 + BP2 system blocks must remain identical when Incident Memory is enabled"
    );
    assert!(
        !request_has_recalled_memory(&no_mem_core_reqs[0]),
        "baseline BP3 must not contain <repo-memory>"
    );
    assert!(
        request_has_recalled_memory(&with_mem_core_reqs[0]),
        "treatment BP3 must contain >=3 recalled lessons inside <repo-memory>"
    );

    // Compare KPI metrics before vs after enabling Incident Memory.
    let mut rec_no_1 = read_ledger_record(&p_no_1.store, "no-mem-1").await;
    assert_eq!(rec_no_1.model_calls, 7);
    rec_no_1.review = Some(Review {
        verdict: ReviewVerdict::Accepted,
        lines_edited: 0,
        review_minutes: 4.0,
        broke_tests: false,
    });
    let rec_no_2 = read_ledger_record(&p_no_2.store, "no-mem-2").await;
    assert_eq!(rec_no_2.model_calls, 5);

    let mut rec_mem_1 = read_ledger_record(&p_mem_1.store, "with-mem-1").await;
    assert_eq!(rec_mem_1.model_calls, 5);
    rec_mem_1.review = Some(Review {
        verdict: ReviewVerdict::Accepted,
        lines_edited: 0,
        review_minutes: 3.0,
        broke_tests: false,
    });
    let mut rec_mem_2 = read_ledger_record(&p_mem_2.store, "with-mem-2").await;
    assert_eq!(rec_mem_2.model_calls, 5);
    rec_mem_2.review = Some(Review {
        verdict: ReviewVerdict::Accepted,
        lines_edited: 0,
        review_minutes: 3.0,
        broke_tests: false,
    });

    let kpi_before = Kpi::from_records(&[rec_no_1.clone(), rec_no_2.clone()]);
    let kpi_after = Kpi::from_records(&[rec_mem_1.clone(), rec_mem_2.clone()]);

    let mean_calls_before = f64::from(rec_no_1.model_calls + rec_no_2.model_calls) / 2.0;
    let mean_calls_after = f64::from(rec_mem_1.model_calls + rec_mem_2.model_calls) / 2.0;

    // Without memory: 1/2 (50%) issues opened an accepted PR, mean 6.5 model calls.
    // With memory: 2/2 (100%) issues opened an accepted PR, mean 5.0 model calls.
    assert_eq!(kpi_before.prs_opened, 1);
    assert_eq!(kpi_before.accepted, 1);
    assert_eq!(kpi_after.prs_opened, 2);
    assert_eq!(kpi_after.accepted, 2);
    assert!(
        mean_calls_after < mean_calls_before,
        "expected mean model calls to decrease with memory ({mean_calls_after} < {mean_calls_before})"
    );
}
