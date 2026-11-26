//! Where a job's code is and runs (#663): the boundary between the
//! lifecycle (the agent loop, the router, the ledger) and the job's
//! workspace. The loop reaches the workspace through [`ExecutionEnv`]
//! only: the agent's tools (the ACI and codebase-memory MCP servers), the
//! gate, and the tree's changes for checkpoints (#662). Today the one
//! environment is [`LocalExecutionEnv`], a worktree of the platform's
//! clone on this host with its MCP servers over stdio and the gate on the
//! local Docker daemon, as before; [`RemoteExecutionEnv`] (#664) keeps
//! the tree here and runs the repo's containers (the gate's steps,
//! `run_tests`, the formatter, sidecars) on a remote execution host's
//! Docker, through its `aegis-docker-proxy --listen-tls` with mutual TLS.

use std::future::Future;
use std::path::{Path, PathBuf};

use aegis_gate::GateReport;

use super::GatePolicy;
use aegis_lang::RepoProfiles;
use aegis_orchestrator::McpServerSpec;
use tokio_util::sync::CancellationToken;

use super::pool::{self, PodJob};
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
        policy: &GatePolicy,
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

    /// The execution pod the job has from the pool (#665), kept in its
    /// checkpoint so that the job's next worker asks for it again.
    fn pod(&self) -> Option<&str> {
        None
    }
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
        policy: &GatePolicy,
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

/// The Docker client of a remote execution host (#664): a script in the
/// job's directory that runs `docker` against the host's proxy with this
/// worker's certificate. The gate and the ACI take it as their `docker`
/// program, so every container of the job runs there.
fn remote_docker(
    job_dir: &Path,
    docker: &str,
    remote: &crate::config::RemoteExecution,
) -> Result<PathBuf, LifecycleError> {
    remote.check().map_err(LifecycleError::Config)?;
    if docker.contains(['\'', '\n', '\0']) {
        return Err(LifecycleError::Config(format!(
            "run.docker `{docker}` cannot be quoted for the remote execution host"
        )));
    }
    let quote = |p: &Path| format!("'{}'", p.display());
    let script = format!(
        "#!/bin/sh\n# The remote execution host's Docker (#664).\nexec '{docker}' --host '{}' \\\n  --tlsverify --tlscacert {} --tlscert {} --tlskey {} \"$@\"\n",
        remote.docker_host,
        quote(&remote.tls_ca),
        quote(&remote.tls_cert),
        quote(&remote.tls_key),
    );
    std::fs::create_dir_all(job_dir).map_err(|e| LifecycleError::io(job_dir, &e))?;
    let path = job_dir.join("docker-remote");
    std::fs::write(&path, script).map_err(|e| LifecycleError::io(&path, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| LifecycleError::io(&path, &e))?;
    }
    Ok(path)
}

/// The job's tree on this host, its containers on a remote execution
/// host (#664). The host sees the tree by the same path (the run's
/// `state_dir` shared with it); [`ExecutionEnv::prepare_parts`] checks
/// that it does before anything runs there.
#[derive(Debug)]
pub(crate) struct RemoteExecutionEnv {
    local: LocalExecutionEnv,
    docker: String,
    /// The pool's pod this host is (#665), if it is one.
    pod: Option<String>,
}

/// What [`RemoteExecutionEnv`] writes for the host to read back.
const PROBE: &str = "remote-probe";

impl RemoteExecutionEnv {
    pub(crate) fn create(
        clone: &Path,
        start: &str,
        job_dir: &Path,
        docker: &str,
        remote: &crate::config::RemoteExecution,
    ) -> Result<Self, LifecycleError> {
        let local = LocalExecutionEnv::create(clone, start, job_dir)?;
        let docker = remote_docker(job_dir, docker, remote)?
            .display()
            .to_string();
        Ok(Self {
            local,
            docker,
            pod: None,
        })
    }

    /// `cfg` with the host's Docker in place of this host's.
    fn remote(&self, cfg: &RunConfig) -> RunConfig {
        let mut cfg = cfg.clone();
        cfg.run.docker = self.docker.clone();
        cfg
    }

