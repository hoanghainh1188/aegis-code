//! Where a job's code is and runs (#663): the boundary between the
//! lifecycle (the agent loop, the router, the ledger) and the job's
//! workspace. The loop reaches the workspace through [`ExecutionEnv`]
//! only: the agent's tools (the ACI and codebase-memory MCP servers), the
//! gate, and the tree's changes for checkpoints (#662). Today the one
//! environment is [`LocalExecutionEnv`], a worktree of the platform's
//! clone on this host with its MCP servers over stdio and the gate on the
//! local Docker daemon, as before; a remote one (#664) runs them on
//! another host.

use std::future::Future;
use std::path::{Path, PathBuf};

use aegis_gate::{DependencyPolicy, GateReport};
use aegis_lang::RepoProfiles;
use aegis_orchestrator::McpServerSpec;
use tokio_util::sync::CancellationToken;

use super::{JobTree, build_cache, fetch_base, gate, tool_specs};
use crate::answer::FinalAnswer;
use crate::job::Worktree;
use crate::{LifecycleError, RunConfig};

/// A job's workspace and what runs on it.
pub(crate) trait ExecutionEnv: Send + Sync {
    /// The workspace's root as this host's processes see it: the module
    /// map, the code graph, the answer's sources, the PR's commit.
    fn root(&self) -> &Path;

    /// The job's own directory for its scratch files (logs, caches, the
    /// checkpoint's index), never in the workspace.
    fn job_dir(&self) -> &Path;

    /// The MCP servers of the agent's tools and how to reach them.
    fn tool_servers(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
    ) -> Result<Vec<McpServerSpec>, LifecycleError>;

    /// Gets each part of the workspace ready before the agent's first
    /// call: its dependencies fetched, its build cache seeded. `Some` is
    /// why the job cannot go on.
    fn prepare_parts(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Option<String>, LifecycleError>> + Send;

    /// The verification gate on the workspace as it is, against `base`.
    fn run_gate(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        base: &str,
        answer: &FinalAnswer,
        policy: &DependencyPolicy,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<GateReport, LifecycleError>> + Send;

    /// The workspace's changes from `start`, as `git diff --binary`.
    fn export_diff(
        &self,
        start: &str,
    ) -> impl Future<Output = Result<String, LifecycleError>> + Send;

    /// Applies an [`ExecutionEnv::export_diff`] to the workspace at its
    /// start.
    fn apply_diff(&self, diff: &str) -> impl Future<Output = Result<(), LifecycleError>> + Send;
}

/// A worktree of the platform's clone on this host (ADR 0003 §6), its
/// MCP servers child processes over stdio, the gate on the local Docker
/// daemon. The worktree goes with it.
#[derive(Debug)]
pub(crate) struct LocalExecutionEnv {
    tree: Worktree,
    job_dir: PathBuf,
}

impl LocalExecutionEnv {
    /// A fresh worktree of `clone` at `start`, in `job_dir/repo`.
    pub(crate) fn create(
        clone: &Path,
        start: &str,
        job_dir: &Path,
    ) -> Result<Self, LifecycleError> {
        Ok(Self {
            tree: Worktree::create(clone, start, &job_dir.join("repo"))?,
            job_dir: job_dir.to_owned(),
        })
    }
}

/// Runs `work` off the async runtime, a panic or an abort as an error.
async fn blocking<T: Send + 'static>(
    what: &'static str,
    work: impl FnOnce() -> Result<T, LifecycleError> + Send + 'static,
) -> Result<T, LifecycleError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| LifecycleError::Config(format!("{what}: {e}")))?
}

impl ExecutionEnv for LocalExecutionEnv {
    fn root(&self) -> &Path {
        self.tree.path()
    }

    fn job_dir(&self) -> &Path {
        &self.job_dir
    }

    fn tool_servers(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
    ) -> Result<Vec<McpServerSpec>, LifecycleError> {
        tool_specs(cfg, profiles, self.root(), &self.job_dir)
    }

    async fn prepare_parts(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, LifecycleError> {
        for part in profiles.parts() {
            let dir = self.root().join(&part.dir);
            if let Some(why) = fetch_base(cfg, part, &dir, &self.job_dir, cancel).await? {
                return Ok(Some(why));
            }
            // The gate would start the tree's build from the cache anyway;
            // doing it now spares the agent's first `run_tests` a cold
            // build (#137).
            if let Some(cache) = build_cache(cfg, part) {
                blocking("seeding the build cache", move || Ok(cache.seed(&dir)?)).await?;
            }
        }
        Ok(None)
    }

    async fn run_gate(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        base: &str,
        answer: &FinalAnswer,
        policy: &DependencyPolicy,
        cancel: &CancellationToken,
    ) -> Result<GateReport, LifecycleError> {
        let job = JobTree {
            tree: self.root(),
            base,
            job_dir: &self.job_dir,
        };
        gate(cfg, profiles, job, answer, policy, cancel).await
    }

    async fn export_diff(&self, start: &str) -> Result<String, LifecycleError> {
        let (tree, start, scratch) = (
            self.root().to_owned(),
            start.to_owned(),
            self.job_dir.join("checkpoint.index"),
        );
        blocking("the tree's changes", move || {
            super::checkpoint::tree_diff(&tree, &start, &scratch)
        })
        .await
    }

    async fn apply_diff(&self, diff: &str) -> Result<(), LifecycleError> {
        let (tree, diff) = (self.root().to_owned(), diff.to_owned());
        blocking("applying the changes", move || {
            super::checkpoint::apply_diff(&tree, &diff)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.name=t", "-c", "user.email=t@e", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A job's changes leave one local environment and reach another, as
    /// a checkpoint carries them (#662, #663); each worktree goes with its
    /// environment.
    #[tokio::test]
    async fn changes_move_between_two_local_environments() {
        let root = std::env::temp_dir().join(format!(
            "aegis-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let clone = root.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        std::fs::write(clone.join("a.txt"), "one\n").unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["add", "-A"]);
        git(&clone, &["commit", "-qm", "base"]);
        let start = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&clone)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let start = start.trim();

        let first = LocalExecutionEnv::create(&clone, start, &root.join("jobs/one")).unwrap();
        assert!(
            first.root().ends_with("jobs/one/repo"),
            "{}",
            first.root().display()
        );
        assert_eq!(first.job_dir(), root.join("jobs/one"));
        std::fs::write(first.root().join("a.txt"), "two\n").unwrap();
        std::fs::write(first.root().join("b.txt"), "new\n").unwrap();
        let diff = first.export_diff(start).await.unwrap();
        assert!(diff.contains("b.txt"), "{diff}");
        let first_tree = first.root().to_owned();
        drop(first);
        assert!(
            !first_tree.exists(),
            "the worktree goes with its environment"
        );

        let second = LocalExecutionEnv::create(&clone, start, &root.join("jobs/two")).unwrap();
        second.apply_diff(&diff).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(second.root().join("a.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(
            std::fs::read_to_string(second.root().join("b.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            second.export_diff(start).await.unwrap(),
            diff,
            "the same changes"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(&root);
    }
}
