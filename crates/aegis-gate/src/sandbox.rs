//! Gate steps in a container (design §6.1, §7; #5): each command runs in a
//! fresh container of the profile's image, with no network, no extra
//! capabilities, as the host user, and only the tree under check (`/work`)
//! and the profile's cache directories mounted. Nothing else of the host,
//! no `.env`, key or token, is visible. The patched and the base run never
//! share a container, so one cannot leave anything for the other except
//! through the build caches.
//!
//! Every container also has bounded memory, CPU and processes, and an
//! image it cannot write to except at the profile's `sandbox.writable`
//! directories and a small `/tmp` (#320): code of the patch may be hostile,
//! and one container must not take the machine, and the other jobs on it,
//! down. [`Hardening::args`] is the one place these flags are written; the
//! ACI's formatter and language server containers use it too.
//!
//! Containers are driven through the `docker` CLI, not the Docker API
//! (ADR 0003 §6, #184): the gate runs off the async runtime, and a child
//! process gives it the same timeout, cancel and log handling as a step
//! run on the host.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Deserialize;

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

/// What one sandbox container may use (#320), the run's `[run.sandbox_limits]`.
/// `/tmp` is memory: its size counts against `memory_mb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// RAM, with no swap on top.
    pub memory_mb: u64,
    pub cpus: u32,
    pub pids: u32,
    /// Size of the container's `/tmp`.
    pub tmp_mb: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_mb: 8192,
            cpus: 4,
            pids: 4096,
            tmp_mb: 1024,
        }
    }
}

/// How a container is locked down: the run's [`Limits`] with the profile's
/// own (`sandbox.memory_mb`, `sandbox.cpus`), and where the image may be
/// written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardening {
    pub limits: Limits,
    /// Directories of the image the tools write to (`HOME`, a toolchain's
    /// lock files): each gets a volume of its own, filled from the image
    /// and removed with the container.
    pub writable: Vec<String>,
    /// Mount the image read-only. Off only for the Sonar scanner, whose
    /// image is not one of ours and which runs no code of the patch.
    pub read_only_rootfs: bool,
}

impl Default for Hardening {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            writable: Vec::new(),
            read_only_rootfs: true,
        }
    }
}

impl Hardening {
    /// `limits` with what `profile` sets in their place.
    pub fn new(limits: Limits, profile: &aegis_lang::Sandbox) -> Self {
        Self {
            limits: Limits {
                memory_mb: profile.memory_mb.unwrap_or(limits.memory_mb),
                cpus: profile.cpus.unwrap_or(limits.cpus),
                ..limits
            },
            writable: profile.writable.clone(),
            read_only_rootfs: true,
        }
    }

