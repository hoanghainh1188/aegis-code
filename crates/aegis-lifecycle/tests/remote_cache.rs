//! #665: the repo's caches in an object store. Host A's caches, which its
//! fetch refreshed, go up to the bucket after its job; host B starts with
//! an empty `state_dir` and its job starts from them. The bucket is an S3
//! API in memory, the jobs real (ACI, gate in `aegis-sandbox-python`,
//! git), the model scripted.
//!
//! Needs Docker and the `aegis-sandbox-python` image (`docker/python`);
//! skipped otherwise, unless `AEGIS_REQUIRE_DOCKER=1`.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use aegis_lang::Profiles;
use aegis_ledger::{MemoryLedger, Outcome, PriceTable};
use aegis_lifecycle::{IssueInput, Platform, RunConfig, run_issue};
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, StopReason, Usage,
};
use common::{PRICES, aci_bin, git};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const IMAGE: &str = "aegis-sandbox-python";

/// Whether Docker and the image are here.
fn docker() -> Option<PathBuf> {
    let out = Command::new("docker")
        .args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])
        .output()
        .ok()?;
    let host = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let socket = PathBuf::from(host.strip_prefix("unix://")?);
    let image = Command::new("docker")
        .args(["image", "inspect", IMAGE])
        .output()
        .is_ok_and(|o| o.status.success());
    if out.status.success() && image {
        return Some(socket);
    }
    assert!(
        std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
        "docker or {IMAGE} is missing"
    );
    eprintln!("skipping: docker or {IMAGE} is missing");
    None
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The Python fixture with its off-by-one (`>`) committed on `main`, a
/// bare fork next to it.
fn python_repo() -> Scratch {
    let root = std::env::temp_dir().join(format!(
        "aegis-remote-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let clone = root.join("clone");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/python");
    for file in ["pyproject.toml", "cart/__init__.py", "tests/test_cart.py"] {
        let to = clone.join(file);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixture.join(file), &to).unwrap();
    }
    let lib = std::fs::read_to_string(clone.join("cart/__init__.py")).unwrap();
    std::fs::write(
        clone.join("cart/__init__.py"),
        lib.replacen(">= MAX_ITEMS", "> MAX_ITEMS", 1),
    )
    .unwrap();
    std::fs::write(
        clone.join(".gitignore"),
        "__pycache__/\n.venv/\ncoverage.xml\n",
    )
    .unwrap();
    git(&clone, &["init", "-q", "-b", "main"]);
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
            "base",
        ],
    );
    git(&root, &["init", "-q", "--bare", "fork.git"]);
    Scratch(root)
}

/// Light answers by its job; Core follows its script.
struct Scripted {
    core: Mutex<Vec<Vec<ContentBlock>>>,
}

impl ModelProvider for Scripted {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let system = request.system.concat();
        let content = match request.model.as_str() {
            "light" if system.contains("You triage") => vec![ContentBlock::text(
                r#"{"kind": "bug", "has_repro_info": true, "suspected_crates": [], "summary": "Cart accepts a fourth item."}"#,
            )],
            "light" => vec![ContentBlock::text("fix: refuse a fourth item in the cart")],
            "core" => {
                let mut script = self.core.lock().unwrap();
                assert!(!script.is_empty(), "core script exhausted");
                script.remove(0)
            }
            other => panic!("unexpected model {other}"),
        };
        let stop = if content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        Ok(Response {
            id: "msg".into(),
            model: "scripted".into(),
            content,
            stop_reason: Some(stop),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 100,
                ..Usage::default()
            },
        })
    }
}

fn call(id: &str, name: &str, input: serde_json::Value) -> Vec<ContentBlock> {
    vec![ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
    }]
}

const REPRO: &str = "import pytest\n\nfrom cart import CartFullError, cart_of\n\n\ndef test_fourth_item_is_refused():\n    cart = cart_of(1, 2, 3)\n    with pytest.raises(CartFullError):\n        cart.add(4)\n";

fn script() -> Vec<Vec<ContentBlock>> {
    vec![
        call(
            "t1",
            "create_file",
            json!({"file": "tests/test_fourth.py", "content": REPRO}),
        ),
        call(
            "t2",
            "edit",
            json!({"file": "cart/__init__.py", "from": 17, "to": 17, "content": "        if len(self.items) >= MAX_ITEMS:"}),
        ),
        vec![ContentBlock::text(
            json!({
                "status": "fixed",
                "cause": "`add` compared with `>` so a fourth item was accepted.",
                "evidence": [{"location": "cart/__init__.py:17", "excerpt": "MAX_ITEMS"}],
                "changes": [{"location": "cart/__init__.py:17", "note": "compare with >="}],
                "repro_test": {"name": "tests/test_fourth.py", "files": ["tests/test_fourth.py"]},
                "allowed_test_changes": [],
                "stop_reason": null
            })
            .to_string(),
        )],
    ]
}

/// An S3 bucket in memory, path style, that counts its `PUT`s.
#[derive(Clone, Default)]
struct Bucket {
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    puts: Arc<Mutex<u32>>,
}