    /// Whether the host reads back, by this job's path, what this worker
    /// wrote there: a container of the root part's image on the host's
    /// Docker reads a fresh nonce from the job's directory.
    async fn check_shared(&self, cfg: &RunConfig, profiles: &RepoProfiles) -> Result<(), String> {
        let Some(sandbox) = super::sandbox(cfg, &profiles.parts()[0]).map_err(|e| e.to_string())?
        else {
            return Err("remote execution needs the sandbox".into());
        };
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let job_dir = self.job_dir().to_owned();
        std::fs::write(job_dir.join(PROBE), &nonce).map_err(|e| e.to_string())?;
        let read = blocking("checking the execution host", move || {
            sandbox
                .read_mounted(&job_dir, PROBE)
                .map_err(|e| LifecycleError::Config(e.to_string()))
        })
        .await
        .map_err(|e| e.to_string())?;
        if read.trim() == nonce {
            Ok(())
        } else {
            Err(format!(
                "it reads `{}` where this worker wrote `{nonce}`",
                read.trim()
            ))
        }
    }
}

impl ExecutionEnv for RemoteExecutionEnv {
    fn root(&self) -> &Path {
        self.local.root()
    }

    fn job_dir(&self) -> &Path {
        self.local.job_dir()
    }

    fn tool_servers(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
    ) -> Result<Vec<McpServerSpec>, LifecycleError> {
        self.local.tool_servers(&self.remote(cfg), profiles)
    }

    async fn prepare_parts(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, LifecycleError> {
        let cfg = self.remote(cfg);
        if let Err(why) = self.check_shared(&cfg, profiles).await {
            return Ok(Some(format!(
                "the remote execution host does not see this job's files ({why}): it must mount \
                 run.state_dir at the same path"
            )));
        }
        self.local.prepare_parts(&cfg, profiles, cancel).await
    }

    async fn run_gate(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        base: &str,
        answer: &FinalAnswer,
        policy: &GatePolicy,
        cancel: &CancellationToken,
    ) -> Result<GateReport, LifecycleError> {
        self.local
            .run_gate(&self.remote(cfg), profiles, base, answer, policy, cancel)
            .await
    }

    async fn export_diff(&self, start: &str) -> Result<String, LifecycleError> {
        self.local.export_diff(start).await
    }

    async fn apply_diff(&self, diff: &str) -> Result<(), LifecycleError> {
        self.local.apply_diff(diff).await
    }

    fn pod(&self) -> Option<&str> {
        self.pod.as_deref()
    }
}

/// The job's environment, as its run's configuration picks it.
#[derive(Debug)]
pub(crate) enum JobEnv {
    Local(LocalExecutionEnv),
    Remote(RemoteExecutionEnv),
}

impl JobEnv {
    /// A remote one when the run names `[run.remote_execution]`, or a pod
    /// of the job's own from `[run.execution_pool]` (#665): `previous`,
    /// the pod the job had before its worker failed.
    pub(crate) async fn create(
        cfg: &RunConfig,
        start: &str,
        job_dir: &Path,
        job_id: &str,
        previous: Option<&str>,
    ) -> Result<Self, LifecycleError> {
        if let Some(pool) = &cfg.run.execution_pool {
            std::fs::create_dir_all(job_dir).map_err(|e| LifecycleError::io(job_dir, &e))?;
            let job = PodJob {
                id: job_id,
                dir: job_dir,
                state_dir: &cfg.run.state_dir,
            };
            let pod = pool::provision(pool, job, previous)
                .await
                .map_err(LifecycleError::Config)?;
            tracing::info!(job = job_id, pod = %pod.name, host = %pod.remote.docker_host, "the job has its execution pod");
            let created = RemoteExecutionEnv::create(
                &cfg.repo.path,
                start,
                job_dir,
                &cfg.run.docker,
                &pod.remote,
            );
            return match created {
                Ok(env) => Ok(Self::Remote(RemoteExecutionEnv {
                    pod: Some(pod.name),
                    ..env
                })),
                Err(e) => {
                    pool::release(pool, job, &pod.name).await;
                    Err(e)
                }
            };
        }
        Ok(match &cfg.run.remote_execution {
            Some(remote) => Self::Remote(RemoteExecutionEnv::create(
                &cfg.repo.path,
                start,
                job_dir,
                &cfg.run.docker,
                remote,
            )?),
            None => Self::Local(LocalExecutionEnv::create(&cfg.repo.path, start, job_dir)?),
        })
    }

    /// Ends the job's environment: its worktree goes, and its pod, if it
    /// has one, goes back to the pool.
    pub(crate) async fn release(self, cfg: &RunConfig, job_id: &str) {
        let pod = self.pod().map(str::to_owned);
        let job_dir = self.job_dir().to_owned();
        drop(self);
        if let (Some(pool), Some(pod)) = (&cfg.run.execution_pool, pod) {
            let job = PodJob {
                id: job_id,
                dir: &job_dir,
                state_dir: &cfg.run.state_dir,
            };
            pool::release(pool, job, &pod).await;
        }
    }
}

impl ExecutionEnv for JobEnv {
    fn root(&self) -> &Path {
        match self {
            Self::Local(e) => e.root(),
            Self::Remote(e) => e.root(),
        }
    }

