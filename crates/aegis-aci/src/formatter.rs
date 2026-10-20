//! Runs the profile's `edit.format` command on one file, with a time limit
//! and a cap on the output kept. No shell: the argv comes from the profile's
//! command template.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use aegis_lang::{CommandTemplate, Placeholder};

use crate::AciError;

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

fn capture(stream: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stream {
            let mut limited = s.take(MAX_OUTPUT_BYTES);
            let _ = limited.read_to_end(&mut buf);
            // Keep draining so the formatter never blocks on a full pipe.
            let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
        }
        buf
    })
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

/// `relative` is the file's path from `root`; it is passed as `./path` so
/// a name starting with `-` cannot read as an option.
pub(crate) fn run(
    command: &CommandTemplate,
    root: &Path,
    relative: &str,
) -> Result<Result<(), Failure>, AciError> {
    let argument = format!("./{relative}");
    let argv = command.render(&[(Placeholder::File, &argument)])?;
    let program = argv[0].clone();
    let mut child = Command::new(&program)
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AciError::Formatter {
            program: program.clone(),
            message: e.to_string(),
        })?;
    let stdout = capture(child.stdout.take());
    let stderr = capture(child.stderr.take());
    let status = wait(&mut child, &program)?;
    let output = [stderr, stdout]
        .into_iter()
        .map(|h| String::from_utf8_lossy(&h.join().unwrap_or_default()).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let Some(status) = status else {
        return Ok(Err(Failure::TimedOut));
    };
    if status.success() {
        return Ok(Ok(()));
    }
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
