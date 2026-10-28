//! "Xong khi" of #16: one sample issue runs from start to a PR, or stops
//! with a reason. The model is scripted; everything else is real: the ACI
//! server over MCP on a worktree of a git repo, the Rust gate (cargo,
//! rustfmt, clippy), the push to a bare "fork", the ledger. GitHub is
//! faked with wiremock.

mod common;

use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use aegis_lang::Profiles;
use aegis_ledger::{
    IssueId, IssueRecord, JsonFileLedger, Kpi, LedgerEvent, LedgerStore, MemoryLedger, Outcome,
    PriceTable, Tier, ToolCall, copy_ledger,
};
use aegis_lifecycle::{
    IssueInput, IssueReport, ModeConfig, Modes, Platform, Replay, ReplayProvider, RepoSettings,
    RepoSpec, ReviewRound, RunConfig, run_issue, run_review,
};
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, Role, StopReason, Usage,
};
use aegis_store::RepoMode;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use common::{
    BUG_FROM, BUG_TO, PRICES, Scratch, aci_bin, config, fork_refs, git, github, repo_with_bug,
    tools_ready,
};

/// Core and Expert follow scripts; Light answers by the job its system
/// prompt names.
struct Scripted {
    core: Mutex<VecDeque<Vec<ContentBlock>>>,
    expert: Mutex<VecDeque<Vec<ContentBlock>>>,
    requests: Mutex<Vec<Request>>,
}

impl Scripted {
    fn new(core: Vec<Vec<ContentBlock>>, expert: Vec<Vec<ContentBlock>>) -> Self {
        Self {
            core: Mutex::new(core.into()),
            expert: Mutex::new(expert.into()),
            requests: Mutex::new(vec![]),
        }
    }

    fn requests_for(&self, model: &str) -> Vec<Request> {
        let all = self.requests.lock().unwrap();
        all.iter().filter(|r| r.model == model).cloned().collect()
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
            ..Usage::default()
        },
    }
}

impl ModelProvider for Scripted {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let system = request.system.concat();
        // A customer-jp repo's models are `jp.` profiles of the same tiers.
        let content = match request.model.trim_start_matches("jp.") {
            "light" if system.contains("You triage") => vec![ContentBlock::text(
                r#"{"kind": "bug", "has_repro_info": true, "suspected_crates": [], "summary": "Cart accepts a fourth item."}"#,
            )],
            "light" if system.contains("failed check") => {
                vec![ContentBlock::text(
                    "repro_test failed: fourth_item_is_refused",
                )]
            }
            "light" => vec![ContentBlock::text("fix: refuse a fourth item in the cart")],
            "core" => self
                .core
                .lock()
                .unwrap()
                .pop_front()
                .expect("core script exhausted"),
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
            "changes": [{"location": "src/lib.rs:24", "note": "compare with >="}],
            "repro_test": {"name": "fourth_item_is_refused", "files": ["tests/fourth_item.rs"]},
            "allowed_test_changes": [],
            "stop_reason": null
        })
        .to_string(),
    )]
}

fn issue() -> IssueInput {
    IssueInput {
        id: "fixture-7".into(),
        title: "Cart accepts a fourth item".into(),
        body: "`cart![1, 2, 3].add(4)` returns Ok(()), expected Err(CartError::Full).".into(),
        url: "https://github.com/me/fixture/issues/7".into(),
        base: None,
        hidden: None,
        kind: None,
    }
}

fn platform(config: RunConfig, model: Scripted) -> Arc<Platform<Scripted, MemoryLedger>> {
    platform_priced(config, model, PRICES)
}

fn platform_priced(
    config: RunConfig,
    model: Scripted,
    prices: &str,
) -> Arc<Platform<Scripted, MemoryLedger>> {
    Arc::new(Platform {
        config,
        provider: model,
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(prices).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github_token: "t".into(),
    })
}

async fn run(p: &Arc<Platform<Scripted, MemoryLedger>>, cancel: CancellationToken) -> IssueReport {
    run_issue(Arc::clone(p), issue(), cancel).await.unwrap()
}

#[test]
fn the_lifecycle_future_can_be_spawned() {
    fn spawnable<F: Future + Send + 'static>(_: &F) {}
    let scratch = repo_with_bug();
    let p = platform(
        config(&scratch, "http://127.0.0.1:1", 20),
        Scripted::new(vec![], vec![]),
    );
    let future = run_issue(p, issue(), CancellationToken::new());
    spawnable(&future);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bug_goes_from_issue_to_pr() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    let model = Scripted::new(
        vec![
            call("t1", "search_symbol", json!({"name": "Cart::add"})),
            create_repro("t2"),
            fix("t3"),
            fixed_answer(),
        ],
        vec![],
    );
    let p = platform(config(&scratch, &server.uri(), 20), model);

    let report = tokio::spawn({
        let p = Arc::clone(&p);
        async move { run(&p, CancellationToken::new()).await }
    })
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(
        report.pr_url.as_deref(),
        Some("https://github.com/me/fixture/pull/3")
    );
    assert_eq!(report.model_calls, 4);
    assert_eq!(fork_refs(&scratch), "refs/heads/agent/fixture-7");

    let record = p
        .store
        .record(&IssueId::parse("fixture-7").unwrap())
        .unwrap();
    assert_eq!(record.outcome, Some(Outcome::PrOpened));
    assert_eq!(record.gate_runs.len(), 1);
    assert!(record.gate_runs[0].passed);
    assert!(record.cost_usd > 0.0);
    // The job's worktree is gone; the clone is untouched.
    assert!(!scratch.0.join("state/jobs/fixture-7/repo").exists());
    let clone_lib = std::fs::read_to_string(scratch.0.join("clone/src/lib.rs")).unwrap();
    assert!(clone_lib.contains(BUG_TO));

    let pr: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(pr["head"], "agent/fixture-7");
    assert_eq!(pr["title"], "fix: refuse a fourth item in the cart");
    let body = pr["body"].as_str().unwrap();
    assert!(
        body.contains("`fourth_item_is_refused` (FAIL → PASS)"),
        "{body}"
    );
    assert!(body.contains("repro_test ✔ · test ✔ · coverage"), "{body}");
}

