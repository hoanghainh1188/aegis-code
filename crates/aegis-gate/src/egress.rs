//! The fetch step's way out (#66, design §7): an internal Docker network
//! with nothing on it but the egress proxy (`docker/egress/egress.py`),
//! which lets through only HTTPS to the profile's registry hosts. The fetch
//! command runs there; the gate's own steps stay on `--network none` and
//! read what it fetched through read-only mounts.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::command::{CancelToken, Ran, Runner};
use crate::{GateError, Sandbox};

const PROXY_SCRIPT: &str = include_str!("../../../docker/egress/egress.py");
const PROXY_PORT: u16 = 3128;
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the fetch step may go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    pub docker: String,
    /// An image with Python 3 (the Python sandbox image).
    pub proxy_image: String,
    /// Exact host names, HTTPS only.
    pub allow: Vec<String>,
}

/// A running proxy and its network; both are removed on drop.
#[derive(Debug)]
pub struct EgressSession {
    docker: String,
    network: String,
    proxy: String,
}

fn docker(docker: &str, args: &[&str]) -> Result<String, GateError> {
    let out = Command::new(docker)
        .args(args)
        .stdin(Stdio::null())
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

impl Egress {
    /// Starts the proxy on its own internal network.
    pub fn start(&self) -> Result<EgressSession, GateError> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let session = EgressSession {
            docker: self.docker.clone(),
            network: format!("aegis-fetch-{}-{n}", std::process::id()),
            proxy: format!("aegis-egress-{}-{n}", std::process::id()),
        };
        docker(
            &self.docker,
            &["network", "create", "--internal", &session.network],
        )?;
        let allow = format!("AEGIS_ALLOW={}", self.allow.join(","));
        // The proxy reaches the outside on the default network and the
        // fetch container on the internal one.
        docker(
            &self.docker,
            &[
                "run",
                "-d",
                "--rm",
                "--name",
                &session.proxy,
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--env",
                &allow,
                &self.proxy_image,
                "python",
                "-c",
                PROXY_SCRIPT,
            ],
        )?;
        docker(
            &self.docker,
            &["network", "connect", &session.network, &session.proxy],
        )?;
        let started = Instant::now();
        while !docker(&self.docker, &["logs", &session.proxy])?.contains("listening") {
            if started.elapsed() > READY_TIMEOUT {
                return Err(GateError::Spawn {
                    program: "egress proxy".into(),
                    message: "did not start".into(),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(session)
    }
}

impl EgressSession {
    /// The proxy's decisions so far (`allow <host>`, `deny <host:port>`).
    pub fn log(&self) -> String {
        docker(&self.docker, &["logs", &self.proxy]).unwrap_or_default()
    }

    /// Runs `argv` in `sandbox`'s image on the internal network, with the
    /// caches mounted writable and the proxy in the environment.
    pub fn fetch(
        &self,
        sandbox: &Sandbox,
        argv: &[String],
        tree: &Path,
        log: &Path,
        timeout: Duration,
        cancel: CancelToken,
    ) -> Result<Ran, GateError> {
        let proxy = format!("http://{}:{PROXY_PORT}", self.proxy);
        let mut fetch = Sandbox {
            network: Some(self.network.clone()),
            read_only_mounts: false,
            ..sandbox.clone()
        };
        for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
            fetch.env.push((key.into(), proxy.clone()));
        }
        // The images default to offline; this container is the one that
        // may download.
        for (key, value) in [
            ("CARGO_NET_OFFLINE", "false"),
            ("UV_OFFLINE", "0"),
            ("PIP_NO_INDEX", "0"),
        ] {
            fetch.env.push((key.into(), value.into()));
        }
        let runner = Runner {
            env: vec![],
            timeout,
            cancel,
            sandbox: Some(fetch),
        };
        runner.run(argv, tree, log, &[])
    }
}

impl Drop for EgressSession {
    fn drop(&mut self) {
        let _ = docker(&self.docker, &["rm", "-f", &self.proxy]);
        let _ = docker(&self.docker, &["network", "rm", &self.network]);
    }
}
