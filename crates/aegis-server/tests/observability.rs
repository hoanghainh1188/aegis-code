//! "Xong khi" of #32: a whole job, from the queue to its PR, through a
//! worker and the real runner, with its secrets in a fake secret store
//! (#30). Its logs are captured as the server writes them: every line of
//! the job carries its id, none holds a secret or the content of a file
//! of the repo. Its model calls show up in `/metrics`.
//!
//! The model and GitHub are faked with wiremock; the repo, the ACI
//! server, the Rust gate and the push to a bare fork are real. One test in
//! this file: it sets the process' log subscriber.

#[path = "../../aegis-lifecycle/tests/common/mod.rs"]
mod lifecycle;

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aegis_ledger::IssueId;
use aegis_lifecycle::{ModeConfig, Modes, RepoSettings, RepoSpec};
use aegis_provider::ContentBlock;
use aegis_secrets::{MemorySource, Secrets};
use aegis_server::lifecycle::LifecycleRunner;
use aegis_server::metrics::{self, metrics};
use aegis_server::worker::{WorkerConfig, run_worker};
use aegis_store::{JobSource, JobStatus, NewJob, RepoMode};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracing_subscriber::EnvFilter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

use lifecycle::{PRICES, Scratch, aci_bin, fork_refs, git, repo_with_bug, tools_ready};

const MODEL_KEY: (&str, &str) = ("AEGIS_TEST_MODEL_KEY", "sk-leak-model-0c41e9");
const FORK_TOKEN: (&str, &str) = ("AEGIS_TEST_FORK_TOKEN", "ghp-leak-fork-77ad02");
/// In the repo's own code: no log line may hold it.
const CANARY: &str = "repo-content-canary-5b1f";

/// Where the server's log lines go in this test.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The Messages API: Light answers by its job, Core follows its script.
struct Model {
    core: Mutex<VecDeque<Vec<ContentBlock>>>,
}

impl Respond for Model {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let system = body["system"].to_string();
        let content = match body["model"].as_str().unwrap() {
            "light" if system.contains("You triage") => vec![ContentBlock::text(
                r#"{"kind": "bug", "has_repro_info": true, "suspected_crates": [], "summary": "Cart accepts a fourth item."}"#,
            )],
            "light" => vec![ContentBlock::text("fix: refuse a fourth item in the cart")],
            "core" => self.core.lock().unwrap().pop_front().expect("core script"),
            other => panic!("unexpected model {other}"),
        };
        let tool = content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg",
            "type": "message",
            "role": "assistant",
            "model": body["model"],
            "content": content,
            "stop_reason": if tool { "tool_use" } else { "end_turn" },
            "usage": {
                "input_tokens": 200,
                "output_tokens": 50,
                "cache_read_input_tokens": 800,
                "cache_creation_input_tokens": 100
            }
        }))
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

fn core_script() -> VecDeque<Vec<ContentBlock>> {
    VecDeque::from([
        call("t1", "search_symbol", json!({"name": "Cart::add"})),
        call(
            "t2",
            "create_file",
            json!({"file": "tests/fourth_item.rs", "content": REPRO}),
        ),
        call(
            "t3",
            "edit",
            json!({"file": "src/lib.rs", "from": 24, "to": 24, "content": "        if self.items.len() >= MAX_ITEMS {"}),
        ),
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
        )],
    ])
}

async fn model() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Model {
            core: Mutex::new(core_script()),
        })
        .mount(&server)
        .await;
    server
}

/// The issue and the PR.
async fn github() -> MockServer {
    let server = lifecycle::github().await;
    Mock::given(method("GET"))
        .and(path("/repos/me/fixture/issues/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "title": "Cart accepts a fourth item",
            "body": "`cart![1, 2, 3].add(4)` returns Ok(()), expected Err(CartError::Full).",
            "html_url": "https://github.com/me/fixture/issues/7"
        })))
        .mount(&server)
        .await;
    server
}

/// The fixture with the canary in its code, committed.
fn repo_with_canary() -> Scratch {
    let scratch = repo_with_bug();
    let clone = scratch.0.join("clone");
    let lib = clone.join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    // At the end: the scripted edit's line numbers stay the fixture's.
    std::fs::write(&lib, format!("{text}// {CANARY}\n")).unwrap();
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
            "canary",
        ],
    );
    scratch
}

fn mode(scratch: &Scratch, model_api: &str) -> ModeConfig {
    let prices = scratch.0.join("prices.toml");
    std::fs::write(&prices, PRICES).unwrap();
    ModeConfig::from_toml(&format!(
        r#"
[provider]
kind = "claude_api"
api_key_env = "{key}"
base_url = "{model_api}"
[models.light]
id = "light"
[models.core]
id = "core"
[models.expert]
id = "expert"
[run]
state_dir = "{state}"
pricing = "{prices}"
aci_bin = "{aci}"
step_timeout_s = 600
[identity]
name = "aegis-bot"
email = "aegis-bot@example.invalid"
"#,
        key = MODEL_KEY.0,
        state = scratch.0.join("state").display(),
        prices = prices.display(),
        aci = aci_bin().display(),
    ))
    .unwrap()
}