fn rev_parse(repo: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// "Xong khi" of #243: after a review with a line note, a round goes on
/// from the PR's branch (a reviewer's commit included), passes the gate
/// against the PR's base, pushes one more commit on top without force and
/// answers the note on the PR.
#[tokio::test(flavor = "multi_thread")]
async fn a_review_round_pushes_onto_the_pr_branch_and_answers_each_note() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path(
            "/repos/me/fixture/issues/3/comments",
        ))
        .respond_with(wiremock::ResponseTemplate::new(201).set_body_json(json!({
            "id": 1, "html_url": "https://github.com/me/fixture/pull/3#c1"
        })))
        .mount(&server)
        .await;
    let first = platform(
        config(&scratch, &server.uri(), 20),
        Scripted::new(vec![create_repro("t1"), fix("t2"), fixed_answer()], vec![]),
    );
    let opened = run(&first, CancellationToken::new()).await;
    assert_eq!(opened.outcome, Outcome::PrOpened, "{opened:?}");

    // A reviewer pushes a commit of their own to the PR's branch.
    let fork = scratch.0.join("fork.git");
    let theirs = scratch.0.join("reviewer");
    git(
        &scratch.0,
        &[
            "clone",
            "-q",
            "-b",
            "agent/fixture-7",
            fork.to_str().unwrap(),
            "reviewer",
        ],
    );
    std::fs::write(theirs.join("NOTES.md"), "Reviewed.\n").unwrap();
    git(&theirs, &["add", "-A"]);
    git(
        &theirs,
        &[
            "-c",
            "user.name=r",
            "-c",
            "user.email=r@example.com",
            "commit",
            "-qm",
            "review notes",
        ],
    );
    git(&theirs, &["push", "-q", "origin", "agent/fixture-7"]);
    let reviewer_commit = rev_parse(&theirs, "HEAD");

    let round = ReviewRound {
        pull: aegis_pr::PullState {
            number: 3,
            url: "https://github.com/me/fixture/pull/3".into(),
            head_ref: "agent/fixture-7".into(),
            // Stale on purpose: the round goes by the branch it fetches.
            head_sha: "0000000".into(),
            base_ref: "main".into(),
        },
        notes: vec![aegis_pr::ReviewNote {
            author: "lan".into(),
            at: aegis_pr::NoteAt::Line {
                path: "src/lib.rs".into(),
                line: Some(24),
            },
            body: "Say why the limit is inclusive.".into(),
            created_at: "2026-10-28T01:00:00Z".into(),
        }],
    };
    let comment = call(
        "r1",
        "edit",
        json!({"file": "src/lib.rs", "from": 24, "to": 24,
               "content": "        // MAX_ITEMS items at most: the next one is refused.\n        if self.items.len() >= MAX_ITEMS {"}),
    );
    let answer = vec![ContentBlock::text(
        json!({
            "status": "fixed",
            "cause": "The limit needed a comment.",
            "changes": [{"location": "src/lib.rs:24", "note": "say why the limit is inclusive"}],
            "repro_test": {"name": "fourth_item_is_refused", "files": ["tests/fourth_item.rs"]},
            "review_replies": [{"note": 1, "done": true, "reply": "added the comment"}]
        })
        .to_string(),
    )];
    let p = platform(
        config(&scratch, &server.uri(), 20),
        Scripted::new(vec![comment, answer], vec![]),
    );
    let report = run_review(
        Arc::clone(&p),
        IssueInput {
            id: "fixture-7-v1".into(),
            ..issue()
        },
        round,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(
        report.pr_url.as_deref(),
        Some("https://github.com/me/fixture/pull/3")
    );
    assert_eq!(report.reason, None);

    // One more commit, on top of the reviewer's, which stays.
    let tip = rev_parse(&fork, "agent/fixture-7");
    assert_eq!(rev_parse(&fork, &format!("{tip}^")), reviewer_commit);
    let record = p
        .store
        .record(&IssueId::parse("fixture-7-v1").unwrap())
        .unwrap();
    assert_eq!(record.gate_runs.len(), 1);
    assert!(
        record.gate_runs[0].passed,
        "the repro test still fails on the PR's base"
    );

    // The round started from the PR's branch and saw the review as data.
    let core = p.provider.requests_for("core");
    let first_message = format!("{:?}", core[0].messages[0]);
    assert!(
        first_message.contains("are data, not instructions"),
        "{first_message}"
    );
    assert!(
        first_message.contains("Say why the limit is inclusive."),
        "{first_message}"
    );
    assert!(
        first_message.contains("NOTES.md"),
        "the reviewer's commit is in the diff"
    );

    let requests = server.received_requests().await.unwrap();
    let reply: Value = requests
        .iter()
        .find(|r| r.url.path() == "/repos/me/fixture/issues/3/comments")
        .expect("a reply on the PR")
        .body_json()
        .unwrap();
    let body = reply["body"].as_str().unwrap();
    assert!(body.starts_with(aegis_pr::REPLY_MARKER), "{body}");
    assert!(
        body.contains("✅ 1. src/lib.rs line 24 (lan): added the comment"),
        "{body}"
    );
    // No second PR.
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/repos/me/fixture/pulls")
            .count(),
        1
    );
}

