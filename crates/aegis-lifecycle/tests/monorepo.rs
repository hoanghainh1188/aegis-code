//! "Xong khi" of #71: a repo of two languages runs from issue to PR. The
//! Rust fixture is the repo's root, the Python fixture lives under `py/`
//! with its own profile (`[[repo.dirs]]`); the same off-by-one bug is in
//! both. The model is scripted; the ACI server, the gate of each part
//! (cargo, rustfmt, clippy at the root; pytest in `py/`), the push and the
//! ledger are real. GitHub is faked with wiremock.

mod common;

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use aegis_lang::Profiles;
use aegis_ledger::{IssueId, MemoryLedger, Outcome, PriceTable};
use aegis_lifecycle::{HiddenTests, IssueInput, Platform, RunConfig, run_issue};
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, StopReason, Usage,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use common::{BUG_FROM, BUG_TO, PRICES, Scratch, aci_bin, fork_refs, git, github, tools_ready};

const PY_BUG_FROM: &str = "len(self.items) >= MAX_ITEMS";
const PY_BUG_TO: &str = "len(self.items) > MAX_ITEMS";

const PY_REPRO: &str = "import pytest

from cart import CartFullError, cart_of


def test_fourth_item_is_refused():
    cart = cart_of(1, 2, 3)
    with pytest.raises(CartFullError):
        cart.add(4)
";

/// Light answers by its system prompt; Core follows its script.
struct Scripted {
    core: Mutex<VecDeque<Vec<ContentBlock>>>,
    requests: Mutex<Vec<Request>>,
}

fn reply(content: Vec<ContentBlock>) -> Response {
    let tool = content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    Response {
        id: "msg".into(),
        model: "scripted".into(),
        content,
        stop_reason: Some(if tool {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        }),
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
                r#"{"kind": "bug", "has_repro_info": true, "suspected_crates": [], "summary": "Both carts accept a fourth item."}"#,
            )],
            "light" if system.contains("failed check") => {
                vec![ContentBlock::text("a check failed")]
            }
            "light" => vec![ContentBlock::text(
                "fix: refuse a fourth item in both carts",
            )],
            "core" => self
                .core
                .lock()
                .unwrap()
                .pop_front()
                .expect("core script exhausted"),
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

/// `py` holds the Python fixture; both have the bug, committed on `main`.
fn monorepo_with_bugs() -> Scratch {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "aegis-monorepo-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let repo = root.join("clone");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for (from, to) in [
        ("rust/Cargo.toml", "Cargo.toml"),
        ("rust/Cargo.lock", "Cargo.lock"),
        ("rust/src/lib.rs", "src/lib.rs"),
        ("python/pyproject.toml", "py/pyproject.toml"),
        ("python/cart/__init__.py", "py/cart/__init__.py"),
        ("python/tests/test_cart.py", "py/tests/test_cart.py"),
    ] {
        let to = repo.join(to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixtures.join(from), to).unwrap();
    }
    for (file, from, to) in [
        ("src/lib.rs", BUG_FROM, BUG_TO),
        ("py/cart/__init__.py", PY_BUG_FROM, PY_BUG_TO),
    ] {
        let path = repo.join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
    }
    std::fs::write(
        repo.join(".gitignore"),
        "target/\nlcov.info\n__pycache__/\n.pytest_cache/\ncoverage.xml\n",
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "base",
        ],
    );
    git(&root, &["init", "-q", "--bare", "fork.git"]);
    Scratch(root)
}

/// No coverage tool and no ruff needed: stand-ins write the reports, and
/// the Python part is formatted and linted by nothing.
fn config(scratch: &Scratch, api: &str) -> RunConfig {
    RunConfig::from_toml(&config_toml(scratch, api)).unwrap()
}

fn config_toml(scratch: &Scratch, api: &str) -> String {
    let root = &scratch.0;
    format!(
        r#"
[provider]
kind = "claude_api"
[models.light]
id = "light"
[models.core]
id = "core"
[models.expert]
id = "expert"
[run]
state_dir = "{state}"
pricing = "unused"
aci_bin = "{aci}"
step_timeout_s = 600
[budget]
max_cost_usd = 4.0
max_model_calls = 20
[repo]
path = "{clone}"
language = "rust"
base = "main"
key = "monorepo"
[repo.overrides.gate]
coverage = ["sh", "-c", "echo TN: > lcov.info"]
[repo.overrides.coverage]
format = "lcov"
path = "lcov.info"
[[repo.dirs]]
path = "py"
language = "python"
[repo.dirs.overrides]
edit_format = false
[repo.dirs.overrides.gate]
format = false
lint = false
coverage = ["sh", "-c", "echo '<coverage/>' > coverage.xml"]
[repo.dirs.overrides.coverage]
format = "cobertura"
path = "coverage.xml"
[repo.fork]
owner = "me"
name = "fixture"
api = "{api}"
token_env = "UNUSED"
push_to_local = "{fork}"
[identity]
name = "aegis-bot"
email = "aegis-bot@example.invalid"
"#,
        state = root.join("state").display(),
        aci = aci_bin().display(),
        clone = root.join("clone").display(),
        fork = root.join("fork.git").display(),
    )
}

