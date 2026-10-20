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

/// Full commit id of `rev` in the clone.
pub fn resolve(repo: &Path, rev: &str) -> Result<String, LifecycleError> {
    let spec = format!("{rev}^{{commit}}");
    git(repo, &["rev-parse", "--verify", "--end-of-options", &spec])
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
