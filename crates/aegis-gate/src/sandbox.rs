//! Gate steps in a container (design §6.1, §7; #5): each command runs in a
//! fresh container of the profile's image, with no network, no extra
//! capabilities, as the host user, and only the tree under check (`/work`)
//! and the profile's cache directories mounted. Nothing else of the host,
//! no `.env`, key or token, is visible. The patched and the base run never
//! share a container, so one cannot leave anything for the other except
//! through the build caches.
//!
//! Containers are driven through the `docker` CLI, not the Docker API
//! (ADR 0003 §6, #184): the gate runs off the async runtime, and a child
//! process gives it the same timeout, cancel and log handling as a step
//! run on the host.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::GateError;

/// Where the tree under check is mounted.
pub const WORKDIR: &str = "/work";

/// Variables the `docker` client itself may need from this process (to
/// find the daemon); none of them reach the container.
pub const CLIENT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    /// The `docker` (or compatible) program.
    pub docker: String,
    pub image: String,
    /// (host directory, path in the container): the profile's caches that
    /// live outside the tree (`/usr/local/cargo/registry`, uv's cache).
    pub mounts: Vec<(PathBuf, String)>,
    /// Set in the container (the profile's `sandbox.env`, the request's
    /// env); not passed to the `docker` client.
    pub env: Vec<(String, String)>,
    /// Docker network; `None` is `none`. Only the fetch step (#66) sets
    /// one, an internal network whose only way out is the egress proxy.
    pub network: Option<String>,
    /// Mount `mounts` read-only: gate steps read the caches the fetch step
    /// filled, they never change them (#66).
    pub read_only_mounts: bool,
}

#[cfg(unix)]
fn host_user() -> String {
    // SAFETY: getuid and getgid cannot fail and touch no memory.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    format!("{uid}:{gid}")
}

#[cfg(not(unix))]
fn host_user() -> String {
    "1000:1000".to_owned()
}

/// A name no other run uses, to kill the container on timeout or cancel.
pub(crate) fn container_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "aegis-gate-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

impl Sandbox {
    /// `docker run …` for `argv` with `dir` as `/work`. `pass` names
    /// variables the container takes from the `docker` client's own
    /// environment, so their values (secrets) never appear on its command
    /// line.
    pub(crate) fn wrap(
        &self,
        name: &str,
        argv: &[String],
        dir: &Path,
        pass: &[&str],
    ) -> Vec<String> {
        let mut out = vec![
            self.docker.clone(),
            "run".into(),
            "--rm".into(),
            "--name".into(),
            name.into(),
            "--network".into(),
            self.network.clone().unwrap_or_else(|| "none".into()),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--pids-limit".into(),
            "4096".into(),
            "--user".into(),
            host_user(),
            "--volume".into(),
            format!("{}:{WORKDIR}", dir.display()),
            "--workdir".into(),
            WORKDIR.into(),
        ];
        let mode = if self.read_only_mounts { ":ro" } else { "" };
        for (host, inside) in &self.mounts {
            out.push("--volume".into());
            out.push(format!("{}:{inside}{mode}", host.display()));
        }
        for (key, value) in &self.env {
            out.push("--env".into());
            out.push(format!("{key}={value}"));
        }
        for key in pass {
            out.push("--env".into());
            out.push((*key).to_owned());
        }
        out.push(self.image.clone());
        out.extend(argv.iter().cloned());
        out
    }

    /// The `docker` client with only [`CLIENT_ENV`] from this process.
    fn client(&self) -> Command {
        let mut c = Command::new(&self.docker);
        c.env_clear()
            .envs(
                CLIENT_ENV
                    .iter()
                    .filter_map(|k| Some((*k, std::env::var_os(k)?))),
            )
            .stdin(Stdio::null());
        c
    }