    /// The flags of `docker run` that confine the container, on `network`
    /// (`none` but for the fetch step and the Sonar scanner), as the host
    /// user.
    pub fn args(&self, network: &str) -> Vec<String> {
        let l = &self.limits;
        let memory = format!("{}m", l.memory_mb);
        let mut out: Vec<String> = [
            "--network",
            network,
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            &l.pids.to_string(),
            "--memory",
            &memory,
            // Equal to `--memory`: no swap on top.
            "--memory-swap",
            &memory,
            "--cpus",
            &l.cpus.to_string(),
            "--user",
            &host_user(),
        ]
        .map(str::to_owned)
        .to_vec();
        if self.read_only_rootfs {
            out.extend([
                "--read-only".to_owned(),
                "--tmpfs".to_owned(),
                // `exec`: Docker's tmpfs is `noexec` by default, and test
                // binaries (`go test`) and native libraries (Maven's jansi)
                // run from /tmp.
                format!("/tmp:rw,exec,nosuid,nodev,size={}m", l.tmp_mb),
            ]);
            for dir in &self.writable {
                out.extend([
                    "--mount".to_owned(),
                    format!("type=volume,destination={dir}"),
                ]);
            }
        }
        out
    }
}

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
    pub hardening: Hardening,
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
    /// line. No `--rm`: whether the container ran out of memory is read
    /// after it ends ([`Self::oom_killed`]), then [`Self::remove`] removes it.
    pub(crate) fn wrap(
        &self,
        name: &str,
        argv: &[String],
        dir: &Path,
        pass: &[&str],
    ) -> Vec<String> {
        let network = self.network.as_deref().unwrap_or("none");
        let mut out = vec![
            self.docker.clone(),
            "run".into(),
            "--name".into(),
            name.into(),
        ];
        out.extend(self.hardening.args(network));
        out.extend([
            "--volume".into(),
            format!("{}:{WORKDIR}", dir.display()),
            "--workdir".into(),
            WORKDIR.into(),
        ]);
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

    /// Whether the kernel killed a process of the container for going over
    /// its memory: the step's own exit code does not say so when the
    /// process killed is a child (`rustc` under `cargo` exits 101). False
    /// when the container cannot be inspected.
    pub(crate) fn oom_killed(&self, name: &str) -> bool {
        self.client()
            .args(["inspect", "--format", "{{.State.OOMKilled}}", name])
            .output()
            .is_ok_and(|o| o.status.success() && o.stdout.trim_ascii() == b"true")
    }

    /// Stops the container if it still runs, and removes it with its
    /// volumes (the `writable` directories). A container that is already
    /// gone is not an error. The error says why one may still be there.
    pub(crate) fn remove(&self, name: &str) -> Result<(), String> {
        let out = self
            .client()
            .args(["rm", "--force", "--volumes", name])
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
            hardening: Hardening::default(),
        };
        let argv = sandbox.wrap(
            "n",
            &["cargo".into(), "test".into()],
            Path::new("/jobs/7/repo"),
            &["SONAR_TOKEN"],
        );
        let text = argv.join(" ");
        for expected in [
            "docker run --name n --network none --cap-drop ALL --security-opt no-new-privileges",
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
            hardening: Hardening::default(),
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
    fn bounds_memory_cpu_and_processes_with_no_swap() {
        let text = fake_sandbox("docker")
            .wrap("n", &["true".into()], Path::new("/r"), &[])
            .join(" ");
        assert!(
            text.contains("--memory 8192m --memory-swap 8192m --cpus 4"),
            "{text}"
        );
        // Removed after its memory is read (`oom_killed`), not on exit.
        assert!(!text.contains("--rm"), "{text}");
    }

    #[test]
    fn the_image_is_read_only_but_tmp_and_the_profiles_writable_dirs() {
        let mut sandbox = fake_sandbox("docker");
        sandbox.hardening.writable = vec!["/home/aegis".into(), "/usr/local/cargo".into()];
        let text = sandbox
            .wrap("n", &["true".into()], Path::new("/r"), &[])
            .join(" ");
        for expected in [
            "--read-only --tmpfs /tmp:rw,exec,nosuid,nodev,size=1024m",
            "--mount type=volume,destination=/home/aegis",
            "--mount type=volume,destination=/usr/local/cargo",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        // The flags come before the image: none of them reaches the command.
        let image = text.find(" img ").unwrap();
        assert!(text.rfind("--").unwrap() < image, "{text}");
    }

    #[test]
    fn a_profile_sets_its_own_memory_and_cpus_over_the_runs() {
        let profile = aegis_lang::Sandbox {
            image: "img".into(),
            cache_dirs: vec![],
            writable: vec!["/home/aegis".into()],
            memory_mb: Some(12_288),
            cpus: None,
        };
        let run = Limits {
            memory_mb: 4096,
            cpus: 2,
            pids: 512,
            tmp_mb: 256,
        };
        let h = Hardening::new(run, &profile);
        assert_eq!(
            h.limits,
            Limits {
                memory_mb: 12_288,
                ..run
            }
        );
        assert_eq!(h.writable, ["/home/aegis"]);
        let text = h.args("none").join(" ");
        assert!(text.contains("--pids-limit 512"), "{text}");
        assert!(text.contains("size=256m"), "{text}");
    }

    #[test]
    fn a_writable_image_has_no_read_only_flags() {
        let h = Hardening {
            writable: vec!["/home/aegis".into()],
            read_only_rootfs: false,
            ..Hardening::default()
        };
        let text = h.args("sonar").join(" ");
        assert!(!text.contains("--read-only"), "{text}");
        assert!(!text.contains("--tmpfs"), "{text}");
        assert!(!text.contains("--mount"), "{text}");
        // Still bounded and confined.
        assert!(text.contains("--network sonar --cap-drop ALL"), "{text}");
        assert!(text.contains("--memory 8192m"), "{text}");
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
            hardening: Hardening::default(),
        }
    }

    /// A `docker` that writes `stderr` and exits with `code`.
    #[cfg(unix)]
    fn fake_docker(name: &str, stderr: &str, code: i32) -> PathBuf {
        fake_docker_out(name, "", stderr, code)
    }

    /// A `docker` that writes `stdout` and `stderr` and exits with `code`.
    #[cfg(unix)]
    fn fake_docker_out(name: &str, stdout: &str, stderr: &str, code: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aegis-fake-docker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '{stdout}'\necho '{stderr}' >&2\nexit {code}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test's fork may still hold the write end until it execs
        // (ETXTBSY on Linux); wait until the script runs.
        for _ in 0..100 {
            match Command::new(&path).output() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                _ => break,
            }
        }
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
    fn a_remove_that_fails_says_why_unless_the_container_is_gone() {
        let gone = fake_docker(
            "gone",
            "Error response from daemon: No such container: n",
            1,
        );
        fake_sandbox(gone.to_str().unwrap()).remove("n").unwrap();
        let down = fake_docker("rmdown", "Cannot connect to the Docker daemon", 1);
        let why = fake_sandbox(down.to_str().unwrap())
            .remove("n")
            .unwrap_err();
        assert!(why.contains("Cannot connect"), "{why}");
        let missing = fake_sandbox("/nonexistent/docker").remove("n").unwrap_err();
        assert!(missing.contains("cannot start"), "{missing}");
    }

    #[cfg(unix)]
    #[test]
    fn out_of_memory_is_what_docker_says_of_the_container() {
        let oom = fake_docker_out("oom", "true\\n", "", 0);
        assert!(fake_sandbox(oom.to_str().unwrap()).oom_killed("n"));
        let fine = fake_docker_out("fine", "false\\n", "", 0);
        assert!(!fake_sandbox(fine.to_str().unwrap()).oom_killed("n"));
        // Cannot tell: not out of memory.
        let gone = fake_docker("inspectgone", "No such container: n", 1);
        assert!(!fake_sandbox(gone.to_str().unwrap()).oom_killed("n"));
    }
}