fn spec(scratch: &Scratch, github_api: &str) -> RepoSpec {
    let clone = scratch.0.join("state/repos/fixture");
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    std::fs::rename(scratch.0.join("clone"), &clone).unwrap();
    let settings: RepoSettings = serde_json::from_value(json!({
        "overrides": {
            "gate": { "coverage": ["sh", "-c", "echo TN: > lcov.info"] },
            "coverage": { "format": "lcov", "path": "lcov.info" }
        },
        "fork": {
            "owner": "me", "name": "fixture", "api": github_api, "token_env": FORK_TOKEN.0,
            "push_to_local": scratch.0.join("fork.git")
        }
    }))
    .unwrap();
    RepoSpec {
        slug: "fixture".into(),
        url: "https://github.com/me/fixture".into(),
        default_branch: "main".into(),
        language: aegis_lang::Language::Rust,
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        settings,
    }
}

fn worker_config() -> WorkerConfig {
    WorkerConfig {
        worker_id: "observed".into(),
        max_workers: 1,
        lease: Duration::from_secs(30),
        renew_every: Duration::from_secs(1),
        poll_every: Duration::from_millis(100),
        drain: Duration::from_secs(5),
        retry_base: Duration::from_secs(60),
        max_cost_usd: 4.0,
    }
}

/// The first line of `text` that starts with `start`, its value.
fn value_of(text: &str, start: &str) -> f64 {
    let line = text
        .lines()
        .find(|l| l.starts_with(start))
        .unwrap_or_else(|| panic!("no {start} in\n{text}"));
    line.rsplit(' ').next().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_whole_job_logs_its_id_and_no_secret_or_code_and_is_counted() {
    let captured = Captured::default();
    let writer = captured.clone();
    let logs = aegis_server::logs::subscriber(move || writer.clone(), EnvFilter::new("debug"));
    tracing::subscriber::set_global_default(logs).unwrap();
    if !tools_ready() {
        return;
    }
    let Some(db) = aegis_store::testing::test_db().await else {
        return;
    };
    let store = db.store.clone();
    let scratch = repo_with_canary();
    let (model_api, github_api) = (model().await, github().await);
    let modes = Modes::new(Some(mode(&scratch, &model_api.uri())), None).unwrap();
    let repo = modes
        .register(&store, &spec(&scratch, &github_api.uri()))
        .await
        .unwrap();
    let secrets = Secrets::new(MemorySource::new([MODEL_KEY, FORK_TOKEN]));
    let runner = LifecycleRunner::new(store.clone(), modes, secrets)
        .await
        .unwrap();
    let job = store
        .create_job(&NewJob {
            repo_id: repo.id,
            issue_ref: "me/fixture#7".into(),
            ledger_key: IssueId::parse("fixture-7").unwrap(),
            source: JobSource::Github,
        })
        .await
        .unwrap();

    let shutdown = CancellationToken::new();
    let worker = tokio::spawn(run_worker(
        store.clone(),
        Arc::new(runner),
        worker_config(),
        shutdown.clone(),
    ));
    let started = Instant::now();
    let ended = loop {
        let j = store.job(job.id).await.unwrap().unwrap();
        if !matches!(j.status, JobStatus::Queued | JobStatus::Running) {
            break j;
        }
        assert!(started.elapsed() < Duration::from_secs(600), "{j:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    shutdown.cancel();
    worker.await.unwrap();
    assert_eq!(ended.status, JobStatus::Done, "{ended:?}");
    assert_eq!(
        ended.pr_url.as_deref(),
        Some("https://github.com/me/fixture/pull/3")
    );
    assert_eq!(fork_refs(&scratch), "refs/heads/agent/fixture-7");

    let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    // Every line inside the job's span names it, its repo and attempt.
    let in_job: Vec<&Value> = lines
        .iter()
        .filter(|l| l["span"]["name"] == "job" || l["spans"][0]["name"] == "job")
        .collect();
    assert!(in_job.len() >= 2, "{text}");
    for line in &in_job {
        let job_span = &line["spans"][0];
        assert_eq!(job_span["job_id"], job.id.0, "{line}");
        assert_eq!(job_span["attempt"], 1, "{line}");
    }
    assert!(
        in_job
            .iter()
            .any(|l| l["spans"][0]["repo"] == "fixture" && l["spans"][0]["language"] == "rust"),
        "{text}"
    );
    // No secret, nor the repo's code, nor what the agent wrote.
    let code = std::fs::read_to_string(scratch.0.join("state/repos/fixture/src/lib.rs")).unwrap();
    let mut forbidden = vec![MODEL_KEY.1, FORK_TOKEN.1, CANARY, REPRO];
    forbidden.extend(code.lines().map(str::trim).filter(|l| l.len() > 24));
    for needle in forbidden {
        assert!(!text.contains(needle), "the logs hold `{needle}`");
    }

    // The job's calls, from the responses: 2 Light, 4 Core.
    let m = metrics().render().unwrap();
    let labels = r#"repo="fixture",language="rust""#;
    assert_eq!(
        value_of(
            &m,
            &format!(r#"aegis_provider_calls_total{{{labels},tier="core",result="ok"}}"#)
        ),
        4.0
    );
    assert_eq!(
        value_of(
            &m,
            &format!(r#"aegis_provider_tokens_total{{{labels},tier="core",kind="cache_read"}}"#)
        ),
        3200.0
    );
    assert!(value_of(&m, r#"aegis_job_attempts_total{status="done"}"#) >= 1.0);
    assert_eq!(value_of(&m, "aegis_jobs_running "), 0.0);

    // The same over HTTP, with the queue as it is now: empty.
    let response = metrics::router(store.clone())
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/openmetrics-text")
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let scraped = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(
        value_of(&scraped, r#"aegis_queue_jobs{state="ready"}"#),
        0.0
    );
    assert_eq!(value_of(&scraped, "aegis_queue_oldest_ready_seconds "), 0.0);
    db.drop().await;
}
