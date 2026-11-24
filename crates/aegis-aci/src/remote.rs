//! The ACI served to a remote worker (#664): MCP over Streamable HTTP, on
//! a TCP listener that takes only clients whose certificate chains to the
//! worker CA (mutual TLS, `aegis_mtls`). The tools are the same as over
//! stdio and work on this host's workspace; the worker keeps the agent
//! loop. Each MCP session gets the same server: one ACI per workspace.

use std::net::SocketAddr;
use std::sync::Arc;

use aegis_mtls::TlsAcceptor;
use http_body_util::BodyExt;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

use crate::AciServer;

/// Where the MCP endpoint is: `https://<host>:<port>/mcp`.
pub const MCP_PATH: &str = "/mcp";

/// Serves `server` on `listener` over mutual TLS until the task is
/// dropped. `allowed_hosts` are the names the worker dials the host by:
/// a request with another `Host` is refused (as rmcp does against DNS
/// rebinding).
pub async fn serve_mtls(
    server: AciServer,
    listener: TcpListener,
    tls: TlsAcceptor,
    allowed_hosts: Vec<String>,
) -> std::io::Result<()> {
    let config = StreamableHttpServerConfig::default().with_allowed_hosts(allowed_hosts);
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    loop {
        let (tcp, peer) = listener.accept().await?;
        let (tls, service) = (tls.clone(), service.clone());
        tokio::spawn(async move { connection(tcp, peer, tls, service).await });
    }
}

async fn connection(
    tcp: tokio::net::TcpStream,
    peer: SocketAddr,
    tls: TlsAcceptor,
    service: StreamableHttpService<AciServer, LocalSessionManager>,
) {
    let stream = match tls.accept(tcp).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("aegis-aci: {peer}: TLS handshake refused: {e}");
            return;
        }
    };
    let routed = tower::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let mut service = service.clone();
        async move {
            if req.uri().path() != MCP_PATH {
                let mut not_found = hyper::Response::new(BodyExt::boxed(
                    http_body_util::Full::new(bytes::Bytes::from_static(b"not found")),
                ));
                *not_found.status_mut() = hyper::StatusCode::NOT_FOUND;
                return Ok::<_, std::convert::Infallible>(not_found);
            }
            tower::Service::call(&mut service, req).await
        }
    });
    if let Err(e) = auto::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(routed))
        .await
    {
        eprintln!("aegis-aci: {peer}: connection ended: {e}");
    }
}
