//! Services the tests need, such as PostgreSQL or Redis (#490, ADR 0013).
//!
//! A gate run that declares sidecars starts them on an internal Docker
//! network of its own: the containers reach each other by name, and
//! nothing on the network reaches the host or the Internet. The test
//! steps (`repro_test`, `test`, `coverage`) run on that network, or, for
//! a sidecar with `localhost = true`, in its network namespace, so a test
//! that connects to `localhost:5432` reaches it. Every other step stays
//! on `--network none`.
//!
//! Sidecars are hardened like the gate's own containers: no capabilities,
//! no new privileges, a non-root user, a read-only root, and their data on
//! tmpfs. Before every test command after the first they are stopped and
//! started again, which empties the tmpfs: each run (the patch, the base,
//! a rerun) starts from a fresh service.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::sandbox::CLIENT_ENV;
use crate::{GateError, Sandbox};

/// Container and network names start with this; `aegis-docker-proxy`
/// lets only them use sidecar networks, aliases and images.
pub const SIDECAR_PREFIX: &str = "aegis-sidecar-";

const DEFAULT_READY_TIMEOUT_S: u64 = 60;
const DEFAULT_MEMORY_MB: u64 = 1024;
const DEFAULT_CPUS: u32 = 2;
const PIDS: u32 = 512;
const TMPFS_MB: u64 = 512;
/// Between two readiness probes.
const PROBE_INTERVAL: Duration = Duration::from_millis(500);
/// How much of a sidecar's log an error quotes.
const LOG_TAIL_LINES: usize = 30;

fn default_ready_timeout() -> u64 {
    DEFAULT_READY_TIMEOUT_S
}

/// One service, as `[[repo.sidecars]]` declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarSpec {
    /// Its host name on the network (`postgres`), and in the names of
    /// its containers.
    pub name: String,
    /// Must be one the deployment's proxy allows (`--sidecar-image`).
    pub image: String,
    /// `uid:gid`, not root: the image's own service user (70:70 for
    /// `postgres:*-alpine`, 999:999 for `redis`).
    pub user: String,
    /// The service's environment (`POSTGRES_PASSWORD`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Directories it writes (its data, its socket), each on tmpfs owned
    /// by `user`. `/tmp` always is.
    #[serde(default)]
    pub tmpfs: Vec<String>,
    /// Exits 0 once the service accepts connections; runs in the image,
    /// in the sidecar's network namespace (`["pg_isready", "-h",
    /// "localhost"]`).
    pub ready: Vec<String>,
    #[serde(default = "default_ready_timeout")]
    pub ready_timeout_s: u64,
    /// The test steps run in its network namespace: the service is on
    /// their `localhost`. One sidecar of a repo at most.
    #[serde(default)]
    pub localhost: bool,
    /// Added to the environment of the test steps
    /// (`DATABASE_URL = "postgres://postgres:test@postgres:5432/test"`).
    #[serde(default)]
    pub test_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
}

/// A host name: lowercase letters, digits and `-`, a letter first.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.ends_with('-')
}

/// `uid:gid`, both decimal and neither 0, as the proxy requires.
fn is_user(user: &str) -> bool {
    let id = |s: &str| {
        !s.is_empty()
            && s.len() <= 10
            && s.bytes().all(|b| b.is_ascii_digit())
            && s.parse::<u32>().is_ok_and(|n| n != 0)
    };
    user.split_once(':').is_some_and(|(u, g)| id(u) && id(g))
}

fn is_env_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl SidecarSpec {
    pub fn check(&self) -> Result<(), String> {
        let name = &self.name;
        if !is_name(name) {
            return Err(format!(
                "sidecar name {name:?} must be a host name: lowercase letters, digits and `-`, \
                 a letter first, at most 32"
            ));
        }
        if self.image.trim().is_empty() || self.image.contains(char::is_whitespace) {
            return Err(format!("sidecar `{name}`: image {:?}", self.image));
        }
        if !is_user(&self.user) {
            return Err(format!(
                "sidecar `{name}`: user {:?} must be a non-root uid:gid",
                self.user
            ));
        }
        if self.ready.is_empty() || self.ready[0].is_empty() {
            return Err(format!("sidecar `{name}`: `ready` is required"));
        }
        if self.ready_timeout_s == 0 {
            return Err(format!("sidecar `{name}`: ready_timeout_s must be > 0"));
        }
        for dir in &self.tmpfs {
            let plain = dir.starts_with('/')
                && dir != "/"
                && !dir.split('/').any(|c| c == ".." || c == ".")
                && !dir.contains([',', ':']);
            if !plain {
                return Err(format!(
                    "sidecar `{name}`: tmpfs {dir:?} must be a plain absolute path"
                ));
            }
        }
        if let Some(key) = self
            .env
            .keys()
            .chain(self.test_env.keys())
            .find(|k| !is_env_key(k))
        {
            return Err(format!(
                "sidecar `{name}`: environment variable name {key:?}"
            ));
        }
        if self.memory_mb == Some(0) || self.cpus == Some(0) {
            return Err(format!("sidecar `{name}`: limits must be > 0"));
        }
        Ok(())
    }
}