    /// Fails when the daemon cannot be reached, so a stopped daemon is an
    /// error of the gate and not every step failing with `docker run`'s
    /// exit code 125.
    pub fn check(&self) -> Result<(), GateError> {
        let out = self
            .client()
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .map_err(|e| GateError::Spawn {
                program: self.docker.clone(),
                message: e.to_string(),
            })?;
        if out.status.success() {
            return Ok(());
        }
        Err(GateError::Spawn {
            program: self.docker.clone(),
            message: format!(
                "the Docker daemon is not reachable: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        })
    }

    /// Stops the container now; `--rm` removes it. A container that is
    /// already gone is not an error: it ended on its own meanwhile. The
    /// error says why one may still be running.
    pub(crate) fn kill(&self, name: &str) -> Result<(), String> {
        let out = self
            .client()
            .args(["kill", name])
            .stdout(Stdio::null())
            .output()
            .map_err(|e| format!("cannot start `{}`: {e}", self.docker))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() || stderr.contains("No such container") {
            Ok(())
        } else {
            Err(stderr.trim().to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_without_network_capabilities_or_host_paths() {
        let sandbox = Sandbox {
            docker: "docker".into(),
            image: "aegis-sandbox-rust".into(),
            mounts: vec![(
                PathBuf::from("/state/cache/registry"),
                "/usr/local/cargo/registry".into(),
            )],
            env: vec![("GATE_X".into(), "1".into())],
            network: None,
            read_only_mounts: true,
        };
        let argv = sandbox.wrap(
            "n",
            &["cargo".into(), "test".into()],
            Path::new("/jobs/7/repo"),
            &["SONAR_TOKEN"],
        );
        let text = argv.join(" ");
        for expected in [
            "docker run --rm --name n --network none --cap-drop ALL --security-opt no-new-privileges",
            "--volume /jobs/7/repo:/work --workdir /work",
            "--volume /state/cache/registry:/usr/local/cargo/registry:ro",
            "--env GATE_X=1 --env SONAR_TOKEN aegis-sandbox-rust cargo test",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn the_fetch_container_joins_its_network_and_may_write_the_caches() {
        let sandbox = Sandbox {
            docker: "docker".into(),
            image: "aegis-sandbox-rust".into(),
            mounts: vec![(PathBuf::from("/c"), "/usr/local/cargo/registry".into())],
            env: vec![],
            network: Some("aegis-fetch-1-0".into()),
            read_only_mounts: false,
        };
        let text = sandbox
            .wrap("n", &["cargo".into(), "fetch".into()], Path::new("/r"), &[])
            .join(" ");
        assert!(text.contains("--network aegis-fetch-1-0 "), "{text}");
        assert!(
            text.contains("--volume /c:/usr/local/cargo/registry "),
            "{text}"
        );
        assert!(!text.contains(":ro"), "{text}");
    }

    #[test]
    fn limits_processes_and_runs_as_the_host_user() {
        let sandbox = fake_sandbox("docker");
        let argv = sandbox.wrap("n", &["true".into()], Path::new("/r"), &[]);
        let text = argv.join(" ");
        assert!(text.contains("--pids-limit 4096"), "{text}");
        assert!(text.contains(&format!("--user {}", host_user())), "{text}");
        assert!(!text.contains("--privileged"), "{text}");
        // Nothing of the host but the tree and the caches.
        let volumes = argv.iter().filter(|a| *a == "--volume").count();
        assert_eq!(volumes, 1 + sandbox.mounts.len(), "{text}");
    }

    #[test]
    fn secret_values_never_reach_the_command_line() {
        let sandbox = fake_sandbox("docker");
        let text = sandbox
            .wrap("n", &["true".into()], Path::new("/r"), &["SONAR_TOKEN"])
            .join(" ");
        assert!(text.contains("--env SONAR_TOKEN "), "{text}");
        assert!(!text.contains("SONAR_TOKEN="), "{text}");
    }

    fn fake_sandbox(docker: &str) -> Sandbox {
        Sandbox {
            docker: docker.into(),
            image: "img".into(),
            mounts: vec![],
            env: vec![],
            network: None,
            read_only_mounts: true,
        }
    }

    /// A `docker` that writes `stderr` and exits with `code`.
    #[cfg(unix)]
    fn fake_docker(name: &str, stderr: &str, code: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aegis-fake-docker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho '{stderr}' >&2\nexit {code}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_stopped_daemon_is_an_error_that_says_so() {
        let docker = fake_docker(
            "down",
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
            1,
        );
        let err = fake_sandbox(docker.to_str().unwrap()).check().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("daemon is not reachable"), "{text}");
        assert!(text.contains("Cannot connect"), "{text}");
        let up = fake_docker("up", "", 0);
        fake_sandbox(up.to_str().unwrap()).check().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_kill_that_fails_says_why_unless_the_container_is_gone() {
        let gone = fake_docker(
            "gone",
            "Error response from daemon: No such container: n",
            1,
        );
        fake_sandbox(gone.to_str().unwrap()).kill("n").unwrap();
        let down = fake_docker("killdown", "Cannot connect to the Docker daemon", 1);
        let why = fake_sandbox(down.to_str().unwrap()).kill("n").unwrap_err();
        assert!(why.contains("Cannot connect"), "{why}");
        let missing = fake_sandbox("/nonexistent/docker").kill("n").unwrap_err();
        assert!(missing.contains("cannot start"), "{missing}");
    }
}
