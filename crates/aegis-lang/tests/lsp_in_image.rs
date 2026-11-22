//! #549: each profile's language server (`[aci] lsp`) is in its sandbox
//! image and answers `initialize` there, run as ACI runs it: no network,
//! capabilities dropped, read-only image with the profile's writable
//! directories. A server missing from its image made ACI's LSP tools
//! vanish without a word. Needs Docker and the images; a profile whose
//! image is missing is skipped, unless `AEGIS_REQUIRE_DOCKER=1`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use aegis_lang::{Profile, Profiles};
use serde_json::{Value, json};

/// jdtls is the slowest to answer, a few seconds on a laptop.
const TIMEOUT: Duration = Duration::from_secs(180);

fn image_present(image: &str) -> bool {
    let ok = Command::new("docker")
        .args(["image", "inspect", image])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or the image {image} is missing"
        );
        eprintln!("skipping {image}: docker or the image is missing");
    }
    ok
}

/// The host user, as ACI runs the server (`Hardening`): tools that look up
/// their home from the user (jdtls, as root, wrote to `/root`) see the
/// image's writable `HOME` instead.
fn host_user() -> String {
    let id = |flag: &str| {
        Command::new("id")
            .arg(flag)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|_| "1000".to_owned())
    };
    format!("{}:{}", id("-u"), id("-g"))
}

fn frame(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// The next message on `out`, or `None` once it closes.
fn read_message(out: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if out.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; length?];
    out.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// A container this test started, removed when dropped (#610): killing
/// the `docker run` client leaves the container running, and jdtls does
/// not exit when its stdin closes, so `--rm` alone never fires. Dropped on
/// a panic or a timeout too.
struct Container(String);

impl Container {
    fn named() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        Self(format!(
            "aegis-lsp-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The server's answer to `initialize`, or why there is none.
fn initialize(profile: &Profile) -> Result<Value, String> {
    let lsp = profile.aci.lsp.as_ref().expect("checked by the caller");
    let container = Container::named();
    let mut argv: Vec<String> = [
        "run",
        "-i",
        "--rm",
        "--name",
        &container.0,
        "--network",
        "none",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,exec,nosuid,nodev,size=512m",
    ]
    .map(str::to_owned)
    .to_vec();
    argv.extend(["--user".into(), host_user()]);
    for dir in &profile.sandbox.writable {
        argv.extend(["--mount".into(), format!("type=volume,destination={dir}")]);
    }
    argv.push(profile.sandbox.image.clone());
    argv.extend(lsp.argv().iter().cloned());
    let mut child = Command::new("docker")
        .args(&argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run docker: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"processId": null, "rootUri": "file:///tmp", "capabilities": {}},
    });
    stdin
        .write_all(&frame(&request))
        .and_then(|()| stdin.flush())
        .map_err(|e| format!("cannot write to the server: {e}"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = BufReader::new(stdout);
        // Notifications and server requests may come first.
        while let Some(message) = read_message(&mut out) {
            if message.get("id") == Some(&json!(1)) && message.get("method").is_none() {
                let _ = tx.send(message);
                return;
            }
        }
    });
    let answer = rx.recv_timeout(TIMEOUT);
    drop(container);
    let _ = child.kill();
    let _ = child.wait();
    let mut errors = String::new();
    let _ = stderr.read_to_string(&mut errors);
    answer.map_err(|_| format!("no answer to initialize; its stderr:\n{errors}"))
}

#[test]
fn every_language_server_answers_in_its_image() {
    let profiles = Profiles::builtin().unwrap();
    let mut languages: Vec<_> = profiles.languages().collect();
    languages.sort_by_key(|l| format!("{l:?}"));
    for language in languages {
        let profile = profiles.get(language).unwrap();
        if profile.aci.lsp.is_none() || !image_present(&profile.sandbox.image) {
            continue;
        }
        let answer = initialize(profile).unwrap_or_else(|e| panic!("{language:?}: {e}"));
        assert!(
            answer.pointer("/result/capabilities").is_some(),
            "{language:?}: {answer}"
        );
    }
}