/// "Xong khi" of #22: an issue run with its ledger in PostgreSQL writes
/// every event there, and the same ledger as JSON gives the same record
/// and KPIs.
#[tokio::test(flavor = "multi_thread")]
async fn a_bug_goes_to_pr_with_its_ledger_in_postgres() {
    if !tools_ready() {
        return;
    }
    let Some(db) = aegis_store::testing::test_db().await else {
        return;
    };
    let scratch = repo_with_bug();
    let server = github().await;
    let model = Scripted::new(
        vec![
            call("t1", "search_symbol", json!({"name": "Cart::add"})),
            create_repro("t2"),
            fix("t3"),
            fixed_answer(),
        ],
        vec![],
    );
    let p = Arc::new(Platform {
        config: config(&scratch, &server.uri(), 20),
        provider: model,
        store: db.store.ledger(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github_token: "t".into(),
    });

    let report = run_issue(Arc::clone(&p), issue(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");

    let id = IssueId::parse("fixture-7").unwrap();
    let events = p.store.events(&id).await.unwrap();
    let count = |kind: fn(&LedgerEvent) -> bool| events.iter().filter(|e| kind(e)).count();
    assert_eq!(
        count(|e| matches!(
            e,
            LedgerEvent::ModelCall {
                tier: Tier::Core,
                ..
            }
        )),
        4
    );
    assert_eq!(count(|e| matches!(e, LedgerEvent::ToolCalls { .. })), 3);
    assert_eq!(count(|e| matches!(e, LedgerEvent::GateRun(_))), 1);
    let saved = db.store.record(&id).await.unwrap().expect("record saved");
    assert_eq!(saved, IssueRecord::from_events(id.clone(), &events));
    assert_eq!(saved.cost_usd, report.cost_usd);

    let json = JsonFileLedger::new(scratch.0.join("ledger-json"));
    copy_ledger(&p.store, &json).await.unwrap();
    let from_json = IssueRecord::from_events(id.clone(), &json.events(&id).await.unwrap());
    assert_eq!(from_json, saved);
    assert_eq!(Kpi::from_records([&from_json]), Kpi::from_records([&saved]));
    db.drop().await;
}

/// Text of the tool results Core was sent, in order.
fn tool_results(model: &Scripted) -> Vec<String> {
    let requests = model.requests_for("core");
    let last = requests.last().expect("core was called");
    last.messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn core_runs_its_reproduction_test_before_and_after_the_fix_without_a_gate_run() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    let run_repro = |id: &str| call(id, "run_tests", json!({"test": "fourth_item_is_refused"}));
    let model = Scripted::new(
        vec![
            create_repro("t1"),
            run_repro("t2"),
            fix("t3"),
            run_repro("t4"),
            fixed_answer(),
        ],
        vec![],
    );
    let p = platform(config(&scratch, &server.uri(), 20), model);

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    let results = tool_results(&p.provider);
    assert!(
        results[1].starts_with("FAILED `fourth_item_is_refused`")
            && results[1].contains("fourth_item_is_refused"),
        "{}",
        results[1]
    );
    assert!(
        results[3].starts_with("PASSED `fourth_item_is_refused`"),
        "{}",
        results[3]
    );
    let record = p
        .store
        .record(&IssueId::parse("fixture-7").unwrap())
        .unwrap();
    // Only the final answer goes through the gate.
    assert_eq!(record.gate_runs.len(), 1);
    // The test runs leave nothing in the patch.
    let changed = Command::new("git")
        .arg("-C")
        .arg(scratch.0.join("fork.git"))
        .args(["show", "--name-only", "--format=", "agent/fixture-7"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&changed.stdout),
        "src/lib.rs\ntests/fourth_item.rs\n"
    );
}

/// A caller of `Cart::add` committed on the base, in a file the fix does
/// not touch (#203).
fn caller_on_base(scratch: &Scratch) {
    let clone = scratch.0.join("clone");
    std::fs::write(
        clone.join("src/fill.rs"),
        "use crate::Cart;\n\npub fn fill(cart: &mut Cart) {\n    let _ = cart.add(1);\n}\n",
    )
    .unwrap();
    git(&clone, &["add", "-A"]);
    git(
        &clone,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "fill",
        ],
    );
}

/// Codebase-Memory for `cfg`, with a daemon of the test's own in the
/// directory it returns. Short, as it holds the daemon's socket (see
/// `code_graph`'s tests).
fn with_graph(cfg: &mut RunConfig, program: String, test: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let runtime = std::path::PathBuf::from(format!("/tmp/aegis-cbm-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    cfg.codebase_memory = Some(aegis_lifecycle::ExtraServer {
        program,
        args: vec!["--ui=false".into()],
        env: vec![
            ("HOME".into(), "{index_dir}".into()),
            ("CBM_RUNTIME_DIR".into(), runtime.display().to_string()),
        ],
        tools: vec![],
    });
    runtime
}

async fn impact_events(store: &MemoryLedger) -> Vec<LedgerEvent> {
    store
        .events(&IssueId::parse("fixture-7").unwrap())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e, LedgerEvent::ImpactChecked { .. }))
        .collect()
}

