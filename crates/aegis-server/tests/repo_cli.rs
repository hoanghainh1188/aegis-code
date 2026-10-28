//! `aegis-server repo add|update|show|list` (#234): a repo goes from a
//! file to a row and a clone, checked against its mode first and audited.

use std::path::{Path, PathBuf};
use std::process::Command;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_secrets::Secrets;
use aegis_server::repo_cli;
use aegis_store::testing::test_db;
use aegis_store::{Page, Store};

const POC: &str = include_str!("../../../config/poc.toml");

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = std::env::temp_dir().join(format!(
        "aegis-repo-cli-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// An upstream repo with one commit on `main`.
fn upstream(dir: &Path) -> PathBuf {
    let up = dir.join("upstream");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "-q", "-b", "main"]);
    commit(&up, "one")
}

fn commit(up: &Path, text: &str) -> PathBuf {
    std::fs::write(up.join("a.txt"), text).unwrap();
    git(up, &["add", "-A"]);
    git(up, &["commit", "-qm", text]);
    up.to_owned()
}

/// The poc mode, its state under `dir`; no `customer-jp` mode.
fn modes(dir: &Path) -> Modes {
    let mut poc = ModeConfig::from_toml(POC).unwrap();
    poc.run.state_dir = dir.join("state");
    Modes::new(Some(poc), None).unwrap()
}

fn repo_file(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

fn demo(url: &Path, extra: &str) -> String {
    format!(
        r#"
slug = "demo"
url = "{}"
language = "go"
{extra}

[settings.fork]
owner = "aegis-bot"
name = "demo"
token_env = "AEGIS_TEST_FORK_TOKEN"
"#,
        url.display()
    )
}

async fn run(store: &Store, modes: &Modes, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    repo_cli::run(store, modes, &Secrets::none(), &args).await
}

async fn actions(store: &Store) -> Vec<String> {
    let log = store
        .audit_log(None, Page::first(Page::MAX_LIMIT))
        .await
        .unwrap();
    log.items.into_iter().rev().map(|e| e.action).collect()
}

#[tokio::test]
async fn a_repo_is_added_cloned_updated_and_shown_by_command() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let tmp = scratch("add");
    let up = upstream(&tmp.0);
    let modes = modes(&tmp.0);
    let file = repo_file(&tmp.0, "demo.toml", &demo(&up, ""));

    let out = run(s, &modes, &["add", &file]).await.unwrap();
    let clone = tmp.0.join("state/repos/demo");
    assert!(out.contains("repo `demo` added"), "{out}");
    assert!(out.contains(&clone.display().to_string()), "{out}");
    assert!(out.contains("grant <email> demo"), "{out}");
    let first = git(&up, &["rev-parse", "HEAD"]);
    assert_eq!(git(&clone, &["rev-parse", "main"]), first);
    let repo = s.repo_by_slug("demo").await.unwrap().unwrap();
    assert_eq!(
        (repo.language.as_str(), repo.max_concurrent_jobs),
        ("go", 1)
    );
    let err = run(s, &modes, &["add", &file]).await.unwrap_err();
    assert!(err.contains("already taken"), "{err}");
    assert!(clone.exists(), "a clone it did not make is kept");

    // Upstream moves on; `update` fetches it with the new settings.
    commit(&up, "two");
    let file = repo_file(
        &tmp.0,
        "demo.json",
        &serde_json::to_string(
            &toml::from_str::<repo_cli::RepoFile>(&demo(&up, "max_concurrent_jobs = 3")).unwrap(),
        )
        .unwrap(),
    );
    let out = run(s, &modes, &["update", &file]).await.unwrap();
    assert!(out.contains("repo `demo` updated"), "{out}");
    assert_eq!(
        git(&clone, &["rev-parse", "main"]),
        git(&up, &["rev-parse", "HEAD"])
    );
    let repo = s.repo_by_slug("demo").await.unwrap().unwrap();
    assert_eq!(repo.max_concurrent_jobs, 3);

    let shown = run(s, &modes, &["show", "demo"]).await.unwrap();
    let back: repo_cli::RepoFile = serde_json::from_str(&shown).unwrap();
    assert_eq!(back.max_concurrent_jobs, 3);
    assert_eq!(back.settings.fork.owner, "aegis-bot");
    let listed = run(s, &modes, &["list"]).await.unwrap();
    assert!(listed.starts_with("demo\tpoc\tgo\tmain\t"), "{listed}");

    assert_eq!(
        actions(s).await,
        ["repo.register", "repo.update"],
        "each change audited once"
    );
}

#[tokio::test]
async fn a_repo_that_cannot_run_is_refused_before_anything_is_written() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let tmp = scratch("refused");
    let up = upstream(&tmp.0);
    let modes = modes(&tmp.0);
    let cases = [
        (
            demo(&up, "mode = \"customer-jp\""),
            "no configuration for mode",
        ),
        (
            demo(&up, "").replace("token_env", "imag = \"x\"\ntoken_env"),
            "unknown field",
        ),
        (demo(&tmp.0.join("nowhere"), ""), "git clone"),
        (demo(&up, "default_branch = \"trunk\""), "trunk"),
    ];
    for (n, (text, why)) in cases.iter().enumerate() {
        let file = repo_file(&tmp.0, &format!("bad{n}.toml"), text);
        let err = run(s, &modes, &["add", &file]).await.unwrap_err();
        assert!(err.contains(why), "{why}: {err}");
        assert!(!tmp.0.join("state/repos/demo").exists(), "{why}");
    }
    assert!(s.repo_by_slug("demo").await.unwrap().is_none());
    let file = repo_file(&tmp.0, "demo.toml", &demo(&up, ""));
    let err = run(s, &modes, &["update", &file]).await.unwrap_err();
    assert!(err.contains("no repo `demo`"), "{err}");
    assert!(actions(s).await.is_empty());
    assert_eq!(run(s, &modes, &["list"]).await.unwrap(), "no repos");
    assert!(run(s, &modes, &["remove", "demo"]).await.is_err());
}
