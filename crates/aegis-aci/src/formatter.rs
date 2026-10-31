//! Runs the profile's `edit.format` command on one file, with a time limit
//! and a cap on the output kept. No shell: the argv comes from the profile's
//! command template. The command either rewrites the file in place or, with
//! `[edit] stdin`, reads it on stdin and prints it formatted (#88).

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use aegis_lang::{CommandTemplate, MAX_SOURCE_BYTES, Placeholder};

use crate::{AciError, FormatContainer};

/// Where the tree is mounted in the format container, as in the gate's.
const WORKDIR: &str = "/work";

/// `argv` in a fresh container of the profile's image with `root` as
/// `/work`, locked down as the gate's ([`aegis_gate::Hardening`]): no
/// network, no capabilities, bounded, as the host user (so the file keeps
/// its owner); `-i` when the formatter reads stdin. Returns the command and
/// the container's name.
fn in_container(
    c: &FormatContainer,
    root: &Path,
    argv: &[String],
    interactive: bool,
) -> (Vec<String>, String) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "aegis-format-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut out: Vec<String> = [c.docker.as_str(), "run", "--rm", "--name", &name]
        .map(str::to_owned)
        .to_vec();
    if interactive {
        out.insert(2, "-i".to_owned());
    }
    out.extend(c.hardening().args("none"));
    out.extend([
        "--volume".to_owned(),
        format!("{}:{WORKDIR}", root.display()),
        "--workdir".to_owned(),
        WORKDIR.to_owned(),
    ]);
    for (host, inside) in &c.mounts {
        out.extend(["--volume".to_owned(), format!("{host}:{inside}:ro")]);
    }
    out.push(c.image.clone());
    out.extend(argv.iter().cloned());
    (out, name)
}

/// A formatter still running after this long is killed.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(60);

/// Bytes of stdout/stderr kept for the agent.
const MAX_OUTPUT_BYTES: u64 = 16 * 1024;

/// Lines of formatter output shown to the agent.
const MAX_MESSAGE_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    Exit { code: Option<i32>, message: String },
    TimedOut,
}

/// How long to wait for the output pipes to close once the formatter has
/// exited. A daemon it started may hold them open for good; its output is
/// then dropped rather than blocking the edit.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// The formatted file a stdin formatter may print: a source file's limit,
/// and one more byte to tell a longer output.
const MAX_FORMATTED_BYTES: u64 = MAX_SOURCE_BYTES as u64 + 1;

/// Reads up to `limit` bytes of `stream` on its own thread. The thread
/// keeps draining the rest so the formatter never blocks on a full pipe,
/// and is left behind if the pipe never closes.
fn capture(stream: Option<impl Read + Send + 'static>, limit: u64) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stream {
            let mut limited = s.take(limit);
            let _ = limited.read_to_end(&mut buf);
            let _ = sender.send(buf);
            let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
        } else {
            let _ = sender.send(buf);
        }
    });
    receiver
}

fn wait(child: &mut Child, program: &str) -> Result<Option<std::process::ExitStatus>, AciError> {
    let started = Instant::now();
    loop {
        let status = child.try_wait().map_err(|e| AciError::Formatter {
            program: program.to_owned(),
            message: e.to_string(),
        })?;
        if status.is_some() {
            return Ok(status);
        }
        if started.elapsed() >= TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn failed(message: impl Into<String>) -> Result<Result<Option<String>, Failure>, AciError> {
    Ok(Err(Failure::Exit {
        code: None,
        message: message.into(),
    }))
}

/// `relative` is the file's path from `root`; it is passed as `./path` so
/// a name starting with `-` cannot read as an option. With `input` the
/// formatter reads it on stdin, and its stdout is the formatted file
/// (`Ok(Some(text))`); without, it rewrites the file (`Ok(None)`).
pub(crate) fn run(
    command: &CommandTemplate,
    root: &Path,
    relative: &str,
    container: Option<&FormatContainer>,
    input: Option<&str>,
) -> Result<Result<Option<String>, Failure>, AciError> {
    let argument = format!("./{relative}");
    let argv = command.render(&[(Placeholder::File, &argument)])?;
    let (argv, name) = match container {
        Some(c) => {
            let (argv, name) = in_container(c, root, &argv, input.is_some());
            (argv, Some((c.docker.clone(), name)))
        }
        None => (argv, None),
    };
    let program = argv[0].clone();
    let mut child = Command::new(&program)
        .args(&argv[1..])
        .current_dir(root)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AciError::Formatter {
            program: program.clone(),
            message: e.to_string(),
        })?;
    // On its own thread: a formatter that writes before it has read all of
    // stdin would otherwise block on a full pipe, and us with it.
    if let (Some(text), Some(mut pipe)) = (input, child.stdin.take()) {
        let text = text.to_owned();
        thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let out_limit = if input.is_some() {
        MAX_FORMATTED_BYTES
    } else {
        MAX_OUTPUT_BYTES
    };
    let stdout = capture(child.stdout.take(), out_limit);
    let stderr = capture(child.stderr.take(), MAX_OUTPUT_BYTES);
    let Some(status) = wait(&mut child, &program)? else {
        // Killing the client leaves the container running.
        if let Some((docker, name)) = &name {
            let _ = Command::new(docker)
                .args(["kill", name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        return Ok(Err(Failure::TimedOut));
    };
    if status.success() {
        let Some(text) = input else {
            return Ok(Ok(None));
        };
        let Ok(out) = stdout.recv_timeout(PIPE_GRACE) else {
            return failed("the formatter's output did not end");
        };
        if out.len() as u64 >= MAX_FORMATTED_BYTES {
            return failed("the formatter printed more than a source file may hold");
        }
        // An empty file for a non-empty one is a formatter that printed
        // nothing (a wrong flag), not a formatted file.
        if out.is_empty() && !text.is_empty() {
            return failed("the formatter printed nothing");
        }
        return match String::from_utf8(out) {
            Ok(formatted) => Ok(Ok(Some(formatted))),
            Err(_) => failed("the formatter printed text that is not UTF-8"),
        };
    }
    let deadline = Instant::now() + PIPE_GRACE;
    let output = [stderr, stdout]
        .into_iter()
        .map(|r| {
            let left = deadline.saturating_duration_since(Instant::now());
            String::from_utf8_lossy(&r.recv_timeout(left).unwrap_or_default()).into_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let message = output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(MAX_MESSAGE_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Err(Failure::Exit {
        code: status.code(),
        message,
    }))
}
