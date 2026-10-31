//! A minimal LSP client on stdio (#89): the profile's `aci.lsp` command, in
//! the profile's image when the gate runs in containers (a language server
//! runs the repo's build scripts and proc macros while it indexes). Each
//! part of a repo of several languages has its own, on its directory
//! (#199); paths in and out stay relative to the repo root. Only
//! what `find_definition` and `find_references` need: initialize, wait for
//! indexing, keep edited files in sync, ask, shut down.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use aegis_lang::{in_dir, join_dir};
use serde_json::{Value, json};

use crate::{AciError, FormatContainer};

/// Where the tree is mounted in the container, as in the gate's.
const WORKDIR: &str = "/work";

/// Indexing is over when no progress is open and nothing came for this long.
const QUIET: Duration = Duration::from_secs(1);

/// The longest a server may index; past it, the tools say so for good.
pub(crate) const INDEX_TIMEOUT: Duration = Duration::from_secs(300);

/// How long one tool call waits for indexing before it tells the agent to
/// come back later; the other tools share the ACI's lock meanwhile.
pub(crate) const CALL_WAIT: Duration = Duration::from_secs(30);

/// One request's answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest message the client reads; a server sending more is broken.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Default)]
struct Shared {
    /// Answers waited for, by request id.
    waiting: BTreeMap<u64, Sender<Result<Value, String>>>,
    open_progress: BTreeSet<String>,
    last_message: Option<Instant>,
    initialized: bool,
    /// The server stopped (closed its stdout, or the message was broken).
    gone: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Lsp {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Shared>>,
    next_id: u64,
    started: Instant,
    /// `file:///work` in the container, the part's URI on the machine.
    base_uri: String,
    /// The part's directory from the repo root; `""` is the root.
    dir: String,
    /// The repo root's URI on the machine: a place in another part is
    /// still in the repo.
    repo_uri: Option<String>,
    /// Files the server has open, with their version.
    open: BTreeMap<String, i64>,
    /// The container's name, to stop it.
    container: Option<(String, String)>,
}

/// What a request position or result points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Position {
    /// Relative to the root.
    pub path: String,
    /// 0-based, as LSP counts.
    pub line: u32,
    /// UTF-16 units from the line start, as LSP counts.
    pub character: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Located {
    InRepo(Position),
    /// In the standard library, a dependency: not a file the agent can open.
    Outside(String),
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

fn write_message(stdin: &Mutex<ChildStdin>, message: &Value) -> Result<(), String> {
    let body = message.to_string();
    let mut pipe = stdin.lock().unwrap_or_else(PoisonError::into_inner);
    write!(pipe, "Content-Length: {}\r\n\r\n{body}", body.len())
        .and_then(|()| pipe.flush())
        .map_err(|e| format!("the language server stopped reading: {e}"))
}

/// One message, `Ok(None)` at the end of the stream.
fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or("a message without Content-Length")?;
    if length > MAX_MESSAGE_BYTES {
        return Err(format!("a message of {length} bytes"));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Reads the server's messages: answers go to their waiter, the server's
/// own requests get an empty answer, progress is tracked for indexing.
fn read_loop(stdout: impl Read, stdin: Arc<Mutex<ChildStdin>>, shared: Arc<Mutex<Shared>>) {
    let mut reader = BufReader::new(stdout);
    let end = loop {
        let message = match read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break "the language server exited".to_owned(),
            Err(e) => break format!("the language server sent a broken message: {e}"),
        };
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        let mut state = lock(&shared);
        state.last_message = Some(Instant::now());
        match (method, id) {
            // A request of the server's (progress tokens, configuration,
            // refreshes): answered empty, which servers accept.
            (Some(_), Some(id)) => {
                drop(state);
                let _ = write_message(&stdin, &json!({"jsonrpc": "2.0", "id": id, "result": null}));
            }
            (Some("$/progress"), None) => {
                let token = message["params"]["token"].to_string();
                match message["params"]["value"]["kind"].as_str() {
                    Some("begin") => {
                        state.open_progress.insert(token);
                    }
                    Some("end") => {
                        state.open_progress.remove(&token);
                    }
                    _ => {}
                }
            }
            (None, Some(id)) => {
                let answer = match message.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("error").to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                if let Some(waiter) = id.as_u64().and_then(|n| state.waiting.remove(&n)) {
                    let _ = waiter.send(answer);
                }
            }
            _ => {}
        }
    };
    let mut state = lock(&shared);
    state.gone = Some(end.clone());
    for (_, waiter) in std::mem::take(&mut state.waiting) {
        let _ = waiter.send(Err(end.clone()));
    }
}

/// `path` as a URI path: `/`-separated, with what URIs cannot hold escaped.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode_path(uri_path: &str) -> String {
    let bytes = uri_path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Lsp {
    /// Starts `argv` on the part of `repo` in `dir` and asks it to
    /// initialize; indexing goes on in the background.
    pub(crate) fn start(
        argv: &[String],
        repo: &Path,
        dir: &str,
        options: Option<Value>,
        container: Option<&FormatContainer>,
    ) -> Result<Self, AciError> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = &match dir {
            "" => repo.to_path_buf(),
            dir => repo.join(dir),
        };
        let repo_uri = container
            .is_none()
            .then(|| format!("file://{}", encode_path(&repo.display().to_string())));
        let (argv, base_uri, container_name) = match container {
            Some(c) => {
                let name = format!(
                    "aegis-lsp-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                );
                let mut out: Vec<String> =
                    [c.docker.as_str(), "run", "-i", "--rm", "--name", &name]
                        .map(str::to_owned)
                        .to_vec();
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
                (
                    out,
                    format!("file://{WORKDIR}"),
                    Some((c.docker.clone(), name)),
                )
            }
            None => (
                argv.to_vec(),
                format!("file://{}", encode_path(&root.display().to_string())),
                None,
            ),
        };
        let program = argv[0].clone();
        let mut child = Command::new(&program)
            .args(&argv[1..])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AciError::LanguageServer(format!("cannot start `{program}`: {e}")))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("piped")));
        let stdout = child.stdout.take().expect("piped");
        let shared = Arc::new(Mutex::new(Shared::default()));
        thread::spawn({
            let (stdin, shared) = (Arc::clone(&stdin), Arc::clone(&shared));
            move || read_loop(stdout, stdin, shared)
        });
        let mut lsp = Self {
            child,
            stdin,
            shared,
            next_id: 0,
            started: Instant::now(),
            base_uri,
            dir: dir.to_owned(),
            repo_uri,
            open: BTreeMap::new(),
            container: container_name,
        };
        let params = json!({
            "processId": std::process::id(),
            "rootUri": lsp.base_uri,
            "workspaceFolders": [{"uri": lsp.base_uri, "name": "repo"}],
            "capabilities": {
                "window": {"workDoneProgress": true},
                "textDocument": {
                    "definition": {"linkSupport": false},
                    "synchronization": {"didSave": false},
                },
            },
            "initializationOptions": options.unwrap_or(Value::Null),
        });
        // Answered in the background: the first tool call waits for it.
        let answer = lsp.send_request("initialize", params)?;
        thread::spawn({
            let (stdin, shared) = (Arc::clone(&lsp.stdin), Arc::clone(&lsp.shared));
            move || {
                if answer.recv_timeout(INDEX_TIMEOUT).is_ok_and(|a| a.is_ok()) {
                    let note = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
                    let _ = write_message(&stdin, &note);
                    lock(&shared).initialized = true;
                }
            }
        });
        Ok(lsp)
    }

    fn send_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Receiver<Result<Value, String>>, AciError> {
        self.next_id += 1;
        let id = self.next_id;
        let (sender, receiver) = mpsc::channel();
        lock(&self.shared).waiting.insert(id, sender);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        write_message(&self.stdin, &message).map_err(AciError::LanguageServer)?;
        Ok(receiver)
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), AciError> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        write_message(&self.stdin, &message).map_err(AciError::LanguageServer)
    }

    /// Waits up to `wait` for indexing to end. `Err` says why the tools
    /// cannot answer yet, or at all.
    pub(crate) fn ready(&self, wait: Duration) -> Result<(), AciError> {
        let until = Instant::now() + wait;
        loop {
            {
                let state = lock(&self.shared);
                if let Some(gone) = &state.gone {
                    return Err(AciError::LanguageServer(gone.clone()));
                }
                let quiet = state.last_message.is_some_and(|t| t.elapsed() >= QUIET);
                if state.initialized && state.open_progress.is_empty() && quiet {
                    return Ok(());
                }
            }
            if self.started.elapsed() >= INDEX_TIMEOUT {
                return Err(AciError::LanguageServer(format!(
                    "the language server did not finish indexing in {}s",
                    INDEX_TIMEOUT.as_secs()
                )));
            }
            if Instant::now() >= until {
                return Err(AciError::LanguageServer(format!(
                    "the language server is still indexing ({}s so far); use search_symbol \
                     or search_text meanwhile, or try again shortly",
                    self.started.elapsed().as_secs()
                )));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The URI of `relative`, a path from the repo root inside the part.
    pub(crate) fn uri(&self, relative: &str) -> String {
        let in_part = in_dir(&self.dir, relative).unwrap_or(relative);
        format!("{}/{}", self.base_uri, encode_path(in_part))
    }

    /// The file's current text, as the server should see it: opened the
    /// first time, then replaced whenever the ACI wrote it.
    pub(crate) fn sync(
        &mut self,
        relative: &str,
        text: &str,
        language: &str,
    ) -> Result<(), AciError> {
        let uri = self.uri(relative);
        match self.open.get_mut(relative) {
            None => {
                self.open.insert(relative.to_owned(), 1);
                self.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {
                        "uri": uri, "languageId": language, "version": 1, "text": text,
                    }}),
                )
            }
            Some(version) => {
                *version += 1;
                let version = *version;
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": version},
                        "contentChanges": [{"text": text}],
                    }),
                )
            }
        }
    }

    pub(crate) fn is_open(&self, relative: &str) -> bool {
        self.open.contains_key(relative)
    }

    /// `method` at `at`, the locations it answers with.
    pub(crate) fn locations(
        &mut self,
        method: &str,
        at: &Position,
        extra: Value,
    ) -> Result<Vec<Located>, AciError> {
        let mut params = json!({
            "textDocument": {"uri": self.uri(&at.path)},
            "position": {"line": at.line, "character": at.character},
        });
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        let answer = self.send_request(method, params)?;
        let result = match answer.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(AciError::LanguageServer(e)),
            Err(_) => {
                return Err(AciError::LanguageServer(format!(
                    "no answer to {method} in {}s",
                    REQUEST_TIMEOUT.as_secs()
                )));
            }
        };
        // A Location, a list of them, or LocationLinks.
        let items = match result {
            Value::Null => vec![],
            Value::Array(items) => items,
            one => vec![one],
        };
        Ok(items
            .iter()
            .filter_map(|item| {
                let uri = item
                    .get("uri")
                    .or_else(|| item.get("targetUri"))?
                    .as_str()?;
                let range = item
                    .get("range")
                    .or_else(|| item.get("targetSelectionRange"))?;
                let start = &range["start"];
                Some(self.locate(
                    uri,
                    start["line"].as_u64()? as u32,
                    start["character"].as_u64()? as u32,
                ))
            })
            .collect())
    }

    fn locate(&self, uri: &str, line: u32, character: u32) -> Located {
        let in_repo = |path: String| {
            Located::InRepo(Position {
                path,
                line,
                character,
            })
        };
        if let Some(rest) = uri.strip_prefix(&format!("{}/", self.base_uri)) {
            return in_repo(join_dir(&self.dir, &decode_path(rest)));
        }
        let elsewhere = self
            .repo_uri
            .as_ref()
            .and_then(|repo| uri.strip_prefix(&format!("{repo}/")));
        match elsewhere {
            Some(rest) => in_repo(decode_path(rest)),
            None => Located::Outside(decode_path(uri.trim_start_matches("file://"))),
        }
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        // Polite first; a server that does not stop is stopped.
        if let Ok(answer) = self.send_request("shutdown", Value::Null) {
            let _ = answer.recv_timeout(Duration::from_secs(5));
            let _ = self.notify("exit", Value::Null);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        if let Some((docker, name)) = &self.container {
            let _ = Command::new(docker)
                .args(["kill", name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The root's files the ACI changed since the server last saw them.
#[derive(Debug, Default)]
pub(crate) struct Changed(BTreeSet<PathBuf>);

impl Changed {
    pub(crate) fn mark(&mut self, path: &Path) {
        self.0.insert(path.to_path_buf());
    }

    /// The changed files `of` says are its; the others wait for theirs.
    pub(crate) fn take_where(&mut self, of: impl Fn(&Path) -> bool) -> BTreeSet<PathBuf> {
        let (taken, kept) = std::mem::take(&mut self.0).into_iter().partition(|p| of(p));
        self.0 = kept;
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_survive_the_uri_round_trip() {
        for path in ["src/lib.rs", "a b/ç.rs", "x%y/[1].rs"] {
            assert_eq!(decode_path(&encode_path(path)), path);
        }
        assert_eq!(encode_path("a b"), "a%20b");
    }

    /// A language server in a few lines: answers `initialize`, then per
    /// `mode` stays busy, exits, or answers `definition` with one place in
    /// the repo and one in the standard library.
    const FAKE: &str = r#"
import json, sys
mode = sys.argv[1]
def read():
    n = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line: sys.exit(0)
        if line.strip() == b"": break
        k, v = line.decode().split(":", 1)
        if k.lower() == "content-length": n = int(v)
    return json.loads(sys.stdin.buffer.read(n))
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b); sys.stdout.buffer.flush()
while True:
    m = read()
    if m.get("method") == "initialize":
        if mode == "exit": sys.exit(0)
        root = m["params"]["rootUri"]
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {}}})
        send({"jsonrpc": "2.0", "id": "c1", "method": "window/workDoneProgress/create", "params": {"token": "i"}})
        send({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "i", "value": {"kind": "begin", "title": "Indexing"}}})
        if mode != "busy":
            send({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "i", "value": {"kind": "end"}}})
    elif m.get("method") == "textDocument/definition":
        send({"jsonrpc": "2.0", "id": m["id"], "result": [
            {"uri": root + "/src/a%20b.rs", "range": {"start": {"line": 4, "character": 2}, "end": {"line": 4, "character": 5}}},
            {"targetUri": "file:///rust/lib/core.rs", "targetRange": {}, "targetSelectionRange": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 1}}}]})
    elif m.get("method") == "shutdown":
        send({"jsonrpc": "2.0", "id": m["id"], "result": None})