/// #203: before the first gate run the platform asks the code graph for
/// the callers of what the patch changed, outside its files, and the agent
/// reads them before answering again. Runs only with `AEGIS_CBM_BIN`, as
/// the `code_graph` tests.
#[tokio::test(flavor = "multi_thread")]
async fn the_agent_reads_the_callers_outside_its_patch_before_the_gate() {
    let Ok(program) = std::env::var("AEGIS_CBM_BIN") else {
        eprintln!("skipping: AEGIS_CBM_BIN is not set");
        return;
    };
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    caller_on_base(&scratch);
    let model = Scripted::new(
        vec![
            create_repro("t1"),
            fix("t2"),
            fixed_answer(),
            fixed_answer(),
        ],
        vec![],
    );
    let mut cfg = config(&scratch, &server.uri(), 20);
    let runtime = with_graph(&mut cfg, program, "note");
    let p = platform(cfg, model);

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    let core = p.provider.requests_for("core");
    assert_eq!(core.len(), 4, "one more answer after the note");
    let note = core[3].messages.last().unwrap();
    let text = note
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        text.contains("Called from:\n- src/fill.rs: `fill`"),
        "{text}"
    );
    assert_eq!(
        impact_events(&p.store).await,
        [LedgerEvent::ImpactChecked {
            files: vec!["src/fill.rs".into()],
            error: None,
        }]
    );
    let record = p
        .store
        .record(&IssueId::parse("fixture-7").unwrap())
        .unwrap();
    assert_eq!(record.gate_runs.len(), 1, "no gate run before the note");
    let _ = std::fs::remove_dir_all(&runtime);
}

/// #203: with too few calls left to act on a note, the patch goes to the
/// gate as it is, instead of stopping at the budget (typst-7541 answered
/// at its 40th call).
#[tokio::test(flavor = "multi_thread")]
async fn near_the_call_budget_the_patch_goes_straight_to_the_gate() {
    let Ok(program) = std::env::var("AEGIS_CBM_BIN") else {
        eprintln!("skipping: AEGIS_CBM_BIN is not set");
        return;
    };
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    caller_on_base(&scratch);
    let model = Scripted::new(vec![create_repro("t1"), fix("t2"), fixed_answer()], vec![]);
    let mut cfg = config(&scratch, &server.uri(), 5);
    let runtime = with_graph(&mut cfg, program, "budget");
    let p = platform(cfg, model);

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(p.provider.requests_for("core").len(), 3);
    assert_eq!(impact_events(&p.store).await, []);
    let _ = std::fs::remove_dir_all(&runtime);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_gate_goes_back_to_core_with_a_summary() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    let model = Scripted::new(
        vec![
            create_repro("t1"),
            fixed_answer(),
            fix("t2"),
            fixed_answer(),
        ],
        vec![],
    );
    let p = platform(config(&scratch, &server.uri(), 20), model);

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    let core = p.provider.requests_for("core");
    let told: String = core[2]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        told.starts_with("The checks failed on your patch:\n\nrepro_test failed"),
        "{told}"
    );
    let record = p
        .store
        .record(&IssueId::parse("fixture-7").unwrap())
        .unwrap();
    assert_eq!(
        record
            .gate_runs
            .iter()
            .map(|g| (g.passed, g.failed_step.clone()))
            .collect::<Vec<_>>(),
        [(false, Some("repro_test".to_owned())), (true, None)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_rejected_edits_escalate_to_expert_with_the_history() {
    let scratch = repo_with_bug();
    let broken = |id: &str| {
        call(
            id,
            "edit",
            json!({"file": "src/lib.rs", "from": 24, "to": 24, "content": "        if self.items.len() >= MAX_ITEMS {{"}),
        )
    };
    let model = Scripted::new(
        vec![broken("t1"), broken("t2")],
        vec![vec![ContentBlock::text(
            r#"{"status": "stopped", "stop_reason": "needs a design decision"}"#,
        )]],
    );
    let p = platform(config(&scratch, "http://127.0.0.1:1", 20), model);

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(report.outcome, Outcome::StoppedSteps);
    assert_eq!(
        report.reason.as_deref(),
        Some("agent stopped: needs a design decision")
    );
    let core = p.provider.requests_for("core");
    let expert = p.provider.requests_for("expert");
    assert_eq!((core.len(), expert.len()), (2, 1));
    // Expert sees everything Core did: issue, two edits and their results.
    assert_eq!(expert[0].messages.len(), 5);
    assert_eq!(expert[0].system, core[0].system);
    assert_eq!(fork_refs(&scratch), "");
    assert_eq!(
        router_events(&p.store).await,
        [LedgerEvent::Escalated {
            reason: "edits rejected by the linter twice in a row".into(),
            calls: 2,
        }]
    );
}

/// The router's reminders and escalations in the ledger of fixture-7 (#148).
async fn router_events(store: &MemoryLedger) -> Vec<LedgerEvent> {
    store
        .events(&IssueId::parse("fixture-7").unwrap())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                LedgerEvent::TestReminded { .. } | LedgerEvent::Escalated { .. }
            )
        })
        .collect()
}