/// Every sidecar is valid, names are unique, and one at most is on
/// `localhost`.
pub fn check_sidecars(specs: &[SidecarSpec]) -> Result<(), String> {
    for (i, spec) in specs.iter().enumerate() {
        spec.check()?;
        if specs[..i].iter().any(|s| s.name == spec.name) {
            return Err(format!("sidecar `{}` is declared twice", spec.name));
        }
    }
    if specs.iter().filter(|s| s.localhost).count() > 1 {
        return Err("only one sidecar may have `localhost = true`".into());
    }
    Ok(())
}

/// The sidecars of a gate run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecars {
    pub docker: String,
    pub specs: Vec<SidecarSpec>,
}

/// Running sidecars and their network; all removed on drop. The gate
/// holds one per run; the ACI's `run_tests` one per session (#491).
#[derive(Debug)]
pub struct SidecarSession {
    docker: String,
    network: String,
    /// Container name and spec, in the order declared.
    containers: Vec<(String, SidecarSpec)>,
    /// Probe containers made so far, for unique names.
    probes: AtomicU32,
}

fn client(docker: &str) -> Command {
    let mut c = Command::new(docker);
    c.env_clear()
        .envs(
            CLIENT_ENV
                .iter()
                .filter_map(|k| Some((*k, std::env::var_os(k)?))),
        )
        .stdin(Stdio::null());
    c
}

