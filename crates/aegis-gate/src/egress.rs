//! The fetch step's way out (#66, design §7): an internal Docker network
//! with nothing on it but the egress proxy (`docker/egress/egress.py`),
//! which lets through only HTTPS to the profile's registry hosts. The fetch
//! command runs there; the gate's own steps stay on `--network none` and
//! read what it fetched through read-only mounts.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::command::{CancelToken, Ran, Runner};
use crate::sandbox::{Hardening, Limits};
use crate::{GateError, Sandbox};

const PROXY_SCRIPT: &str = include_str!("../../../docker/egress/egress.py");
const PROXY_PORT: u16 = 3128;
/// What the proxy prints once it accepts connections.
const READY_LINE: &str = "listening";
/// How much of the proxy's log a startup error quotes.
const LOG_TAIL_CHARS: usize = 2000;
/// The proxy relays bytes and keeps nothing (#320).
const PROXY_LIMITS: Limits = Limits {
    memory_mb: 256,
    cpus: 1,
    pids: 256,
    tmp_mb: 16,
};

/// Where the fetch step may go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    pub docker: String,
    /// An image with Python 3 (the Python sandbox image).
    pub proxy_image: String,
    /// Exact host names, HTTPS only.
    pub allow: Vec<String>,
    /// How long the started proxy may take to listen (#326).
    pub ready_timeout: Duration,
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
    /// The default of [`Egress::ready_timeout`]. Under load the time goes
    /// to the Docker daemon, not to the proxy (#326): its calls took
    /// minutes while the proxy listened 50 ms after it started.
    pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(120);

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
        // fetch container on the internal one. It only relays bytes.
        let hardening = Hardening {
            limits: PROXY_LIMITS,
            ..Hardening::default()
        };
        // No `--rm`: a proxy that exits keeps its log for the error. The
        // session removes it.
        let mut args = vec!["run", "-d", "--name", &session.proxy];
        let confined = hardening.args("bridge");
        args.extend(confined.iter().map(String::as_str));
        args.extend([
            "--env",
            &allow,
            &self.proxy_image,
            "python",
            "-c",
            PROXY_SCRIPT,
        ]);
        docker(&self.docker, &args)?;
        docker(
            &self.docker,
            &["network", "connect", &session.network, &session.proxy],
        )?;
        wait_ready(&self.docker, &session.proxy, self.ready_timeout)?;
        Ok(session)
    }
}

/// Waits until `proxy` logs [`READY_LINE`] (#326). One `docker logs -f`
/// follows the log: polling ran a `docker` every 100 ms, each one more call
/// to a daemon that is already the slow part. The follow ends when the
/// container does, so a proxy that exited fails at once and only a slow
/// one is waited for.
fn wait_ready(docker: &str, proxy: &str, timeout: Duration) -> Result<(), GateError> {
    let fail = |message: String| GateError::Spawn {
        program: "egress proxy".into(),
        message,
    };
    let mut follow = Command::new(docker)
        .args(["logs", "-f", proxy])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(format!("`{docker} logs -f`: {e}")))?;
    let (Some(stdout), Some(mut stderr)) = (follow.stdout.take(), follow.stderr.take()) else {
        unreachable!("both are piped");
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = String::new();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let ready = line.contains(READY_LINE);
            seen.push_str(&line);
            seen.push('\n');
            if ready {
                let _ = tx.send((true, seen));
                return;
            }
        }
        let _ = tx.send((false, seen));
    });
    // The proxy's own errors (a traceback) are its stderr.
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let outcome = rx.recv_timeout(timeout);
    let _ = follow.kill();
    let _ = follow.wait();
    let errors = errors.join().unwrap_or_default();
    let log = |seen: &str| last_chars(&format!("{seen}{errors}"), LOG_TAIL_CHARS);
    match outcome {
        Ok((true, _)) => Ok(()),
        Ok((false, seen)) => Err(fail(format!(
            "exited before listening; its log:\n{}",
            log(&seen)
        ))),
        Err(_) => {
            // Killing the follow ended its output; what it read is sent.
            let seen = rx
                .recv_timeout(Duration::from_secs(5))
                .map(|(_, seen)| seen)
                .unwrap_or_default();
            Err(fail(format!(
                "not listening after {timeout:?}; its log so far:\n{}",
                log(&seen)
            )))
        }
    }
}

/// The last `n` characters of `text`, trimmed.
fn last_chars(text: &str, n: usize) -> String {
    let text = text.trim();
    let skip = text.chars().count().saturating_sub(n);
    text.chars().skip(skip).collect()
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    /// A `docker` whose `logs -f` runs `script`.
    fn fake_docker(name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aegis-fake-egress-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\n[ \"$1\" = probe ] && exit 0\n{script}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test's fork may still hold the write end until it execs
        // (ETXTBSY on Linux); wait until the script runs.
        for _ in 0..100 {
            match Command::new(&path).arg("probe").output() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => break,
            }
        }
        path
    }

    fn wait(name: &str, script: &str, timeout: Duration) -> (Result<(), GateError>, Duration) {
        let docker = fake_docker(name, script);
        let started = Instant::now();
        let result = wait_ready(docker.to_str().unwrap(), "p", timeout);
        (result, started.elapsed())
    }

    // `exec sleep`: the follow is one process, as `docker logs -f` is, so
    // killing it closes its output.

    #[test]
    fn a_listening_proxy_is_ready_without_waiting_for_the_timeout() {
        let (result, took) = wait(
            "ready",
            "echo starting; echo 'listening 3128 allow: pypi.org'; exec sleep 30",
            Duration::from_secs(20),
        );
        result.unwrap();
        assert!(took < Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn a_proxy_that_exits_fails_at_once_with_its_log() {
        let (result, took) = wait(
            "exits",
            "echo starting; echo 'OSError: address in use' >&2; exit 1",
            Duration::from_secs(20),
        );
        let message = result.unwrap_err().to_string();
        assert!(message.contains("exited before listening"), "{message}");
        assert!(message.contains("starting"), "{message}");
        assert!(message.contains("address in use"), "{message}");
        assert!(took < Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn a_slow_proxy_fails_after_the_timeout_with_what_it_logged() {
        let (result, took) = wait(
            "slow",
            "echo starting; exec sleep 30",
            Duration::from_millis(500),
        );
        let message = result.unwrap_err().to_string();
        assert!(message.contains("not listening after 500ms"), "{message}");
        assert!(message.contains("starting"), "{message}");
        assert!(took < Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn an_error_quotes_only_the_end_of_a_long_log() {
        let long = format!("{}end", "x".repeat(3 * LOG_TAIL_CHARS));
        let tail = last_chars(&long, LOG_TAIL_CHARS);
        assert_eq!(tail.chars().count(), LOG_TAIL_CHARS);
        assert!(tail.ends_with("end"));
    }
}
