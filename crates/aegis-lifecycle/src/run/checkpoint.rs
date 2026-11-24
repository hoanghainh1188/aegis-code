//! A job's checkpoint (#662): after each model call (its tools run, their
//! results in the session) and after each failed gate, the session, the
//! router's counters, the loop's own and the tree's changes are kept in
//! the ledger's store. A worker that claims the job again, on this machine
//! or another, makes a fresh tree at the same commit, applies the changes
//! and goes on from there; the prompt it sends starts with the same
//! bytes, so the prompt cache still holds within its lifetime.
//!
//! Only a job that opens a PR is checkpointed: not a review round (its
//! tree starts from a PR's head this worker fetches), a CVE job (the
//! platform's upgrade comes first) or a Sonar group (several trees in one
//! job).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use aegis_ledger::{CHECKPOINT_VERSION, Checkpoint, IssueId, LedgerStore};
use aegis_orchestrator::Session;
use aegis_router::RouterState;
use serde::{Deserialize, Serialize};

use crate::LifecycleError;

/// What the Core/Expert loop needs to go on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DriveState {
    pub session: Session,
    pub router: RouterState,
    pub answer_retries: u32,
    pub dearest_call_usd: f64,
    pub first_expert_call: bool,
    pub impact_checks: usize,
    pub known_functions: BTreeSet<String>,
    /// The router moved to Expert within a loop that a crash cut short:
    /// its reason, for the ledger, which had not been told.
    #[serde(default)]
    pub pending_escalation: Option<String>,
}

/// A checkpoint that can be resumed: its commits are in the clone.
#[derive(Debug, Clone)]
pub(crate) struct Resume {
    pub base: String,
    pub start: String,
    pub diff: String,
    pub turn: u32,
    pub state: DriveState,
}

fn git(
    dir: &Path,
    args: &[&str],
    index: Option<&Path>,
    input: Option<&[u8]>,
) -> Result<String, LifecycleError> {
    let failed = |message: String| LifecycleError::Git {
        args: args.join(" "),
        message,
    };
    let mut c = Command::new("git");
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.autocrlf=false",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        c.env("GIT_INDEX_FILE", index);
    }
    let mut child = c.spawn().map_err(|e| failed(e.to_string()))?;
    if let Some(bytes) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| failed("no stdin".into()))?;
        stdin.write_all(bytes).map_err(|e| failed(e.to_string()))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| failed(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

/// The changes of the job's `tree` from `start`, new files included and
/// what `.gitignore` ignores left out, as `git diff --binary`: built in an
/// index of its own (`scratch`), the tree's own left alone.
pub(crate) fn tree_diff(
    tree: &Path,
    start: &str,
    scratch: &Path,
) -> Result<String, LifecycleError> {
    let _ = std::fs::remove_file(scratch);
    git(
        tree,
        &["read-tree", "--end-of-options", start],
        Some(scratch),
        None,
    )?;
    git(tree, &["add", "-A", "--", "."], Some(scratch), None)?;
    let diff = git(
        tree,
        &[
            "diff",
            "--cached",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "--end-of-options",
            start,
        ],
        Some(scratch),
        None,
    );
    let _ = std::fs::remove_file(scratch);
    diff
}

/// Applies a [`tree_diff`] to a fresh tree at its start.
pub(crate) fn apply_diff(tree: &Path, diff: &str) -> Result<(), LifecycleError> {
    if diff.trim().is_empty() {
        return Ok(());
    }
    git(
        tree,
        &["apply", "--binary", "--whitespace=nowarn", "-"],
        None,
        Some(diff.as_bytes()),
    )
    .map(drop)
}

/// Where a checkpoint of the job in `tree` is written.
#[derive(Clone, Copy)]
pub(crate) struct Saving<'a> {
    pub issue: &'a IssueId,
    pub base: &'a str,
    pub start: &'a str,
    pub tree: &'a Path,
    pub job_dir: &'a Path,
}

/// Keeps `state` and the tree's changes as the job's checkpoint at its
/// `turn`-th model call. A checkpoint that cannot be written is said in
/// the log: the job goes on, and would start over after a crash.
pub(crate) async fn save<S: LedgerStore>(store: &S, at: Saving<'_>, turn: u32, state: &DriveState) {
    let state = match serde_json::to_value(state) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(issue = %at.issue, error = %e, "encoding the checkpoint");
            return;
        }
    };
    let (tree, start, scratch) = (
        at.tree.to_owned(),
        at.start.to_owned(),
        at.job_dir.join("checkpoint.index"),
    );
    let diff = tokio::task::spawn_blocking(move || tree_diff(&tree, &start, &scratch)).await;
    let diff = match diff {
        Ok(Ok(diff)) => diff,
        Ok(Err(e)) => {
            tracing::warn!(issue = %at.issue, error = %e, "the tree's changes for the checkpoint");
            return;
        }
        Err(e) => {
            tracing::warn!(issue = %at.issue, error = %e, "the tree's changes for the checkpoint");
            return;
        }
    };
    let checkpoint = Checkpoint {
        version: CHECKPOINT_VERSION,
        turn,
        base: at.base.to_owned(),
        start: at.start.to_owned(),
        diff,
        state,
    };
    if let Err(e) = store.save_checkpoint(at.issue, &checkpoint).await {
        tracing::warn!(issue = %at.issue, error = %e, "keeping the checkpoint");
    }
}

