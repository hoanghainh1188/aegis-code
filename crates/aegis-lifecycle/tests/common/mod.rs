//! What the lifecycle tests share: the `aegis-aci` binary, a git repo of
//! the Rust fixture with its bare fork, the run configuration, and a
//! faked GitHub.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use aegis_lifecycle::RunConfig;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The `aegis-aci` binary, built once for these tests.
pub fn aci_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args([
                "build",
                "-q",
                "-p",
                "aegis-aci",
                "--bin",
                "aegis-aci",
                "--message-format=json",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find_map(|m| m["executable"].as_str().map(PathBuf::from))
            .expect("aegis-aci executable")
    })
}

pub fn tools_ready() -> bool {
    let ok = ["fmt", "clippy"].iter().all(|t| {
        Command::new("cargo")
            .args([t, "--version"])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_PROFILE_TOOLS").as_deref() != Ok("1"),
            "rustfmt and clippy are required"
        );
        eprintln!("skipping: rustfmt or clippy missing");
    }
    ok
}

pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub const BUG_FROM: &str = "self.items.len() >= MAX_ITEMS";
pub const BUG_TO: &str = "self.items.len() > MAX_ITEMS";

/// The Rust fixture with the off-by-one bug committed on `main`, and a
/// bare fork next to it.
pub fn repo_with_bug() -> Scratch {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "aegis-lifecycle-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let repo = root.join("clone");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/rust");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    for file in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
        std::fs::copy(fixture.join(file), repo.join(file)).unwrap();
    }
    let lib = std::fs::read_to_string(repo.join("src/lib.rs")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), lib.replacen(BUG_FROM, BUG_TO, 1)).unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\nlcov.info\n").unwrap();
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

pub const PRICES: &str = r#"
regional_surcharge = 0.10
[cache_write_multiplier]
5m = 1.25
1h = 2.0
[models.light]
input = 0.1
output = 0.5
cache_read_multiplier = 0.1
[models.core]
input = 2.0
output = 10.0
cache_read_multiplier = 0.05
[models.expert]
input = 4.0
output = 20.0
cache_read_multiplier = 0.05
"#;

pub fn config(scratch: &Scratch, api: &str, max_calls: u32) -> RunConfig {
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
max_model_calls = {max_calls}
[repo]
path = "{clone}"
language = "rust"
base = "main"
key = "fixture"
[repo.overrides.gate]
coverage = ["sh", "-c", "echo TN: > lcov.info"]
[repo.overrides.coverage]
format = "lcov"
path = "lcov.info"
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

pub async fn github() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/me/fixture/pulls"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": 3, "html_url": "https://github.com/me/fixture/pull/3"
        })))
        .mount(&server)
        .await;
    server
}

pub fn fork_refs(scratch: &Scratch) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(scratch.0.join("fork.git"))
        .args(["for-each-ref", "--format=%(refname)"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
