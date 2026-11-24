//! #664: a remote worker reaches the proxy over mutual TLS and its calls
//! are checked as a local one's: an allowed call reaches the daemon, a
//! refused one never does, and a client without the worker CA's
//! certificate is turned away in the handshake. The daemon is faked on a
//! Unix socket that counts what reaches it; no Docker needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aegis_docker_proxy::{Policy, Proxy, bind};
use aegis_mtls::{MtlsFiles, acceptor, connector, server_name};
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aegis-proxy-tls-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A daemon that answers every call `200 OK` and counts them.
fn fake_daemon(socket: &Path) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = bind(socket).unwrap();
    let counted = Arc::clone(&calls);
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let counted = Arc::clone(&counted);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut read = 0;
                while let Ok(n) = s.read(&mut buf[read..]).await {
                    if n == 0 {
                        return;
                    }
                    read += n;
                    if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nOK",
                    )
                    .await;
            });
        }
    });
    calls
}

/// Sends `request` over `files`' TLS; the status line, or `None` when the
/// connection is refused.
async fn send(addr: std::net::SocketAddr, files: &MtlsFiles, request: &str) -> Option<String> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let mut tls = connector(files)
        .unwrap()
        .connect(server_name("exec.internal").unwrap(), tcp)
        .await
        .ok()?;
    tls.write_all(request.as_bytes()).await.ok()?;
    let mut answer = Vec::new();
    let _ = tls.read_to_end(&mut answer).await;
    let text = String::from_utf8_lossy(&answer).into_owned();
    text.lines()
        .next()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
}

#[tokio::test]
async fn a_remote_worker_s_calls_are_checked_as_a_local_one_s() {
    let dir = scratch();
    let (workers, hosts, other) = (ca("workers"), ca("hosts"), ca("other"));
    let host = files(&dir, "host", &hosts, &["exec.internal"], true, &workers);
    let worker = files(&dir, "worker", &workers, &["worker"], false, &hosts);
    let stranger = files(&dir, "stranger", &other, &["worker"], false, &hosts);
    let daemon = fake_daemon(&dir.join("docker.sock"));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let proxy = Proxy {
        upstream: dir.join("docker.sock"),
        policy: Policy {
            mount_roots: vec![dir.join("workspace")],
            ..Policy::default()
        },
    };
    tokio::spawn(proxy.serve_tls(listener, acceptor(&host).unwrap()));

    let ping = "GET /_ping HTTP/1.1\r\nhost: docker\r\nconnection: close\r\n\r\n";
    let status = send(addr, &worker, ping)
        .await
        .expect("the worker is served");
    assert!(status.contains("200"), "{status}");
    assert_eq!(daemon.load(Ordering::SeqCst), 1);

    // A privileged container is refused before the daemon sees it.
    let body = r#"{"Image":"alpine","HostConfig":{"Privileged":true}}"#;
    let create = format!(
        "POST /containers/create?name=x HTTP/1.1\r\nhost: docker\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let status = send(addr, &worker, &create).await.expect("answered");
    assert!(status.contains("403"), "{status}");
    // And a call aegis never makes.
    let images = "DELETE /images/alpine HTTP/1.1\r\nhost: docker\r\nconnection: close\r\n\r\n";
    let status = send(addr, &worker, images).await.expect("answered");
    assert!(status.contains("403"), "{status}");
    assert_eq!(
        daemon.load(Ordering::SeqCst),
        1,
        "nothing refused reached the daemon"
    );

    // A client of another CA never gets an answer.
    assert_eq!(send(addr, &stranger, ping).await, None);
    // Nor plain HTTP.
    let mut plain = TcpStream::connect(addr).await.unwrap();
    plain.write_all(ping.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    let _ = plain.read_to_end(&mut answer).await;
    assert!(!String::from_utf8_lossy(&answer).contains("HTTP/1.1 200"));
    assert_eq!(daemon.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