/// The job's checkpoint, when there is one this worker can go on from:
/// of this format, its commits in the `clone`, no more calls than the
/// ledger has. Any other is said in the log and the job starts over.
pub(crate) async fn load<S: LedgerStore>(
    store: &S,
    issue: &IssueId,
    clone: &Path,
    model_calls: u32,
) -> Option<Resume> {
    let checkpoint = match store.checkpoint(issue).await {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(%issue, error = %e, "reading the checkpoint; starting over");
            return None;
        }
    };
    let refuse = |why: String| {
        tracing::warn!(%issue, why, "the checkpoint is not resumed; starting over");
        None
    };
    if checkpoint.version != CHECKPOINT_VERSION {
        return refuse(format!(
            "format {} is not {CHECKPOINT_VERSION}",
            checkpoint.version
        ));
    }
    if checkpoint.turn > model_calls {
        return refuse(format!(
            "it was written at call {}, the ledger has {model_calls}",
            checkpoint.turn
        ));
    }
    for commit in [&checkpoint.base, &checkpoint.start] {
        if crate::job::resolve(clone, commit).is_err() {
            return refuse(format!("commit {commit} is not in the clone"));
        }
    }
    let state: DriveState = match serde_json::from_value(checkpoint.state) {
        Ok(state) => state,
        Err(e) => return refuse(format!("its state does not read: {e}")),
    };
    Some(Resume {
        base: checkpoint.base,
        start: checkpoint.start,
        diff: checkpoint.diff,
        turn: checkpoint.turn,
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        git(dir, args, None, None).unwrap();
    }

    #[test]
    fn a_trees_changes_are_applied_to_a_fresh_tree_at_its_start() {
        let root = std::env::temp_dir().join(format!(
            "aegis-checkpoint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("src")).unwrap();
        std::fs::write(a.join("src/lib.rs"), "fn a() {}\n").unwrap();
        std::fs::write(a.join("old.txt"), "gone\n").unwrap();
        std::fs::write(a.join(".gitignore"), "target/\n").unwrap();
        run(&a, &["init", "-q", "-b", "main"]);
        run(&a, &["add", "-A"]);
        run(
            &a,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "commit",
                "-qm",
                "base",
            ],
        );
        let start = git(&a, &["rev-parse", "HEAD"], None, None).unwrap();
        let start = start.trim();

        // Edited, new (text and binary), removed, and ignored files.
        std::fs::write(a.join("src/lib.rs"), "fn a() { b() }\n").unwrap();
        std::fs::write(a.join("src/new.rs"), "fn b() {}\n").unwrap();
        std::fs::write(a.join("blob.bin"), [0u8, 159, 146, 150, 255]).unwrap();
        std::fs::remove_file(a.join("old.txt")).unwrap();
        std::fs::create_dir_all(a.join("target")).unwrap();
        std::fs::write(a.join("target/out"), "built").unwrap();
        let diff = tree_diff(&a, start, &root.join("scratch.index")).unwrap();
        assert!(!diff.contains("target/out"), "{diff}");
        assert!(
            !diff.contains(&root.display().to_string()),
            "no host path: {diff}"
        );
        // The tree's own index is untouched.
        let status = git(&a, &["status", "--porcelain"], None, None).unwrap();
        assert!(status.contains("?? src/new.rs"), "{status}");

        run(&root, &["clone", "-q", a.to_str().unwrap(), "b"]);
        apply_diff(&b, &diff).unwrap();
        assert_eq!(
            std::fs::read_to_string(b.join("src/lib.rs")).unwrap(),
            "fn a() { b() }\n"
        );
        assert_eq!(
            std::fs::read_to_string(b.join("src/new.rs")).unwrap(),
            "fn b() {}\n"
        );
        assert_eq!(
            std::fs::read(b.join("blob.bin")).unwrap(),
            [0u8, 159, 146, 150, 255]
        );
        assert!(!b.join("old.txt").exists());
        apply_diff(&b, "").unwrap();

        // A checkpoint is resumed only with its commits in the clone, in
        // its format, and no further on than the ledger.
        let store = aegis_ledger::MemoryLedger::new();
        let issue = IssueId::parse("cp-1").unwrap();
        let state = DriveState {
            session: Session::default(),
            router: aegis_router::Router::default().state(),
            answer_retries: 1,
            dearest_call_usd: 0.5,
            first_expert_call: false,
            impact_checks: 0,
            known_functions: BTreeSet::new(),
            pending_escalation: None,
        };
        let keep = |base: &str, version: u32, turn: u32| Checkpoint {
            version,
            turn,
            base: base.to_owned(),
            start: start.to_owned(),
            diff: diff.clone(),
            state: serde_json::to_value(&state).unwrap(),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(load(&store, &issue, &a, 9).await.is_none(), "none kept");
            for bad in [
                keep(&"f".repeat(40), CHECKPOINT_VERSION, 3),
                keep(start, CHECKPOINT_VERSION + 1, 3),
                keep(start, CHECKPOINT_VERSION, 10),
            ] {
                store.save_checkpoint(&issue, &bad).await.unwrap();
                assert!(load(&store, &issue, &a, 9).await.is_none(), "{bad:?}");
            }
            store
                .save_checkpoint(&issue, &keep(start, CHECKPOINT_VERSION, 3))
                .await
                .unwrap();
            let resume = load(&store, &issue, &a, 9).await.expect("resumable");
            assert_eq!((resume.turn, resume.state), (3, state.clone()));
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}