"#;

    fn fake(mode: &str) -> Option<Lsp> {
        if Command::new("python3").arg("--version").output().is_err() {
            return None;
        }
        let argv: Vec<String> = ["python3", "-c", FAKE, mode].map(String::from).to_vec();
        Some(Lsp::start(&argv, &std::env::temp_dir(), "", None, None).unwrap())
    }

    #[test]
    fn a_server_that_keeps_indexing_is_reported_not_waited_for() {
        let Some(lsp) = fake("busy") else { return };
        let started = Instant::now();
        let err = lsp.ready(Duration::from_millis(1500)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(err.to_string().contains("still indexing"), "{err}");
    }

    #[test]
    fn a_server_that_exits_says_so() {
        let Some(lsp) = fake("exit") else { return };
        thread::sleep(Duration::from_millis(500));
        let err = lsp.ready(Duration::from_secs(2)).unwrap_err();
        assert!(err.to_string().contains("exited"), "{err}");
    }

    #[test]
    fn answers_map_back_to_the_repo_or_outside_it() {
        let Some(mut lsp) = fake("ok") else { return };
        lsp.ready(Duration::from_secs(10)).unwrap();
        let at = Position {
            path: "src/a b.rs".into(),
            line: 0,
            character: 0,
        };
        let found = lsp
            .locations("textDocument/definition", &at, json!({}))
            .unwrap();
        assert_eq!(
            found,
            [
                Located::InRepo(Position {
                    path: "src/a b.rs".into(),
                    line: 4,
                    character: 2,
                }),
                Located::Outside("/rust/lib/core.rs".into()),
            ]
        );
    }

    #[test]
    fn messages_are_framed_by_content_length() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let text = format!(
            "Content-Length: {}\r\nContent-Type: x\r\n\r\n{body}",
            body.len()
        );
        let mut reader = BufReader::new(text.as_bytes());
        let message = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(message["id"], 1);
        assert_eq!(read_message(&mut reader).unwrap(), None);
        let mut broken = BufReader::new("Content-Type: x\r\n\r\n{}".as_bytes());
        assert!(read_message(&mut broken).is_err());
    }
}
