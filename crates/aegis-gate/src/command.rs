//! Runs one gate command: argv from the profile (no shell), a clean
//! environment, a time limit, and a cancel signal. Output goes to a log
//! file; the gate reads it back to summarize. On Unix the command gets its
//! own process group, so a timeout or cancel also stops what it started
//! (test binaries under `cargo test`, pytest workers).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::GateError;
use crate::sandbox::{CLIENT_ENV, Sandbox, container_name};

/// Variables a step gets from this process. Everything else, including
/// provider credentials, stays out; the caller adds what the repo needs.
pub const INHERITED_ENV: &[&str] = &["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL"];

/// After the command exits, how long to wait for its output pipes to close
/// (a daemon it started may hold them open).
const PIPE_GRACE: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(50);

/// Output kept per command; the rest is read and dropped.
pub(crate) const MAX_LOG_BYTES: u64 = 32 * 1024 * 1024;

/// Set from outside to stop the gate; checked between steps and while a
/// command runs.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// Killed by a signal.
    Signal,
    TimedOut,
    Cancelled,
    /// A process of the container went over its memory and the kernel
    /// killed it (#320); the limit, in MB.
    OutOfMemory {
        limit_mb: u64,
    },
}

impl Exit {
    pub fn success(self) -> bool {
        self == Self::Code(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub exit: Exit,
    pub duration: Duration,
    pub log: PathBuf,
}

/// What every command of one gate run shares.
#[derive(Debug, Clone)]
pub struct Runner {
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    pub cancel: CancelToken,
    /// Run each command in a container instead of on the machine.
    pub sandbox: Option<Sandbox>,
}

fn spawn_copy(
    stream: Option<impl Read + Send + 'static>,
    log: Arc<Mutex<File>>,
) -> mpsc::Receiver<()> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        if let Some(mut s) = stream {
            let mut buf = [0u8; 8192];
            let mut written = 0u64;
            while let Ok(n) = s.read(&mut buf) {
                if n == 0 {
                    break;
                }
                // Past the cap the output is drained, not kept.
                if written < MAX_LOG_BYTES {
                    if let Ok(mut f) = log.lock() {
                        let _ = f.write_all(&buf[..n]);
                    }
                    written += n as u64;
                }
            }
        }
        let _ = done.send(());
    });
    finished
}

#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_group(_: &mut Command) {}

