//! #664: a listener with `server_config` talks only to a client whose
//! certificate chains to its CA; a client with `client_config` talks only
//! to a host whose certificate chains to its CA and names the host.

use std::path::{Path, PathBuf};

use aegis_mtls::{MtlsFiles, acceptor, connector, server_name};
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair};
use rustls::pki_types::pem::PemObject;
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

/// A leaf of `ca` for `names`, as a server's or a client's.
fn leaf(ca: &Ca, names: &[&str], server: bool) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).unwrap();
    params.extended_key_usages = vec![if server {
        ExtendedKeyUsagePurpose::ServerAuth
    } else {
        ExtendedKeyUsagePurpose::ClientAuth
    }];
    let cert = params.signed_by(&key, &ca.issuer).unwrap();
    (cert.pem(), key.serialize_pem())
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn files(dir: &Path, who: &str, (cert, key): (String, String), trusts: &Ca) -> MtlsFiles {
    MtlsFiles {
        cert: write(dir, &format!("{who}.crt"), &cert),
        key: write(dir, &format!("{who}.key"), &key),
        ca: write(dir, &format!("{who}-trusts.crt"), &trusts.pem),
    }
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aegis-mtls-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An echo server on `server`'s TLS; its address.
async fn echo(server: &MtlsFiles) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = acceptor(server).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let mut buf = [0u8; 5];
                    if tls.read_exact(&mut buf).await.is_ok() {
                        let _ = tls.write_all(&buf).await;
                    }
                }
            });
        }
    });
    addr
}

/// Whether `client` gets its five bytes back from `addr` as `host`.
async fn talks(addr: std::net::SocketAddr, client: &MtlsFiles, host: &str) -> bool {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let Ok(mut tls) = connector(client)
        .unwrap()
        .connect(server_name(host).unwrap(), tcp)
        .await
    else {
        return false;
    };
    let mut back = [0u8; 5];
    tls.write_all(b"hello").await.is_ok()
        && tls.read_exact(&mut back).await.is_ok()
        && &back == b"hello"
}

#[tokio::test]
async fn only_certificates_of_the_pinned_cas_get_through() {
    let dir = scratch();
    let (workers, hosts, other) = (ca("workers"), ca("hosts"), ca("someone else"));
    let host = files(
        &dir,
        "host",
        leaf(&hosts, &["exec.internal"], true),
        &workers,
    );
    let addr = echo(&host).await;

    let worker = files(&dir, "worker", leaf(&workers, &["worker"], false), &hosts);
    assert!(
        talks(addr, &worker, "exec.internal").await,
        "the worker gets through"
    );

    // A client certificate of another CA: refused.
    let stranger = files(&dir, "stranger", leaf(&other, &["worker"], false), &hosts);
    assert!(!talks(addr, &stranger, "exec.internal").await);

    // No client certificate at all: refused.
    let tcp = TcpStream::connect(addr).await.unwrap();
    let anonymous = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates({
        let mut roots = rustls::RootCertStore::empty();
        for c in rustls::pki_types::CertificateDer::pem_file_iter(&worker.ca).unwrap() {
            roots.add(c.unwrap()).unwrap();
        }
        roots
    })
    .with_no_client_auth();
    let anonymous = tokio_rustls::TlsConnector::from(std::sync::Arc::new(anonymous));
    let refused = match anonymous
        .connect(server_name("exec.internal").unwrap(), tcp)
        .await
    {
        Err(_) => true,
        Ok(mut tls) => {
            // TLS 1.3 reports the refusal on the first read.
            let _ = tls.write_all(b"hello").await;
            let mut back = [0u8; 5];
            tls.read_exact(&mut back).await.is_err()
        }
    };
    assert!(refused, "a client without a certificate is refused");

    // Plain TCP: no answer that is not a TLS alert.
    let mut plain = TcpStream::connect(addr).await.unwrap();
    plain.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    let mut answer = Vec::new();
    let _ = plain.read_to_end(&mut answer).await;
    assert!(
        !String::from_utf8_lossy(&answer).contains("HTTP/1.1"),
        "{answer:?}"
    );

    // The worker refuses a host of another CA, or under another name.
    let impostor = files(
        &dir,
        "impostor",
        leaf(&other, &["exec.internal"], true),
        &workers,
    );
    let impostor_addr = echo(&impostor).await;
    assert!(!talks(impostor_addr, &worker, "exec.internal").await);
    assert!(!talks(addr, &worker, "other.internal").await);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn certificate_key_and_ca_go_together() {
    assert_eq!(MtlsFiles::from_flags(None, None, None), Ok(None));
    assert!(MtlsFiles::from_flags(Some("c".into()), None, Some("a".into())).is_err());
    let dir = scratch();
    let bad = MtlsFiles {
        cert: write(&dir, "empty.crt", ""),
        key: write(&dir, "empty.key", ""),
        ca: write(&dir, "empty-ca.crt", ""),
    };
    let err = aegis_mtls::server_config(&bad).unwrap_err().to_string();
    assert!(err.contains("empty"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
