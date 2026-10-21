//! Gate steps in a container (design §6.1, §7; #5): each command runs in a
//! fresh container of the profile's image, with no network, no extra
//! capabilities, as the host user, and only the tree under check (`/work`)
//! and the profile's cache directories mounted. Nothing else of the host,
//! no `.env`, key or token, is visible. The patched and the base run never
//! share a container, so one cannot leave anything for the other except
//! through the build caches.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

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
    /// `docker run …` for `argv` with `dir` as `/work`.
    pub(crate) fn wrap(&self, name: &str, argv: &[String], dir: &Path) -> Vec<String> {
        let mut out = vec![
            self.docker.clone(),
            "run".into(),
            "--rm".into(),
            "--name".into(),
            name.into(),
            "--network".into(),
            "none".into(),
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
        for (host, inside) in &self.mounts {
            out.push("--volume".into());
            out.push(format!("{}:{inside}", host.display()));
        }
        for (key, value) in &self.env {
            out.push("--env".into());
            out.push(format!("{key}={value}"));
        }
        out.push(self.image.clone());
        out.extend(argv.iter().cloned());
        out
    }

    /// Stops the container now; `--rm` removes it.
    pub(crate) fn kill(&self, name: &str) {
        let _ = Command::new(&self.docker)
            .args(["kill", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
        };
        let argv = sandbox.wrap(
            "n",
            &["cargo".into(), "test".into()],
            Path::new("/jobs/7/repo"),
        );
        let text = argv.join(" ");
        for expected in [
            "docker run --rm --name n --network none --cap-drop ALL --security-opt no-new-privileges",
            "--volume /jobs/7/repo:/work --workdir /work",
            "--volume /state/cache/registry:/usr/local/cargo/registry",
            "--env GATE_X=1 aegis-sandbox-rust cargo test",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }
}
