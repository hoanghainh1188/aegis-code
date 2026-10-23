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
use aegis_ledger::{IssueId, MemoryLedger, Outcome, PriceTable};
use aegis_lifecycle::{
    IssueInput, IssueReport, Platform, Replay, ReplayProvider, RunConfig, run_issue,
};
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, StopReason, Usage,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use common::{
    BUG_FROM, BUG_TO, PRICES, Scratch, config, fork_refs, git, github, repo_with_bug, tools_ready,
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
        let content = match request.model.as_str() {
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
    }
}

fn platform(config: RunConfig, model: Scripted) -> Arc<Platform<Scripted, MemoryLedger>> {
    Arc::new(Platform {
        config,
        provider: model,
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
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
