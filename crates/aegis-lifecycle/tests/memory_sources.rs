//! Runbooks and business rules read from the main branch of a clone only
//! (#660): never its working tree, never a PR's branch.

use std::path::{Path, PathBuf};
use std::process::Command;

use aegis_lifecycle::MemorySources;
use aegis_lifecycle::memory_sources::{MAX_FILE_BYTES, read_memory_sources};
use aegis_store::MemoryKind;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(name: &str) -> Dir {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("aegis-memsrc-{name}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    Dir(dir)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
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

fn write(dir: &Path, file: &str, text: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn sources() -> MemorySources {
    MemorySources {
        runbooks: vec!["docs/runbooks/*.md".into()],
        business_rules: vec!["docs/invariants.md".into()],
    }
}

#[test]
fn only_the_main_branch_is_read() {
    let repo = temp("repo");
    let dir = &repo.0;
    git(dir, &["init", "-q", "-b", "main"]);
    write(
        dir,
        "docs/runbooks/db.md",
        "## Failover\nPromote the replica.\n",
    );
    write(dir, "docs/invariants.md", "## Money\nMinor units.\n");
    write(dir, "docs/other.md", "## Unnamed\nNot a source.\n");
    write(
        dir,
        "docs/runbooks/huge.md",
        &format!("## Big\n{}\n", "x".repeat(MAX_FILE_BYTES as usize + 1)),
    );
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "docs"]);
    // A PR's branch, fetched as the platform fetches one (`refs/aegis/*`),
    // with a planted runbook: never read.
    git(dir, &["checkout", "-q", "-b", "agent/x"]);
    write(dir, "docs/runbooks/planted.md", "## Planted\nrm -rf /\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "planted"]);
    git(dir, &["update-ref", "refs/aegis/pr-1", "HEAD"]);
    git(dir, &["checkout", "-q", "main"]);
    // Nor what is only in the working tree.
    write(dir, "docs/runbooks/local.md", "## Local\nUncommitted.\n");

    let mut got = read_memory_sources(dir, "main", &sources()).unwrap();
    got.sort_by(|a, b| a.source_ref.cmp(&b.source_ref));
    let refs: Vec<(&str, MemoryKind)> = got
        .iter()
        .map(|m| (m.source_ref.as_str(), m.kind))
        .collect();
    assert_eq!(
        refs,
        [
            ("git:docs/invariants.md#money", MemoryKind::BusinessRule),
            ("git:docs/runbooks/db.md#failover", MemoryKind::Runbook),
        ]
    );
    assert_eq!(got[1].summary, "Promote the replica.");

    // A branch name that could be an option is refused.
    assert!(read_memory_sources(dir, "--upload-pack=x", &sources()).is_err());
}