fn docker(docker: &str, args: &[&str]) -> Result<String, GateError> {
    let out = client(docker)
        .args(args)
        .output()
        .map_err(|e| GateError::Spawn {
            program: docker.to_owned(),
            message: e.to_string(),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(GateError::Spawn {
            program: format!("{docker} {}", args.first().copied().unwrap_or("")),
            message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

fn error(name: &str, message: String) -> GateError {
    GateError::Spawn {
        program: format!("sidecar `{name}`"),
        message,
    }
}

/// The flags of a sidecar or its probe: the gate's hardening (#320) with
/// the service's user, and its directories on tmpfs owned by that user.
fn confined(spec: &SidecarSpec, network: &str) -> Vec<String> {
    let memory = format!("{}m", spec.memory_mb.unwrap_or(DEFAULT_MEMORY_MB));
    let (uid, gid) = spec.user.split_once(':').unwrap_or(("", ""));
    let mut out: Vec<String> = [
        "--network",
        network,
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        &PIDS.to_string(),
        "--memory",
        &memory,
        "--memory-swap",
        &memory,
        "--cpus",
        &spec.cpus.unwrap_or(DEFAULT_CPUS).to_string(),
        "--user",
        &spec.user,
        "--read-only",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=64m",
    ]
    .map(str::to_owned)
    .to_vec();
    for dir in &spec.tmpfs {
        out.push("--tmpfs".into());
        out.push(format!(
            "{dir}:rw,nosuid,nodev,size={TMPFS_MB}m,uid={uid},gid={gid},mode=0700"
        ));
    }
    out
}

impl Sidecars {
    /// The arguments of `docker run` for the sidecar `container`.
    pub(crate) fn run_args(spec: &SidecarSpec, container: &str, network: &str) -> Vec<String> {
        let mut args = vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--name".to_owned(),
            container.to_owned(),
        ];
        args.extend(confined(spec, network));
        args.extend(["--network-alias".to_owned(), spec.name.clone()]);
        for (key, value) in &spec.env {
            args.extend(["--env".to_owned(), format!("{key}={value}")]);
        }
        args.push(spec.image.clone());
        args
    }

    /// Starts every sidecar on a new internal network and waits until
    /// each is ready. On an error, what was started is removed.
    pub fn start(&self) -> Result<SidecarSession, GateError> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let id = format!("{SIDECAR_PREFIX}{}-{n}", std::process::id());
        let mut session = SidecarSession {
            docker: self.docker.clone(),
            network: id.clone(),
            containers: vec![],
            probes: AtomicU32::new(0),
        };
        docker(&self.docker, &["network", "create", "--internal", &id])?;
        for spec in &self.specs {
            let container = format!("{id}-{}", spec.name);
            let args = Self::run_args(spec, &container, &id);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            // Kept even when `run` fails half-way: drop removes it.
            session.containers.push((container, spec.clone()));
            docker(&self.docker, &args).map_err(|e| error(&spec.name, e.to_string()))?;
        }
        session.wait_ready()?;
        Ok(session)
    }
}

impl SidecarSession {
    /// The sandbox of a test step: on the sidecars' network, or in the
    /// network namespace of the `localhost` one, with their `test_env`.
    pub fn sandbox_for(&self, sandbox: &Sandbox) -> Sandbox {
        let network = match self.containers.iter().find(|(_, s)| s.localhost) {
            Some((container, _)) => format!("container:{container}"),
            None => self.network.clone(),
        };
        let mut out = Sandbox {
            network: Some(network),
            ..sandbox.clone()
        };
        for (_, spec) in &self.containers {
            out.env
                .extend(spec.test_env.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        out
    }

    /// Stops and starts every sidecar, which empties its tmpfs, and waits
    /// until each is ready again.
    pub fn reset(&self) -> Result<(), GateError> {
        for (container, spec) in &self.containers {
            docker(&self.docker, &["stop", "--time", "5", container])
                .and_then(|_| docker(&self.docker, &["start", container]))
                .map_err(|e| error(&spec.name, e.to_string()))?;
        }
        self.wait_ready()
    }

    fn wait_ready(&self) -> Result<(), GateError> {
        for (container, spec) in &self.containers {
            self.wait_one(container, spec)?;
        }
        Ok(())
    }

    /// Runs `ready` in a probe container until it exits 0. A sidecar that
    /// stopped, or one not ready in `ready_timeout_s`, is an error with
    /// the end of its log.
    fn wait_one(&self, container: &str, spec: &SidecarSpec) -> Result<(), GateError> {
        let timeout = Duration::from_secs(spec.ready_timeout_s);
        let started = Instant::now();
        loop {
            let running = docker(
                &self.docker,
                &["inspect", "--format", "{{.State.Running}}", container],
            )
            .is_ok_and(|s| s == "true");
            if !running {
                return Err(error(
                    &spec.name,
                    format!(
                        "stopped before it was ready; its log:\n{}",
                        self.log(container)
                    ),
                ));
            }
            if self.probe(container, spec)? {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(error(
                    &spec.name,
                    format!(
                        "not ready after {}s (`{}`); its log:\n{}",
                        spec.ready_timeout_s,
                        spec.ready.join(" "),
                        self.log(container)
                    ),
                ));
            }
            std::thread::sleep(PROBE_INTERVAL);
        }
    }

    /// One run of `ready`. An error only when the probe container cannot
    /// be created at all (the proxy refused it, no such image).
    fn probe(&self, container: &str, spec: &SidecarSpec) -> Result<bool, GateError> {
        let n = self.probes.fetch_add(1, Ordering::Relaxed);
        let probe = format!("{container}-probe-{n}");
        let mut args = vec![
            "run".to_owned(),
            "--rm".to_owned(),
            "--name".to_owned(),
            probe,
        ];
        args.extend(confined(spec, &format!("container:{container}")));
        args.extend(["--entrypoint".to_owned(), spec.ready[0].clone()]);
        args.push(spec.image.clone());
        args.extend(spec.ready[1..].iter().cloned());
        let out = client(&self.docker)
            .args(&args)
            .output()
            .map_err(|e| error(&spec.name, format!("cannot start `{}`: {e}", self.docker)))?;
        match out.status.code() {
            Some(0) => Ok(true),
            // `docker run` itself failed: probing again will not help.
            Some(125) => Err(error(
                &spec.name,
                format!(
                    "its readiness probe did not start: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            )),
            _ => Ok(false),
        }
    }

    fn log(&self, container: &str) -> String {
        let tail = LOG_TAIL_LINES.to_string();
        let out = client(&self.docker)
            .args(["logs", "--tail", &tail, container])
            .output();
        match out {
            Ok(o) => format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
            .trim()
            .to_owned(),
            Err(e) => format!("(no log: {e})"),
        }
    }
}

impl Drop for SidecarSession {
    fn drop(&mut self) {
        for (container, _) in &self.containers {
            let _ = docker(&self.docker, &["rm", "--force", "--volumes", container]);
        }
        let _ = docker(&self.docker, &["network", "rm", &self.network]);
    }
}

#[cfg(test)]
mod tests;
