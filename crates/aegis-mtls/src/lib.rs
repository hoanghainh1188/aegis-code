//! Mutual TLS for a remote execution host (#664): the worker reaches the
//! host's ACI and Docker proxy over the network, and each side proves who
//! it is. The host's listeners take only clients whose certificate chains
//! to the CA they are given (a private CA for the worker, not a public
//! one: that pins the worker); the worker takes only a host whose
//! certificate chains to the CA it is given and names the host it dialed.
//! A connection without a client certificate, or with one from another CA,
//! is refused in the handshake, before any request is read.
//!
//! Certificates and keys are PEM files: the operator's PKI makes and
//! rotates them; a listener reads them when it starts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
pub use tokio_rustls::{TlsAcceptor, TlsConnector, client, server};

/// Why mutual TLS could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum MtlsError {
    #[error("{path}: {message}")]
    Pem { path: PathBuf, message: String },
    #[error("{0}")]
    Tls(String),
}

/// One side's certificate chain and key, and the CA it trusts for the
/// other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtlsFiles {
    /// This side's certificate (its chain, leaf first), PEM.
    pub cert: PathBuf,
    /// Its private key, PEM (PKCS#8, PKCS#1 or SEC1).
    pub key: PathBuf,
    /// The CA the other side's certificate must chain to, PEM.
    pub ca: PathBuf,
}

impl MtlsFiles {
    /// All three or none: `--tls-cert`, `--tls-key` and `--tls-client-ca`
    /// (or `--tls-ca`) go together.
    pub fn from_flags(
        cert: Option<PathBuf>,
        key: Option<PathBuf>,
        ca: Option<PathBuf>,
    ) -> Result<Option<Self>, String> {
        match (cert, key, ca) {
            (None, None, None) => Ok(None),
            (Some(cert), Some(key), Some(ca)) => Ok(Some(Self { cert, key, ca })),
            _ => Err("a TLS certificate, its key and the peer's CA go together".into()),
        }
    }
}

fn pem_error(path: &Path, e: impl std::fmt::Display) -> MtlsError {
    MtlsError::Pem {
        path: path.to_owned(),
        message: e.to_string(),
    }
}

fn certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, MtlsError> {
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(|e| pem_error(path, e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| pem_error(path, e))?;
    if certs.is_empty() {
        return Err(pem_error(path, "no certificate in it"));
    }
    Ok(certs)
}

fn key(path: &Path) -> Result<PrivateKeyDer<'static>, MtlsError> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| pem_error(path, e))
}

fn roots(path: &Path) -> Result<Arc<RootCertStore>, MtlsError> {
    let mut store = RootCertStore::empty();
    for cert in certs(path)? {
        store.add(cert).map_err(|e| pem_error(path, e))?;
    }
    Ok(Arc::new(store))
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// A listener's TLS: its certificate, and only clients whose certificate
/// chains to `files.ca`. TLS 1.2 and 1.3 only.
pub fn server_config(files: &MtlsFiles) -> Result<Arc<rustls::ServerConfig>, MtlsError> {
    let verifier = WebPkiClientVerifier::builder_with_provider(roots(&files.ca)?, provider())
        .build()
        .map_err(|e| MtlsError::Tls(e.to_string()))?;
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| MtlsError::Tls(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs(&files.cert)?, key(&files.key)?)
        .map_err(|e| MtlsError::Tls(e.to_string()))?;
    Ok(Arc::new(config))
}

/// A client's TLS: its certificate for the host, and only a host whose
/// certificate chains to `files.ca`.
pub fn client_config(files: &MtlsFiles) -> Result<Arc<rustls::ClientConfig>, MtlsError> {
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| MtlsError::Tls(e.to_string()))?
        .with_root_certificates(roots(&files.ca)?)
        .with_client_auth_cert(certs(&files.cert)?, key(&files.key)?)
        .map_err(|e| MtlsError::Tls(e.to_string()))?;
    Ok(Arc::new(config))
}

/// [`server_config`] as a Tokio acceptor.
pub fn acceptor(files: &MtlsFiles) -> Result<TlsAcceptor, MtlsError> {
    Ok(TlsAcceptor::from(server_config(files)?))
}

/// [`client_config`] as a Tokio connector.
pub fn connector(files: &MtlsFiles) -> Result<TlsConnector, MtlsError> {
    Ok(TlsConnector::from(client_config(files)?))
}

/// The name a client checks the host's certificate against.
pub fn server_name(host: &str) -> Result<ServerName<'static>, MtlsError> {
    ServerName::try_from(host.to_owned()).map_err(|e| MtlsError::Tls(e.to_string()))
}