fn pytest_ready() -> bool {
    let ok = Command::new("python")
        .args(["-m", "pytest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_PROFILE_TOOLS").as_deref() != Ok("1"),
            "pytest is required"
        );
        eprintln!("skipping: pytest missing");
    }
    ok
}

/// Core's script: find both `add`s, write and run a Python reproduction
/// test, fix both, answer.
fn scripted() -> Scripted {
    let answer = json!({
        "status": "fixed",
        "cause": "Both `add`s compared with `>`, so a fourth item was accepted.",
        "changes": [
            {"location": "src/lib.rs:24", "note": "compare with >="},
            {"location": "py/cart/__init__.py:17", "note": "compare with >="}
        ],
        "repro_test": {
            "name": "py/tests/test_fourth.py::test_fourth_item_is_refused",
            "files": ["py/tests/test_fourth.py"]
        },
        "allowed_test_changes": [],
        "stop_reason": null
    });
    Scripted {
        core: Mutex::new(
            vec![
                call("t1", "search_symbol", json!({"name": "Cart::add"})),
                call(
                    "t2",
                    "create_file",
                    json!({"file": "py/tests/test_fourth.py", "content": PY_REPRO}),
                ),
                call(
                    "t3",
                    "run_tests",
                    json!({"test": "py/tests/test_fourth.py::test_fourth_item_is_refused"}),
                ),
                call(
                    "t4",
                    "edit",
                    json!({"file": "py/cart/__init__.py", "from": 17, "to": 17,
                           "content": "        if len(self.items) >= MAX_ITEMS:"}),
                ),
                call(
                    "t5",
                    "edit",
                    json!({"file": "src/lib.rs", "from": 24, "to": 24,
                           "content": "        if self.items.len() >= MAX_ITEMS {"}),
                ),
                vec![ContentBlock::text(answer.to_string())],
            ]
            .into(),
        ),
        requests: Mutex::new(vec![]),
    }
}

fn platform(config: RunConfig) -> Arc<Platform<Scripted, MemoryLedger>> {
    Arc::new(Platform {
        config,
        provider: scripted(),
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github_token: "t".into(),
    })
}

