//! The job's own checkout: a worktree of the platform's clone at the base
//! revision (ADR 0003 §6: two jobs never share a tree). The agent edits it,
//! the gate checks it, and it is removed when the job ends. Git runs on
//! the platform's own clone, without global or system config.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::LifecycleError;

fn git(repo: &Path, args: &[&str]) -> Result<String, LifecycleError> {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| LifecycleError::Git {
            args: args.join(" "),
            message: e.to_string(),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(LifecycleError::Git {
            args: args.join(" "),
            message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

/// `path` as it is at `rev` in the clone (bytes: test data may be
/// binary), or `None` when it does not exist there.
pub fn show(repo: &Path, rev: &str, path: &str) -> Result<Option<Vec<u8>>, LifecycleError> {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(repo)
        .args(["show", &format!("{rev}:{path}")])
        .output()
        .map_err(|e| LifecycleError::Git {
            args: "show".into(),
            message: e.to_string(),
        })?;
    Ok(out.status.success().then_some(out.stdout))
}

/// Full commit id of `rev` in the clone.
pub fn resolve(repo: &Path, rev: &str) -> Result<String, LifecycleError> {
    let spec = format!("{rev}^{{commit}}");
    git(repo, &["rev-parse", "--verify", "--end-of-options", &spec])
}

/// The best common ancestor of `a` and `b` in the clone.
pub fn merge_base(repo: &Path, a: &str, b: &str) -> Result<String, LifecycleError> {
    git(repo, &["merge-base", "--end-of-options", a, b])
}

/// The patch from `from` to `to` that a PR holds (#312), as `git diff`
/// shows it, redacted as the platform redacts anything it keeps, and cut
/// to what the store keeps.
pub fn patch(repo: &Path, from: &str, to: &str) -> Result<aegis_store::Patch, LifecycleError> {
    let full = git(
        repo,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--end-of-options",
            from,
            to,
        ],
    )?;
    let patch = aegis_store::Patch {
        diff: aegis_aci::redact(&full).text,
        truncated: false,
    };
    Ok(patch.capped())
}

/// The changes from `from` to `to`, as `git diff` shows them, at most
/// `max_chars` (cut at a line, with a note of what was left out).
pub fn diff(repo: &Path, from: &str, to: &str, max_chars: usize) -> Result<String, LifecycleError> {
    let full = git(
        repo,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--end-of-options",
            from,
            to,
        ],
    )?;
    if full.chars().count() <= max_chars {
        return Ok(full);
    }
    let mut kept = String::new();
    for line in full.lines() {
        if kept.chars().count() + line.chars().count() + 1 > max_chars {
            break;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    let left = full.lines().count() - kept.lines().count();
    kept.push_str(&format!("[{left} more lines of the diff not shown]"));
    Ok(kept)
}

/// A digest of the files at `rev` that `keep` selects: their paths and
/// blob ids (the hash of each file's content). The same files give the
/// same digest whatever the commit, so an index made from it is redone
/// only when one of them changes (#24).
pub fn files_digest(
    repo: &Path,
    rev: &str,
    keep: impl Fn(&Path) -> bool,
) -> Result<String, LifecycleError> {
    let spec = format!("{rev}^{{commit}}");
    // `-z`: `<mode> <type> <id>\t<path>` entries, paths unquoted.
    let listing = git(
        repo,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            "--end-of-options",
            &spec,
        ],
    )?;
    let kept: String = listing
        .split('\0')
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let mut meta = meta.split(' ');
            let (_mode, kind, id) = (meta.next()?, meta.next()?, meta.next()?);
            (kind == "blob" && keep(Path::new(path))).then(|| format!("{id} {path}\n"))
        })
        .collect();
    hash_object(repo, kept.as_bytes())
}

/// Git's id of `bytes` as a blob, without writing it.
fn hash_object(repo: &Path, bytes: &[u8]) -> Result<String, LifecycleError> {
    use std::io::Write;
    use std::process::Stdio;
    let failed = |message: String| LifecycleError::Git {
        args: "hash-object --stdin".into(),
        message,
    };
    let mut child = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(repo)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(e.to_string()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| failed("no stdin".into()))?;
    stdin.write_all(bytes).map_err(|e| failed(e.to_string()))?;
    drop(stdin);
    let out = child
        .wait_with_output()
        .map_err(|e| failed(e.to_string()))?;
    if !out.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[derive(Debug)]
pub struct Worktree {
    clone: PathBuf,
    path: PathBuf,
}

impl Worktree {
    pub fn create(clone: &Path, commit: &str, path: &Path) -> Result<Self, LifecycleError> {
        // Git takes a relative path from the clone, not from here.
        let path = &std::path::absolute(path).map_err(|e| LifecycleError::io(path, &e))?;
        if path.exists() {
            let _ = git(
                clone,
                &["worktree", "remove", "--force", &path.display().to_string()],
            );
            if path.exists() {
                std::fs::remove_dir_all(path).map_err(|e| LifecycleError::io(path, &e))?;
            }
            git(clone, &["worktree", "prune"])?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LifecycleError::io(parent, &e))?;
        }
        let shown = path.display().to_string();
        git(
            clone,
            &["worktree", "add", "--detach", "--force", &shown, commit],
        )?;
        Ok(Self {
            clone: clone.to_path_buf(),
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let shown = self.path.display().to_string();
        let _ = git(&self.clone, &["worktree", "remove", "--force", &shown]);
        let _ = git(&self.clone, &["worktree", "prune"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_changes_with_the_kept_files_only() {
        let repo = std::env::temp_dir().join(format!("aegis-digest-{}", std::process::id()));
        std::fs::create_dir_all(repo.join("src dir")).unwrap();
        let g = |args: &[&str]| {
            git(
                &repo,
                &[
                    &["-c", "user.name=t", "-c", "user.email=t@example.com"],
                    args,
                ]
                .concat(),
            )
            .unwrap()
        };
        g(&["init", "-q", "-b", "main"]);
        let commit = |file: &str, text: &str| {
            std::fs::write(repo.join(file), text).unwrap();
            g(&["add", "-A"]);
            g(&["commit", "-qm", file]);
            files_digest(&repo, "main", |p| p.extension().is_some_and(|e| e == "rs")).unwrap()
        };
        let first = commit("src dir/lib.rs", "fn a() {}\n");
        assert_eq!(first.len(), 40);
        // Another commit, same Rust files: the index is kept.
        assert_eq!(commit("README.md", "hi\n"), first);
        let changed = commit("src dir/lib.rs", "fn b() {}\n");
        assert_ne!(changed, first);
        // Back to the same content: the same digest as before.
        assert_eq!(commit("src dir/lib.rs", "fn a() {}\n"), first);
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn a_relative_worktree_path_is_taken_from_here_not_from_the_clone() {
        let root = std::env::temp_dir().join(format!("aegis-job-{}", std::process::id()));
        let clone = root.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        std::fs::write(clone.join("a.txt"), "a\n").unwrap();
        let g = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&clone)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["add", "-A"]);
        g(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "x",
        ]);
        let commit = resolve(&clone, "main").unwrap();

        let relative = PathBuf::from(format!("target/aegis-job-test-{}/repo", std::process::id()));
        let tree = Worktree::create(&clone, &commit, &relative).unwrap();
        assert!(tree.path().is_absolute());
        assert_eq!(
            std::fs::read_to_string(relative.join("a.txt")).unwrap(),
            "a\n"
        );
        assert!(!clone.join(&relative).exists());
        drop(tree);
        assert!(!relative.exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(relative.parent().unwrap());
    }
}