/// Kills what is left of the command's process group (daemons, test
/// workers). The command itself has its own group, see [`own_group`].
fn kill_group(child: &Child) {
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg only sends a signal; `pgid` is the group the
        // command was started in, which holds no process of the gate.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

fn kill(child: &mut Child) {
    kill_group(child);
    let _ = child.kill();
    let _ = child.wait();
}

/// What the agent and the log are told when the kernel killed a process
/// of the container for going over its memory, so the agent can tell the
/// limit from a bug of its patch.
pub fn out_of_memory(limit_mb: u64) -> String {
    format!(
        "out of memory: a process went over the container's limit of {limit_mb} MB and \
         was killed"
    )
}

fn exit_of(status: ExitStatus) -> Exit {
    status.code().map_or(Exit::Signal, Exit::Code)
}

impl Runner {
    /// Runs `argv` in `dir` with `extra` added to the environment, writing
    /// its output to `log`.
    pub fn run(
        &self,
        argv: &[String],
        dir: &Path,
        log: &Path,
        extra: &[(&str, &str)],
    ) -> Result<Ran, GateError> {
        self.run_with_secrets(argv, dir, log, extra, &[])
    }

    /// [`Self::run`], with `secrets` added to this command's environment
    /// only. In a container they pass by name through the `docker`
    /// client's environment, never on a command line or in the log.
    pub(crate) fn run_with_secrets(
        &self,
        argv: &[String],
        dir: &Path,
        log: &Path,
        extra: &[(&str, &str)],
        secrets: &[(&str, &str)],
    ) -> Result<Ran, GateError> {
        let started = Instant::now();
        let file = File::create(log).map_err(|e| GateError::io(log, &e))?;
        let file = Arc::new(Mutex::new(file));
        if let Ok(mut f) = file.lock() {
            let _ = writeln!(f, "$ {}", argv.join(" "));
        }
        let container = self.sandbox.as_ref().map(|s| (s, container_name()));
        let mut command = match &container {
            // The container gets the request's env; the client only what
            // it needs to reach the daemon. Its own /tmp replaces `extra`.
            Some((sandbox, name)) => {
                let mut inner = Sandbox::clone(sandbox);
                inner.env.extend(self.env.iter().cloned());
                let names: Vec<&str> = secrets.iter().map(|(k, _)| *k).collect();
                let wrapped = inner.wrap(name, argv, dir, &names);
                let mut c = Command::new(&wrapped[0]);
                c.args(&wrapped[1..])
                    .env_clear()
                    .envs(
                        CLIENT_ENV
                            .iter()
                            .filter_map(|k| Some((*k, std::env::var_os(k)?))),
                    )
                    .envs(secrets.iter().copied());
                c
            }
            None => {
                let mut c = Command::new(&argv[0]);
                c.args(&argv[1..])
                    .env_clear()
                    .envs(
                        INHERITED_ENV
                            .iter()
                            .filter_map(|k| Some((*k, std::env::var_os(k)?))),
                    )
                    .envs(self.env.iter().cloned())
                    .envs(extra.iter().copied())
                    .envs(secrets.iter().copied());
                c
            }
        };
        command.current_dir(dir).stdin(Stdio::null());
        // One pipe for stdout and stderr, so the log keeps the order the
        // command wrote in: cargo's `Running …` (stderr) before the test
        // binary's `test … FAILED` (stdout), which the failure parser needs.
        let (output, writer) = std::io::pipe().map_err(|e| GateError::io(log, &e))?;
        let writer_err = writer.try_clone().map_err(|e| GateError::io(log, &e))?;
        command.stdout(writer).stderr(writer_err);
        own_group(&mut command);
        let spawned = command.spawn();
        // Our ends of the pipe must close, or the reader never sees EOF.
        drop(command);
        let mut child = spawned.map_err(|e| GateError::Spawn {
            program: argv[0].clone(),
            message: e.to_string(),
        })?;
        let pipes = [spawn_copy(Some(output), Arc::clone(&file))];
        let exit = loop {
            match child.try_wait() {
                Ok(Some(status)) => break exit_of(status),
                Ok(None) => {}
                Err(e) => {
                    kill(&mut child);
                    return Err(GateError::io(log, &e));
                }
            }
            if self.cancel.is_cancelled() {
                kill(&mut child);
                break Exit::Cancelled;
            }
            if started.elapsed() >= self.timeout {
                kill(&mut child);
                break Exit::TimedOut;
            }
            thread::sleep(POLL);
        };
        // Nothing the command started may outlive it and touch the repo
        // during later steps; a container lives outside the process group.
        kill_group(&child);
        let mut exit = exit;
        if let Some((sandbox, name)) = &container {
            // A step that succeeded stays a success: what it ran may have
            // let a child go (a test of memory limits). Timed out and
            // cancelled say why already.
            let failed = matches!(exit, Exit::Code(c) if c != 0) || exit == Exit::Signal;
            if failed && sandbox.oom_killed(name) {
                let limit_mb = sandbox.hardening.limits.memory_mb;
                exit = Exit::OutOfMemory { limit_mb };
                if let Ok(mut f) = file.lock() {
                    let _ = writeln!(f, "[aegis] {}", out_of_memory(limit_mb));
                }
            }
            if let Err(why) = sandbox.remove(name)
                && let Ok(mut f) = file.lock()
            {
                let _ = writeln!(f, "[aegis] could not remove container {name}: {why}");
            }
        }
        let deadline = Instant::now() + PIPE_GRACE;
        for pipe in pipes {
            let _ = pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
        Ok(Ran {
            exit,
            duration: started.elapsed(),
            log: log.to_path_buf(),
        })
    }
}