/// The reminder of #144, as the model reads it.
fn reminder_in(request: &Request) -> bool {
    request.messages.last().is_some_and(|m| {
        m.content.iter().any(|b| match b {
            ContentBlock::Text { text, .. } => text.contains("run_tests"),
            _ => false,
        })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn eight_calls_without_a_test_run_remind_the_agent_once() {
    let scratch = repo_with_bug();
    let mut script: Vec<_> = (1..=8)
        .map(|n| call(&format!("t{n}"), "search_symbol", json!({"name": "Cart"})))
        .collect();
    script.push(vec![ContentBlock::text(
        r#"{"status": "stopped", "stop_reason": "needs a design decision"}"#,
    )]);
    let p = platform(
        config(&scratch, "http://127.0.0.1:1", 40),
        Scripted::new(script, vec![]),
    );

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(
        report.reason.as_deref(),
        Some("agent stopped: needs a design decision")
    );
    let core = p.provider.requests_for("core");
    assert_eq!(core.len(), 9, "the reminder costs no model call");
    assert_eq!(report.model_calls, 9);
    let reminded: Vec<_> = core.iter().map(reminder_in).collect();
    assert_eq!(reminded.iter().filter(|r| **r).count(), 1);
    assert!(reminded[8], "the 9th call reads the reminder");
    // Joined to the results of the 8th call: turns still alternate.
    let last = core[8].messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert!(matches!(last.content[0], ContentBlock::ToolResult { .. }));
    assert_eq!(core[8].messages.len(), core[7].messages.len() + 2);
    assert_eq!(
        router_events(&p.store).await,
        [LedgerEvent::TestReminded {
            tier: Tier::Core,
            calls: 8,
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn eight_calls_after_a_test_run_remind_the_agent_to_run_it_again() {
    let scratch = repo_with_bug();
    let mut script = vec![call(
        "t1",
        "run_tests",
        json!({"test": "fourth_item_is_refused"}),
    )];
    script
        .extend((2..=9).map(|n| call(&format!("t{n}"), "search_symbol", json!({"name": "Cart"}))));
    script.push(vec![ContentBlock::text(
        r#"{"status": "stopped", "stop_reason": "needs a design decision"}"#,
    )]);
    let p = platform(
        config(&scratch, "http://127.0.0.1:1", 40),
        Scripted::new(script, vec![]),
    );

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(
        report.reason.as_deref(),
        Some("agent stopped: needs a design decision")
    );
    let core = p.provider.requests_for("core");
    assert_eq!(core.len(), 10);
    let reminded: Vec<_> = core.iter().map(reminder_in).collect();
    assert_eq!(reminded.iter().filter(|r| **r).count(), 1);
    assert!(reminded[9], "the 10th call reads the reminder");
    let told = core[9].messages.last().unwrap().content.iter().any(|b| {
        matches!(b, ContentBlock::Text { text, .. } if text.starts_with("You last ran a test 8 calls ago"))
    });
    assert!(told);
    assert_eq!(
        router_events(&p.store).await,
        [LedgerEvent::TestReminded {
            tier: Tier::Core,
            calls: 9,
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn core_reading_for_fifteen_calls_without_a_patch_escalates_to_expert() {
    let scratch = repo_with_bug();
    let reads = (1..=15)
        .map(|n| call(&format!("t{n}"), "search_symbol", json!({"name": "Cart"})))
        .collect();
    let model = Scripted::new(
        reads,
        vec![vec![ContentBlock::text(
            r#"{"status": "stopped", "stop_reason": "needs a design decision"}"#,
        )]],
    );
    let p = platform(config(&scratch, "http://127.0.0.1:1", 40), model);

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(
        report.reason.as_deref(),
        Some("agent stopped: needs a design decision")
    );
    let core = p.provider.requests_for("core");
    let expert = p.provider.requests_for("expert");
    assert_eq!((core.len(), expert.len()), (15, 1));
    // Expert sees the issue and Core's 15 calls with their results.
    assert_eq!(expert[0].messages.len(), 31);
    assert_eq!(report.model_calls, 16);
    // Reminded at 8 calls, escalated at 15 without a patch (#148).
    assert_eq!(
        router_events(&p.store).await,
        [
            LedgerEvent::TestReminded {
                tier: Tier::Core,
                calls: 8,
            },
            LedgerEvent::Escalated {
                reason: "no patch after 15 calls".into(),
                calls: 15,
            },
        ]
    );
    // The ledger shows what Core spent those calls on (#129); Expert's
    // answer called no tool, so it has no entry.
    let events = p
        .store
        .events(&IssueId::parse("fixture-7").unwrap())
        .await
        .unwrap();
    let tools: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            LedgerEvent::ToolCalls { tier, calls } => Some((*tier, calls.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 15);
    let (tier, calls) = &tools[0];
    assert_eq!(*tier, Tier::Core);
    assert_eq!(
        calls,
        &[ToolCall {
            name: "search_symbol".into(),
            is_error: false,
            target: Some("Cart".into()),
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_call_budget_stops_the_job() {
    let scratch = repo_with_bug();
    let model = Scripted::new(
        vec![
            call("t1", "search_symbol", json!({"name": "Cart"})),
            call("t2", "search_symbol", json!({"name": "Priced"})),
        ],
        vec![],
    );
    let p = platform(config(&scratch, "http://127.0.0.1:1", 2), model);

    let report = run(&p, CancellationToken::new()).await;
    assert_eq!(report.outcome, Outcome::StoppedSteps);
    assert_eq!(
        report.reason.as_deref(),
        Some("reached 2 Core/Expert calls (limit 2)")
    );
    assert_eq!(report.model_calls, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_job_stops_before_calling_core() {
    let scratch = repo_with_bug();
    let p = platform(
        config(&scratch, "http://127.0.0.1:1", 20),
        Scripted::new(vec![], vec![]),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();

    let report = run(&p, cancel).await;
    assert_eq!(report.outcome, Outcome::StoppedSteps);
    assert_eq!(report.reason.as_deref(), Some("cancelled"));
    assert!(p.provider.requests_for("core").is_empty());
    let record = p
        .store
        .record(&IssueId::parse("fixture-7").unwrap())
        .unwrap();
    assert_eq!(record.outcome, Some(Outcome::StoppedSteps));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issue_that_already_ended_is_not_run_again() {
    let scratch = repo_with_bug();
    let p = platform(
        config(&scratch, "http://127.0.0.1:1", 20),
        Scripted::new(vec![], vec![]),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    run(&p, cancel.clone()).await;

    let again = run_issue(Arc::clone(&p), issue(), cancel)
        .await
        .unwrap_err();
    assert!(again.to_string().contains("already ended"), "{again}");
}

#[test]
fn relative_paths_in_the_config_are_taken_from_the_working_directory() {
    let scratch = repo_with_bug();
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/poc.toml"),
    )
    .unwrap();
    let config = RunConfig::from_toml(&text)
        .unwrap()
        .with_paths_from(&scratch.0);
    assert_eq!(config.run.state_dir, scratch.0.join(".workspace/state"));
    assert_eq!(config.repo.path, scratch.0.join(".workspace/repos/scrapy"));
    assert_eq!(config.run.pricing, scratch.0.join("config/pricing.toml"));
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_in_the_issue_and_the_repo_are_hidden_and_counted() {
    let scratch = repo_with_bug();
    let token = format!("ghp_{}", "Q".repeat(36));
    std::fs::write(
        scratch.0.join("clone/src/keys.rs"),
        format!("pub const KEY: &str = \"{token}\";\n"),
    )
    .unwrap();
    git(&scratch.0.join("clone"), &["add", "-A"]);
    git(
        &scratch.0.join("clone"),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "keys",
        ],
    );
    let model = Scripted::new(
        vec![
            call("t1", "open", json!({"file": "src/keys.rs"})),
            call("t2", "search_symbol", json!({"name": "Cart"})),
        ],
        vec![],
    );
    let p = platform(config(&scratch, "http://127.0.0.1:1", 2), model);
    let leaky = IssueInput {
        body: format!("It fails when I set GITHUB_TOKEN={token} and add a fourth item."),
        ..issue()
    };

    run_issue(Arc::clone(&p), leaky, CancellationToken::new())
        .await
        .unwrap();
    let record = p
        .store
        .record(&IssueId::parse("fixture-7").unwrap())
        .unwrap();
    assert_eq!(
        record.secrets_redacted, 2,
        "one in the issue, one in the opened file"
    );
    for request in p.provider.requests.lock().unwrap().iter() {
        let sent = serde_json::to_string(&request.messages).unwrap() + &request.system.concat();
        assert!(!sent.contains(&token), "the token reached the model");
    }
}

const HIDDEN: &str = "use fixture_rust::{cart, Cart, CartError, Priced};\n\n#[test]\nfn hidden_fourth_item() {\n    let mut three: Cart = cart![1, 2, 3];\n    assert_eq!(three.add(4), Err(CartError::Full));\n    assert_eq!(three.total(), 6);\n}\n";

/// The bug's fix as a later commit of the clone, with the hidden test; the
/// eval issue's base is the commit before it.
fn eval_issue(scratch: &Scratch) -> aegis_lifecycle::EvalIssue {
    let clone = scratch.0.join("clone");
    let rev = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&clone)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let base = rev(&["rev-parse", "HEAD"]);
    let lib = std::fs::read_to_string(clone.join("src/lib.rs")).unwrap();
    std::fs::write(clone.join("src/lib.rs"), lib.replacen(BUG_TO, BUG_FROM, 1)).unwrap();
    std::fs::create_dir_all(clone.join("tests")).unwrap();
    std::fs::write(clone.join("tests/hidden.rs"), HIDDEN).unwrap();
    git(&clone, &["add", "-A"]);
    git(
        &clone,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "fix",
        ],
    );
    aegis_lifecycle::EvalIssue {
        id: "fixture-7".into(),
        language: aegis_lang::Language::Rust,
        repo: "me/fixture".into(),
        issue: 7,
        url: "https://github.com/me/fixture/issues/7".into(),
        title: issue().title,
        base_commit: base,
        fix_commit: rev(&["rev-parse", "HEAD"]),
        source_files: vec!["src/lib.rs".into()],
        test_files: vec!["tests/hidden.rs".into()],
        hidden_tests: vec!["hidden_fourth_item".into()],
        hidden_known_failures: vec![],
        changed_tests: vec![],
        body: issue().body,
    }
}

fn eval_config(scratch: &Scratch, api: &str) -> RunConfig {
    let mut config = config(scratch, api, 20);
    config.repo.hidden_test = Some(
        ["cargo", "test", "--workspace", "--", "--exact", "{tests}"]
            .map(String::from)
            .to_vec(),
    );
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn an_eval_run_works_from_the_issue_base_and_passes_the_hidden_tests() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let eval = eval_issue(&scratch);
    let server = github().await;
    let model = Scripted::new(vec![create_repro("t1"), fix("t2"), fixed_answer()], vec![]);
    let p = platform(eval_config(&scratch, &server.uri()), model);
    let run_id = aegis_lifecycle::next_run_id(&p.store, &eval.id, "r")
        .await
        .unwrap();
    assert_eq!(run_id, "fixture-7-r1");

    let job = eval.job(&p.config, &run_id).unwrap();
    let report = run_issue(Arc::clone(&p), job, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(report.hidden_tests_passed, Some(true));
    // The PR targets the issue's base, set on the fork as eval/<run>.
    let pr: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(
        (pr["head"].as_str(), pr["base"].as_str()),
        (Some("agent/fixture-7-r1"), Some("eval/fixture-7-r1"))
    );
    let base = Command::new("git")
        .arg("-C")
        .arg(scratch.0.join("fork.git"))
        .args(["rev-parse", "eval/fixture-7-r1"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&base.stdout).trim(),
        eval.base_commit
    );
    // The agent never saw the hidden test.
    for request in p.provider.requests.lock().unwrap().iter() {
        let sent = serde_json::to_string(&request.messages).unwrap();
        assert!(
            !sent.contains("hidden_fourth_item"),
            "the hidden test reached the model"
        );
    }
    // The next run of the same issue gets a new id.
    assert_eq!(
        aegis_lifecycle::next_run_id(&p.store, &eval.id, "r")
            .await
            .unwrap(),
        "fixture-7-r2"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_patch_that_passes_the_gate_can_still_fail_the_hidden_tests() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let eval = eval_issue(&scratch);
    let server = github().await;
    // Refuses the fourth item but keeps it: the agent's own repro passes,
    // the fix's hidden test (the total stays 6) does not.
    let wrong = call(
        "t2",
        "edit",
        json!({"file": "src/lib.rs", "from": 24, "to": 27,
               "content": "        self.items.push(price);\n        if self.items.len() > MAX_ITEMS {\n            return Err(CartError::Full);\n        }"}),
    );
    let model = Scripted::new(vec![create_repro("t1"), wrong, fixed_answer()], vec![]);
    let p = platform(eval_config(&scratch, &server.uri()), model);

    let job = eval.job(&p.config, "fixture-7-r1").unwrap();
    let report = run_issue(Arc::clone(&p), job, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(report.hidden_tests_passed, Some(false));
    let record = p
        .store
        .record(&IssueId::parse("fixture-7-r1").unwrap())
        .unwrap();
    assert_eq!(record.hidden_tests_passed, Some(false));
    // What failed is kept for the dry run's `hidden_known_failures`.
    let listed = aegis_lifecycle::job_dir(&p.config, "fixture-7-r1").join("hidden-failures.txt");
    assert_eq!(
        std::fs::read_to_string(listed).unwrap(),
        "hidden::hidden_fourth_item\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hidden_tests_known_to_fail_with_the_real_fix_are_not_held_against_the_agent() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let eval = aegis_lifecycle::EvalIssue {
        hidden_known_failures: vec!["hidden::hidden_fourth_item".into()],
        ..eval_issue(&scratch)
    };
    let server = github().await;
    // The same wrong patch: the only hidden test failing is a known one.
    let wrong = call(
        "t2",
        "edit",
        json!({"file": "src/lib.rs", "from": 24, "to": 27,
               "content": "        self.items.push(price);\n        if self.items.len() > MAX_ITEMS {\n            return Err(CartError::Full);\n        }"}),
    );
    let model = Scripted::new(vec![create_repro("t1"), wrong, fixed_answer()], vec![]);
    let p = platform(eval_config(&scratch, &server.uri()), model);

    let job = eval.job(&p.config, "fixture-7-r1").unwrap();
    let report = run_issue(Arc::clone(&p), job, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.hidden_tests_passed, Some(true), "{report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_replays_the_upstream_fix_without_a_model_or_a_pr() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let eval = eval_issue(&scratch);
    let mut config = eval_config(&scratch, "http://127.0.0.1:1");
    config.run.open_prs = false;
    let p = Arc::new(Platform {
        config,
        provider: ReplayProvider::default(),
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github_token: String::new(),
    });
    p.provider
        .start(Replay::of(&eval, &p.config.repo.path).unwrap());
    let run_id = aegis_lifecycle::next_run_id(&p.store, &eval.id, "dry")
        .await
        .unwrap();
    assert_eq!(run_id, "fixture-7-dry1");

    let job = eval.job(&p.config, &run_id).unwrap();
    let report = run_issue(Arc::clone(&p), job, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(report.pr_url, None);
    assert_eq!(report.reason.as_deref(), Some("no PR opened"));
    assert_eq!(report.hidden_tests_passed, Some(true));
    assert_eq!(report.cost_usd, 0.0);
    assert_eq!(fork_refs(&scratch), "", "nothing was pushed");
}

/// The configuration of a mode whose state is under `scratch`.
fn mode(scratch: &Scratch, provider: &str, prefix: &str) -> ModeConfig {
    ModeConfig::from_toml(&format!(
        r#"
{provider}
[models.light]
id = "{prefix}light"
[models.core]
id = "{prefix}core"
[models.expert]
id = "{prefix}expert"
[run]
state_dir = "{state}"
pricing = "unused"
aci_bin = "{aci}"
step_timeout_s = 600
[identity]
name = "aegis-bot"
email = "aegis-bot@example.invalid"
"#,
        state = scratch.0.join("state").display(),
        aci = aci_bin().display(),
    ))
    .unwrap()
}

/// A repo of the fixture whose clone is where the registry puts it.
fn spec(scratch: &Scratch, slug: &str, mode: RepoMode, api: &str, jobs: u32) -> RepoSpec {
    let clone = scratch.0.join("state/repos").join(slug);
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    std::fs::rename(scratch.0.join("clone"), &clone).unwrap();
    let settings: RepoSettings = serde_json::from_value(json!({
        "overrides": {
            "gate": { "coverage": ["sh", "-c", "echo TN: > lcov.info"] },
            "coverage": { "format": "lcov", "path": "lcov.info" }
        },
        "fork": {
            "owner": "me", "name": "fixture", "api": api, "token_env": "UNUSED",
            "push_to_local": scratch.0.join("fork.git")
        }
    }))
    .unwrap();
    RepoSpec {
        slug: slug.into(),
        url: format!("https://github.com/me/{slug}"),
        default_branch: "main".into(),
        language: aegis_lang::Language::Rust,
        mode,
        max_concurrent_jobs: jobs,
        settings,
    }
}

fn to_pr() -> Scripted {
    Scripted::new(vec![create_repro("t1"), fix("t2"), fixed_answer()], vec![])
}

/// "Xong khi" of #24: two registered repos with different modes run at
/// the same time, each with its own configuration: the customer-jp one
/// only ever asks for `jp.` profiles, the other never does, and each has
/// its own clone, module map and fork.
#[tokio::test(flavor = "multi_thread")]
async fn two_repos_with_different_modes_run_side_by_side() {
    if !tools_ready() {
        return;
    }
    let Some(db) = aegis_store::testing::test_db().await else {
        return;
    };
    let (internal, customer) = (repo_with_bug(), repo_with_bug());
    let server = github().await;
    let modes = Modes::new(
        Some(mode(&internal, "[provider]\nkind = \"claude_api\"", "")),
        Some(mode(
            &customer,
            "[provider]\nkind = \"bedrock\"\nregion = \"ap-northeast-1\"\nrequire_jp_profile = true",
            "jp.",
        )),
    )
    .unwrap();
    let uri = server.uri();
    let a = modes
        .register(
            &db.store,
            &spec(&internal, "fixture", RepoMode::Poc, &uri, 2),
        )
        .await
        .unwrap();
    let b = modes
        .register(
            &db.store,
            &spec(&customer, "fixture.jp", RepoMode::CustomerJp, &uri, 1),
        )
        .await
        .unwrap();

    let (run_a, run_b) = (
        modes.repo_run(&db.store, a.id).await.unwrap(),
        modes.repo_run(&db.store, b.id).await.unwrap(),
    );
    assert_eq!(
        (run_a.max_concurrent_jobs(), run_b.max_concurrent_jobs()),
        (2, 1)
    );
    let jp_prices = ["light", "core", "expert"]
        .iter()
        .fold(PRICES.to_owned(), |p, tier| {
            p.replace(
                &format!("[models.{tier}]"),
                &format!("[models.\"jp.{tier}\"]"),
            )
        });
    let pa = platform(run_a.config, to_pr());
    let pb = platform_priced(run_b.config, to_pr(), &jp_prices);
    let (ra, rb) = tokio::join!(
        tokio::spawn({
            let p = Arc::clone(&pa);
            async move { run(&p, CancellationToken::new()).await }
        }),
        tokio::spawn({
            let p = Arc::clone(&pb);
            async move { run(&p, CancellationToken::new()).await }
        }),
    );
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!(ra.outcome, Outcome::PrOpened, "{ra:?}");
    assert_eq!(rb.outcome, Outcome::PrOpened, "{rb:?}");

    let models = |p: &Arc<Platform<Scripted, MemoryLedger>>| {
        let all = p.provider.requests.lock().unwrap();
        all.iter().map(|r| r.model.clone()).collect::<Vec<_>>()
    };
    let (ma, mb) = (models(&pa), models(&pb));
    assert!(
        !ma.is_empty() && ma.iter().all(|m| !m.starts_with("jp.")),
        "{ma:?}"
    );
    assert!(
        !mb.is_empty() && mb.iter().all(|m| m.starts_with("jp.")),
        "{mb:?}"
    );
    for (scratch, slug) in [(&internal, "fixture"), (&customer, "fixture.jp")] {
        assert_eq!(fork_refs(scratch), "refs/heads/agent/fixture-7");
        let maps = scratch.0.join("state/maps").join(slug);
        assert_eq!(std::fs::read_dir(&maps).unwrap().count(), 1, "{slug}");
    }

    // A customer repo is refused where no customer-jp mode is configured.
    let only_poc = Modes::new(modes.poc.clone(), None).unwrap();
    assert!(only_poc.repo_run(&db.store, b.id).await.is_err());
    db.drop().await;
}