fn issue(id: &str, hidden: Option<HiddenTests>) -> IssueInput {
    IssueInput {
        id: id.into(),
        title: "Both carts accept a fourth item".into(),
        body: "`cart![1, 2, 3].add(4)` and `cart_of(1, 2, 3).add(4)` succeed.".into(),
        url: "https://github.com/me/fixture/issues/3".into(),
        base: None,
        hidden,
        kind: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bug_in_a_repo_of_two_languages_goes_from_issue_to_pr() {
    if !tools_ready() || !pytest_ready() {
        return;
    }
    let scratch = monorepo_with_bugs();
    let server = github().await;
    let platform = platform(config(&scratch, &server.uri()));
    let issue = issue("mono-3", None);

    let report = run_issue(Arc::clone(&platform), issue, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(fork_refs(&scratch), "refs/heads/agent/mono-3");

    // The agent saw both languages: the symbol in each, the Python test
    // run in `py/` (failing before the fix), and both conventions.
    let requests = platform.provider.requests.lock().unwrap().clone();
    let core: Vec<&Request> = requests.iter().filter(|r| r.model == "core").collect();
    let results: Vec<String> = core
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(
        results[0].contains("py/cart/__init__.py") && results[0].contains("src/lib.rs"),
        "{}",
        results[0]
    );
    assert!(results[2].starts_with("FAILED"), "{}", results[2]);
    let system = core[0].system.concat();
    assert!(system.contains("`py/` (Python)"), "{system}");

    let record = platform
        .store
        .record(&IssueId::parse("mono-3").unwrap())
        .unwrap();
    assert_eq!(record.gate_runs.len(), 1);
    assert!(record.gate_runs[0].passed);

    // Each part was gated in its own directory.
    let logs = scratch.0.join("state/jobs/mono-3/gate/logs");
    assert!(logs.join("test.log").exists(), "the root's tests ran");
    assert!(logs.join("py/repro_test.log").exists(), "py's repro ran");
    assert!(logs.join("py/test.log").exists(), "py's tests ran");

    // The PR holds both fixes and the new test.
    let show = |spec: &str| {
        let out = Command::new("git")
            .arg("-C")
            .arg(scratch.0.join("fork.git"))
            .args(["show", &format!("agent/mono-3:{spec}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(show("src/lib.rs").contains(BUG_FROM));
    assert!(show("py/cart/__init__.py").contains(PY_BUG_FROM));
    assert!(show("py/tests/test_fourth.py").contains("test_fourth_item_is_refused"));

    let pr: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    let body = pr["body"].as_str().unwrap();
    assert!(body.contains("repro_test ✔ · test ✔ · coverage"), "{body}");
}

/// The fix's own test of `py/`, on a branch of the clone: what an eval
/// issue's hidden test is.
const PY_HIDDEN: &str = "import pytest

from cart import CartFullError, cart_of


def test_a_full_cart_stays_full():
    cart = cart_of(1, 2, 3)
    for item in (4, 5):
        with pytest.raises(CartFullError):
            cart.add(item)
    assert len(cart.items) == 3
";

/// Writes the analysis settings and which coverage reports exist to
/// `record`, and leaves a task for the gate to wait for.
fn fake_scanner(scratch: &Scratch) -> std::path::PathBuf {
    let record = scratch.0.join("sonar-record");
    std::fs::create_dir_all(&record).unwrap();
    let script = scratch.0.join("scanner.sh");
    std::fs::write(
        &script,
        format!(
            "cp .aegis-gate-sonar/project.properties {record}/settings\n\
             ls lcov.info py/coverage.xml > {record}/reports 2>&1\n\
             mkdir -p .aegis-gate-sonar/work\n\
             printf 'projectKey=mono\\nceTaskId=T1\\n' > .aegis-gate-sonar/work/report-task.txt\n",
            record = record.display()
        ),
    )
    .unwrap();
    script
}

/// A Sonar server whose analysis passes.
async fn sonar_server() -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    for (at, body) in [
        (
            "/api/ce/task",
            json!({"task": {"id": "T1", "status": "SUCCESS", "componentKey": "mono",
                            "analysisId": "A1"}}),
        ),
        (
            "/api/qualitygates/project_status",
            json!({"projectStatus": {"status": "OK", "conditions": []}}),
        ),
        (
            "/api/issues/search",
            json!({"paging": {"pageIndex": 1, "pageSize": 100, "total": 0}, "issues": []}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn hidden_tests_and_sonar_go_by_the_part_of_the_repo() {
    // #199: the hidden tests of `py/` run there with its command; Sonar
    // analyses the repo once, with both parts' coverage reports.
    if !tools_ready() || !pytest_ready() {
        return;
    }
    let scratch = monorepo_with_bugs();
    let clone = scratch.0.join("clone");
    git(&clone, &["checkout", "-q", "-b", "fix"]);
    std::fs::write(clone.join("py/tests/test_hidden.py"), PY_HIDDEN).unwrap();
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
    let fix = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&clone)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    git(&clone, &["checkout", "-q", "main"]);
    let (github, sonar) = (github().await, sonar_server().await);
    let scanner = fake_scanner(&scratch);
    let mut cfg = RunConfig::from_toml(&format!(
        "{}\n[sonar]\nurl = \"{}\"\ntoken_env = \"UNUSED\"\nproject_key = \"mono\"\n\
         pull_request = false\nscanner = [\"sh\", \"{}\"]\npoll_s = 1\n",
        config_toml(&scratch, &github.uri()),
        sonar.uri(),
        scanner.display()
    ))
    .unwrap();
    cfg.sonar.as_mut().unwrap().token = Some(aegis_secrets::Secret::new("squ_test"));
    let platform = platform(cfg);
    let hidden = HiddenTests {
        fix_commit: fix.trim().to_owned(),
        test_files: vec!["py/tests/test_hidden.py".into()],
        dir: "py".into(),
        command: ["python", "-m", "pytest", "-q", "tests/test_hidden.py"]
            .map(String::from)
            .to_vec(),
        known_failures: vec![],
    };

    let report = run_issue(
        Arc::clone(&platform),
        issue("mono-4", Some(hidden)),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(report.hidden_tests_passed, Some(true), "{report:?}");
    let log =
        std::fs::read_to_string(scratch.0.join("state/jobs/mono-4/hidden-tests.log")).unwrap();
    assert!(log.contains("1 passed"), "{log}");

    let record = scratch.0.join("sonar-record");
    let settings = std::fs::read_to_string(record.join("settings")).unwrap();
    assert!(
        settings.contains("sonar.rust.lcov.reportPaths=lcov.info\n"),
        "{settings}"
    );
    assert!(
        settings.contains("sonar.python.coverage.reportPaths=py/coverage.xml\n"),
        "{settings}"
    );
    let reports = std::fs::read_to_string(record.join("reports")).unwrap();
    assert_eq!(reports, "lcov.info\npy/coverage.xml\n");
}
