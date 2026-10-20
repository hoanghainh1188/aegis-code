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
        let started = Instant::now();
        let file = File::create(log).map_err(|e| GateError::io(log, &e))?;
        let file = Arc::new(Mutex::new(file));
        if let Ok(mut f) = file.lock() {
            let _ = writeln!(f, "$ {}", argv.join(" "));
        }
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(dir)
            .env_clear()
            .envs(
                INHERITED_ENV
                    .iter()
                    .filter_map(|k| Some((*k, std::env::var_os(k)?))),
            )
            .envs(self.env.iter().cloned())
            .envs(extra.iter().copied())
            .stdin(Stdio::null());
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
        // during later steps.
        kill_group(&child);
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
