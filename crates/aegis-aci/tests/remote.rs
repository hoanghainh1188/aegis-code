//! #664: the `aegis-aci` binary serves its tools to a remote worker over
//! MCP Streamable HTTP with mutual TLS. The worker's `McpTools` reaches
//! them with its certificate and calls them on the host's workspace; a
//! worker of another CA, a plain-HTTP client, and an `http://` URL are
//! refused. The binary refuses to listen without TLS.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use aegis_mtls::MtlsFiles;
use aegis_orchestrator::{McpTools, RemoteMcpSpec, ToolExecutor};
use common::Scratch;
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair};
use serde_json::json;

struct Ca {
    pem: String,
    issuer: Issuer<'static, KeyPair>,
}

fn ca(name: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    let cert = params.self_signed(&key).unwrap();
    Ca {
        pem: cert.pem(),
        issuer: Issuer::new(params, key),
    }
}

fn files(
    dir: &Path,
    who: &str,
    issuer: &Ca,
    names: &[&str],
    server: bool,
    trusts: &Ca,
) -> MtlsFiles {
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).unwrap();
    params.extended_key_usages = vec![if server {
        ExtendedKeyUsagePurpose::ServerAuth
    } else {
        ExtendedKeyUsagePurpose::ClientAuth
    }];
    let cert = params.signed_by(&key, &issuer.issuer).unwrap();
    let write = |name: String, text: String| {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    MtlsFiles {
        cert: write(format!("{who}.crt"), cert.pem()),
        key: write(format!("{who}.key"), key.serialize_pem()),
        ca: write(format!("{who}-ca.crt"), trusts.pem.clone()),
    }
}

/// The ACI binary serving `repo` on a free port over `host`'s TLS.
struct Served {
    child: Child,
    port: u16,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn serve(repo: &Path, host: &MtlsFiles) -> Served {
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aegis-aci"))
        .args(["--repo", &repo.display().to_string(), "--language", "rust"])
        .args(["--listen", &format!("127.0.0.1:{port}")])
        .args(["--tls-cert", &host.cert.display().to_string()])
        .args(["--tls-key", &host.key.display().to_string()])
        .args(["--tls-client-ca", &host.ca.display().to_string()])
        .args(["--allowed-host", "localhost"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Served { child, port };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("aegis-aci did not listen on {port}");
}

fn spec(url: String, tls: &MtlsFiles) -> RemoteMcpSpec {
    RemoteMcpSpec {
        name: "aci".into(),
        url,
        tls: tls.clone(),
        tools: Some(vec!["open".into(), "search_symbol".into()]),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_worker_uses_the_tools_over_mutual_tls_only() {
    let repo = Scratch::fixture("rust");
    let dir: PathBuf = repo.outside();
    std::fs::create_dir_all(&dir).unwrap();
    let (workers, hosts, other) = (ca("workers"), ca("hosts"), ca("other"));
    let host = files(&dir, "host", &hosts, &["localhost"], true, &workers);
    let worker = files(&dir, "worker", &workers, &["worker"], false, &hosts);
    let stranger = files(&dir, "stranger", &other, &["worker"], false, &hosts);
    let served = serve(&repo.repo(), &host);
    let url = format!("https://localhost:{}/mcp", served.port);

    let tools = McpTools::connect_with(&[], &[spec(url.clone(), &worker)])
        .await
        .expect("the worker connects");
    let names: Vec<&str> = tools
        .definitions()
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, ["open", "search_symbol"]);
    // A tool runs on the host's workspace.
    let opened = tools
        .call("open", &json!({"file": "src/lib.rs", "line": 1}))
        .await;
    assert!(!opened.is_error, "{}", opened.content);
    assert!(opened.content.contains("src/lib.rs"), "{}", opened.content);
    let escaped = tools.call("open", &json!({"file": "../etc/passwd"})).await;
    assert!(escaped.is_error, "the workspace's bounds hold remotely");
    tools.shutdown().await;

    // Another CA's worker, plain HTTP, the wrong path: refused.
    assert!(
        McpTools::connect_with(&[], &[spec(url.clone(), &stranger)])
            .await
            .is_err()
    );
    let plain = format!("http://localhost:{}/mcp", served.port);
    assert!(
        McpTools::connect_with(&[], &[spec(plain, &worker)])
            .await
            .is_err()
    );
    let wrong = format!("https://localhost:{}/other", served.port);
    assert!(
        McpTools::connect_with(&[], &[spec(wrong, &worker)])
            .await
            .is_err()
    );
}

#[test]
fn the_binary_never_listens_without_tls() {
    let out = Command::new(env!("CARGO_BIN_EXE_aegis-aci"))
        .args([
            "--repo",
            ".",
            "--language",
            "rust",
            "--listen",
            "127.0.0.1:0",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--listen needs --tls-cert"), "{err}");
}