impl Respond for Bucket {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let path = request.url.path().to_owned();
        let mut objects = self.objects.lock().unwrap();
        match request.method.as_str() {
            "PUT" => {
                *self.puts.lock().unwrap() += 1;
                objects.insert(path, request.body.clone());
                ResponseTemplate::new(200).insert_header("ETag", "\"1\"")
            }
            "GET" => match objects.get(&path) {
                Some(body) => ResponseTemplate::new(200).set_body_bytes(body.clone()),
                None => ResponseTemplate::new(404)
                    .insert_header("Content-Type", "application/xml")
                    .set_body_string("<Error><Code>NoSuchKey</Code><Message>no</Message></Error>"),
            },
            _ => ResponseTemplate::new(405),
        }
    }
}

fn config(root: &Path, state: &str, endpoint: &str) -> RunConfig {
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
sandbox = true
open_prs = false
[run.remote_cache]
url = "s3://aegis-cache/ci"
endpoint = "{endpoint}"
languages = ["python"]
push_interval_s = 0
[budget]
max_cost_usd = 4.0
max_model_calls = 20
[repo]
path = "{clone}"
language = "python"
base = "main"
key = "fixture-py"
[repo.overrides]
fetch = false
audit = false
[repo.fork]
owner = "me"
name = "fixture-py"
api = "http://127.0.0.1:1"
token_env = "UNUSED"
push_to_local = "{fork}"
[identity]
name = "aegis-bot"
email = "aegis-bot@example.invalid"
"#,
        state = root.join(state).display(),
        aci = aci_bin().display(),
        clone = root.join("clone").display(),
        fork = root.join("fork.git").display(),
    ))
    .unwrap()
}

fn issue(id: &str) -> IssueInput {
    IssueInput {
        cve: None,
        id: id.into(),
        title: "Cart accepts a fourth item".into(),
        body: "`cart_of(1, 2, 3).add(4)` does not raise CartFullError.".into(),
        url: "https://github.com/me/fixture-py/issues/7".into(),
        base: None,
        hidden: None,
        kind: None,
        source: Some("github".into()),
        memory: String::new(),
    }
}

fn platform(config: RunConfig) -> Arc<Platform<Scripted, MemoryLedger>> {
    Arc::new(Platform {
        config,
        provider: Scripted {
            core: Mutex::new(script()),
        },
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github: aegis_pr::TokenSource::Static("t".into()),
    })
}

/// The uv cache a container sees at `/home/aegis/.cache/uv`, on the host.
const UV_CACHE: &str = "cache/fixture-py/home_aegis_.cache_uv";

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_host_starts_from_the_caches_another_host_sent_up() {
    if docker().is_none() {
        return;
    }
    // The AWS chain finds these first; nothing of this machine's own
    // AWS setup is read.
    // SAFETY: the only test of this binary, before any thread reads them.
    unsafe {
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_CONFIG_FILE", "/nonexistent/aegis-config");
        std::env::set_var(
            "AWS_SHARED_CREDENTIALS_FILE",
            "/nonexistent/aegis-credentials",
        );
        std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    }
    let server = MockServer::start().await;
    let bucket = Bucket::default();
    Mock::given(any())
        .respond_with(bucket.clone())
        .mount(&server)
        .await;
    let scratch = python_repo();
    let root = &scratch.0;

    // Host A: an earlier job's fetch left the uv cache, refreshed.
    let warm = root.join("state-a").join(UV_CACHE);
    std::fs::create_dir_all(&warm).unwrap();
    std::fs::write(warm.join("wheel"), "from host A").unwrap();
    aegis_gate::mark_refreshed(warm.parent().unwrap()).unwrap();
    let a = platform(config(root, "state-a", &server.uri()));
    let report = run_issue(
        Arc::clone(&a),
        issue("fixture-py-7"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert!(
        bucket
            .objects
            .lock()
            .unwrap()
            .contains_key("/aegis-cache/ci/fixture-py/root-python.tar.gz"),
        "host A's caches went up"
    );
    assert_eq!(*bucket.puts.lock().unwrap(), 1);

    // Host B: a fresh state dir. Its job starts from host A's caches and
    // passes its gate; nothing refreshed them, so nothing goes up again.
    let b = platform(config(root, "state-b", &server.uri()));
    let report = run_issue(
        Arc::clone(&b),
        issue("fixture-py-9"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("state-b").join(UV_CACHE).join("wheel")).unwrap(),
        "from host A"
    );
    assert_eq!(*bucket.puts.lock().unwrap(), 1, "host B sent nothing up");
}

/// The caches belong to the sandbox: without it there are none to keep.
#[test]
fn remote_cache_needs_the_sandbox() {
    let text = "[provider]\nkind = \"claude_api\"\n[models.light]\nid = \"l\"\n[models.core]\nid = \"c\"\n[models.expert]\nid = \"e\"\n[run]\nstate_dir = \"/tmp/aegis-rc\"\npricing = \"p\"\n[run.remote_cache]\nurl = \"s3://aegis-cache\"\n[budget]\nmax_cost_usd = 4.0\n[repo]\npath = \"/tmp/aegis-rc\"\nlanguage = \"python\"\nbase = \"main\"\nkey = \"k\"\n[repo.fork]\nowner = \"o\"\nname = \"n\"\ntoken_env = \"T\"\n[identity]\nname = \"a\"\nemail = \"a@e.invalid\"\n";
    let err = RunConfig::from_toml(text).unwrap_err().to_string();
    assert!(err.contains("run.remote_cache needs run.sandbox"), "{err}");
}
