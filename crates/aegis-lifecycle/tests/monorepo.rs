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
use std::sync::{Arc, Mutex};

use aegis_lang::Profiles;
use aegis_ledger::{IssueId, MemoryLedger, Outcome, PriceTable};
use aegis_lifecycle::{IssueInput, Platform, RunConfig, run_issue};
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
    let root = std::env::temp_dir().join(format!("aegis-monorepo-{}", std::process::id()));
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
    let root = &scratch.0;
    RunConfig::from_toml(&format!(
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
    ))
    .unwrap()
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

#[tokio::test(flavor = "multi_thread")]
async fn a_bug_in_a_repo_of_two_languages_goes_from_issue_to_pr() {
    if !tools_ready() || !pytest_ready() {
        return;
    }
    let scratch = monorepo_with_bugs();
    let server = github().await;
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
    let model = Scripted {
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
    };
    let platform = Arc::new(Platform {
        config: config(&scratch, &server.uri()),
        provider: model,
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github_token: "t".into(),
    });
    let issue = IssueInput {
        id: "mono-3".into(),
        title: "Both carts accept a fourth item".into(),
        body: "`cart![1, 2, 3].add(4)` and `cart_of(1, 2, 3).add(4)` succeed.".into(),
        url: "https://github.com/me/fixture/issues/3".into(),
        base: None,
        hidden: None,
        kind: None,
    };

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