    fn job_dir(&self) -> &Path {
        match self {
            Self::Local(e) => e.job_dir(),
            Self::Remote(e) => e.job_dir(),
        }
    }

    fn tool_servers(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
    ) -> Result<Vec<McpServerSpec>, LifecycleError> {
        match self {
            Self::Local(e) => e.tool_servers(cfg, profiles),
            Self::Remote(e) => e.tool_servers(cfg, profiles),
        }
    }

    async fn prepare_parts(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, LifecycleError> {
        match self {
            Self::Local(e) => e.prepare_parts(cfg, profiles, cancel).await,
            Self::Remote(e) => e.prepare_parts(cfg, profiles, cancel).await,
        }
    }

    async fn run_gate(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        base: &str,
        answer: &FinalAnswer,
        policy: &GatePolicy,
        cancel: &CancellationToken,
    ) -> Result<GateReport, LifecycleError> {
        match self {
            Self::Local(e) => {
                e.run_gate(cfg, profiles, base, answer, policy, cancel)
                    .await
            }
            Self::Remote(e) => {
                e.run_gate(cfg, profiles, base, answer, policy, cancel)
                    .await
            }
        }
    }

    async fn export_diff(&self, start: &str) -> Result<String, LifecycleError> {
        match self {
            Self::Local(e) => e.export_diff(start).await,
            Self::Remote(e) => e.export_diff(start).await,
        }
    }

    async fn apply_diff(&self, diff: &str) -> Result<(), LifecycleError> {
        match self {
            Self::Local(e) => e.apply_diff(diff).await,
            Self::Remote(e) => e.apply_diff(diff).await,
        }
    }

    fn pod(&self) -> Option<&str> {
        match self {
            Self::Local(e) => e.pod(),
            Self::Remote(e) => e.pod(),
        }
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

    /// #664: the remote host's `docker` passes every argument through,
    /// after the host and the worker's certificate.
    #[test]
    fn the_remote_docker_adds_the_host_and_the_certificate() {
        let dir = std::env::temp_dir().join(format!("aegis-remote-docker-{}", std::process::id()));
        let echo = dir.join("echo-docker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&echo, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&echo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let remote = crate::config::RemoteExecution {
            docker_host: "tcp://exec.internal:2376".into(),
            tls_cert: "/etc/aegis/worker.crt".into(),
            tls_key: "/etc/aegis/worker key.pem".into(),
            tls_ca: "/etc/aegis/exec-ca.crt".into(),
        };
        let script = remote_docker(&dir, &echo.display().to_string(), &remote).unwrap();
        let mode = std::fs::metadata(&script).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let out = Command::new(&script)
            .args(["run", "--rm", "an arg with spaces"])
            .output()
            .unwrap();
        let args: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            args,
            [
                "--host",
                "tcp://exec.internal:2376",
                "--tlsverify",
                "--tlscacert",
                "/etc/aegis/exec-ca.crt",
                "--tlscert",
                "/etc/aegis/worker.crt",
                "--tlskey",
                "/etc/aegis/worker key.pem",
                "run",
                "--rm",
                "an arg with spaces",
            ]
        );
        assert!(remote_docker(&dir, "dock'er", &remote).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_remote_host_is_a_tcp_port_and_its_files_absolute_paths() {
        let ok = crate::config::RemoteExecution {
            docker_host: "tcp://exec.internal:2376".into(),
            tls_cert: "/c.pem".into(),
            tls_key: "/k.pem".into(),
            tls_ca: "/ca.pem".into(),
        };
        assert!(ok.check().is_ok());
        for host in [
            "exec.internal:2376",
            "tcp://exec.internal",
            "tcp://",
            "tcp://a b:1",
            "unix:///s",
        ] {
            let bad = crate::config::RemoteExecution {
                docker_host: host.into(),
                ..ok.clone()
            };
            assert!(bad.check().is_err(), "{host}");
        }
        for path in ["relative.pem", "/it's.pem"] {
            let bad = crate::config::RemoteExecution {
                tls_key: path.into(),
                ..ok.clone()
            };
            assert!(bad.check().is_err(), "{path}");
        }
    }
}
