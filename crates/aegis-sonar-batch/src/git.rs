//! Git for the batch: group commits, and the integration tree the group
//! branches are merged into. Git runs without global or system config,
//! hooks or prompts, like the rest of the platform.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::BatchError;

const MAX_BATCH_ID_CHARS: usize = 48;

/// Who the batch's commits are by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}

fn command(repo: &Path, args: &[&str], author: Option<&Author>) -> Result<Output, BatchError> {
    let mut cmd = Command::new("git");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(repo)
        .args(args);
    if let Some(a) = author {
        cmd.env("GIT_AUTHOR_NAME", &a.name)
            .env("GIT_AUTHOR_EMAIL", &a.email)
            .env("GIT_COMMITTER_NAME", &a.name)
            .env("GIT_COMMITTER_EMAIL", &a.email);
    }
    cmd.output().map_err(|e| BatchError::Git {
        args: args.join(" "),
        message: e.to_string(),
    })
}

fn git(repo: &Path, args: &[&str], author: Option<&Author>) -> Result<String, BatchError> {
    let out = command(repo, args, author)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(BatchError::Git {
            args: args.join(" "),
            message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

/// `agent/integration-<batch_id>`; the id is a plain git ref part.
pub fn integration_branch(batch_id: &str) -> Result<String, BatchError> {
    let valid = !batch_id.is_empty()
        && batch_id.chars().count() <= MAX_BATCH_ID_CHARS
        && batch_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && !batch_id.starts_with(['.', '-'])
        && !batch_id.ends_with('.')
        && !batch_id.contains("..")
        && !batch_id.ends_with(".lock");
    if valid {
        Ok(format!("agent/integration-{batch_id}"))
    } else {
        Err(BatchError::InvalidBatchId(batch_id.to_owned()))
    }
}

/// A commit of `tree` on `parent`: a group's gated tree as its branch tip.
pub fn commit_tree(
    repo: &Path,
    tree: &str,
    parent: &str,
    message: &str,
    author: &Author,
) -> Result<String, BatchError> {
    git(
        repo,
        &["commit-tree", tree, "-p", parent, "-m", message],
        Some(author),
    )
}

/// A checkout at the integration branch's tip, merged into one group at a
/// time. The branch moves only when a merge has passed the gate.
#[derive(Debug)]
pub struct IntegrationTree {
    path: PathBuf,
    branch: String,
    author: Author,
}

impl IntegrationTree {
    /// Starts `branch` at the checkout's `HEAD` (the batch's base).
    pub fn start(path: &Path, branch: &str, author: Author) -> Result<Self, BatchError> {
        let tree = Self {
            path: path.to_path_buf(),
            branch: branch.to_owned(),
            author,
        };
        tree.advance()?;
        Ok(tree)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn head(&self) -> Result<String, BatchError> {
        git(&self.path, &["rev-parse", "--verify", "HEAD"], None)
    }

    /// Merges `commit`; `false` on a conflict, with the merge undone.
    pub fn merge(&self, commit: &str, message: &str) -> Result<bool, BatchError> {
        let args = ["merge", "--no-ff", "--no-edit", "-m", message, commit];
        let out = command(&self.path, &args, Some(&self.author))?;
        if out.status.success() {
            return Ok(true);
        }
        let merging = command(
            &self.path,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
            None,
        )?;
        if merging.status.success() {
            git(&self.path, &["merge", "--abort"], None)?;
            return Ok(false);
        }
        Err(BatchError::Git {
            args: args.join(" "),
            message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }

    /// Back to `commit`, with what the gate left in the tree removed.
    pub fn reset(&self, commit: &str) -> Result<(), BatchError> {
        git(&self.path, &["reset", "-q", "--hard", commit], None)?;
        git(&self.path, &["clean", "-q", "-fd"], None)?;
        Ok(())
    }

    /// Moves the integration branch to `HEAD`.
    pub fn advance(&self) -> Result<(), BatchError> {
        let full = format!("refs/heads/{}", self.branch);
        git(&self.path, &["update-ref", &full, "HEAD"], None)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_batch_ids_make_a_branch() {
        assert_eq!(
            integration_branch("scrapy-sonar-1").unwrap(),
            "agent/integration-scrapy-sonar-1"
        );
        for bad in ["", "a/b", "-x", "x.", "a..b", "x.lock", "a b", "é"] {
            assert!(integration_branch(bad).is_err(), "{bad}");
        }
    }
}
