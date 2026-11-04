//! The proxy itself: HTTP/1.1 on a Unix socket, each call checked by
//! [`crate::policy`] and then passed to the daemon's socket. Upgraded
//! connections (`docker run`'s `attach`) are relayed both ways once the
//! daemon accepts them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{
    CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};

use crate::policy::{self, Policy, Route};

/// A create body is a few kilobytes; anything bigger is not aegis's.
const BODY_LIMIT: usize = 1024 * 1024;

type Body = BoxBody<Bytes, hyper::Error>;

/// The daemon's socket and the policy calls are checked against.
#[derive(Debug, Clone)]
pub struct Proxy {
    pub upstream: PathBuf,
    pub policy: Policy,
}

impl Proxy {
    /// Serves `listener` until the task is dropped.
    pub async fn serve(self, listener: UnixListener) -> std::io::Result<()> {
        let proxy = Arc::new(self);
        loop {
            let (stream, _) = listener.accept().await?;
            let proxy = Arc::clone(&proxy);
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req| {
                    let proxy = Arc::clone(&proxy);
                    async move { Ok::<_, hyper::Error>(proxy.handle(req).await) }
                });
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await
                {
                    tracing::debug!(error = %e, "client connection ended");
                }
            });
        }
    }

    async fn handle(&self, mut req: Request<Incoming>) -> Response<Body> {
        let method = req.method().clone();
        let path = req.uri().path().to_owned();
        let query = req.uri().query().map(str::to_owned);
        let client_upgrade = hyper::upgrade::on(&mut req);
        let (mut parts, body) = req.into_parts();
        let (route, body) = match self.check(&method, &path, query.as_deref(), body).await {
            Ok(checked) => checked,
            Err(reason) => {
                // The body may hold the container's environment: never
                // logged.
                tracing::warn!(%method, %path, %reason, "refused");
                return refusal(&reason);
            }
        };
        tracing::debug!(%method, %path, "allowed");
        // The body was read whole and is sent with its length; only
        // `attach` may ask to upgrade the connection.
        for header in [TRANSFER_ENCODING, CONTENT_LENGTH, TE, TRAILER] {
            parts.headers.remove(header);
        }
        if !matches!(route, Route::Attach(_)) {
            parts.headers.remove(CONNECTION);
            parts.headers.remove(UPGRADE);
        }
        parts.headers.insert(CONTENT_LENGTH, body.len().into());
        let req = Request::from_parts(parts, full(body));
        match self.forward(req).await {
            Ok(mut resp) => {
                if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
                    if !matches!(route, Route::Attach(_)) {
                        return error_response(StatusCode::BAD_GATEWAY, "unexpected upgrade");
                    }
                    let upstream_upgrade = hyper::upgrade::on(&mut resp);
                    tokio::spawn(relay(client_upgrade, upstream_upgrade));
                }
                resp.map(BodyExt::boxed)
            }
            Err(e) => {
                tracing::error!(%method, %path, error = %e, "the daemon did not answer");
                error_response(StatusCode::BAD_GATEWAY, &format!("aegis-docker-proxy: {e}"))
            }
        }
    }

    /// The call and the body to send for it, or why it is refused. A body
    /// to check is rebuilt from what was checked; any other must be empty.
    async fn check(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        body: Incoming,
    ) -> Result<(Route, Bytes), String> {
        let route = policy::route(method.as_str(), path, query)?;
        let limit = if route.has_body() { BODY_LIMIT } else { 0 };
        let bytes = Limited::new(body, limit)
            .collect()
            .await
            .map_err(|e| {
                if limit == 0 {
                    "this call takes no body".to_owned()
                } else {
                    format!("body: {e}")
                }
            })?
            .to_bytes();
        let rebuilt = if route.has_body() {
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| format!("body: {e}"))?;
            let out = match &route {
                Route::CreateContainer(_) => self.policy.create(&value)?,
                Route::CreateNetwork => self.policy.network_create(&value)?,
                Route::ConnectNetwork(net) => self.policy.connect(net, &value)?,
                _ => unreachable!("has_body"),
            };
            Bytes::from(serde_json::to_vec(&out).map_err(|e| e.to_string())?)
        } else {
            Bytes::new()
        };
        match &route {
            Route::Network(net) => self.policy.check_network(net)?,
            Route::Container(id) | Route::Attach(id) => self.check_container(id).await?,
            _ => {}
        }
        Ok((route, rebuilt))
    }

    /// A call on container `id` (a name or an ID): allowed when the
    /// daemon says the container is one of aegis's.
    async fn check_container(&self, id: &str) -> Result<(), String> {
        if policy::is_aegis_container(id) {
            return Ok(());
        }
        if !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("container {id:?} is not one of aegis's"));
        }
        let req = Request::get(format!("/containers/{id}/json"))
            .header(CONTENT_LENGTH, 0)
            .body(full(Bytes::new()))
            .map_err(|e| e.to_string())?;
        let resp = self.forward(req).await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("container {id:?}: {}", resp.status()));
        }
        let bytes = Limited::new(resp.into_body(), BODY_LIMIT)
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        let info: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let name = info.get("Name").and_then(Value::as_str).unwrap_or("");
        if policy::is_aegis_container(name) {
            Ok(())
        } else {
            Err(format!("container {name:?} is not one of aegis's"))
        }
    }

    /// Sends `req` to the daemon on a connection of its own.
    async fn forward(&self, mut req: Request<Body>) -> Result<Response<Incoming>, ForwardError> {
        let stream = UnixStream::connect(&self.upstream)
            .await
            .map_err(|e| ForwardError::Connect(self.upstream.clone(), e))?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(ForwardError::Http)?;
        tokio::spawn(async move {
            if let Err(e) = conn.with_upgrades().await {
                tracing::debug!(error = %e, "daemon connection ended");
            }
        });
        if !req.headers().contains_key(HOST) {
            req.headers_mut()
                .insert(HOST, hyper::header::HeaderValue::from_static("docker"));
        }
        sender.send_request(req).await.map_err(ForwardError::Http)
    }
}

#[derive(Debug)]
enum ForwardError {
    Connect(PathBuf, std::io::Error),
    Http(hyper::Error),
}

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(path, e) => write!(f, "cannot reach {}: {e}", path.display()),
            Self::Http(e) => write!(f, "{e}"),
        }
    }
}

/// Copies bytes both ways between the client and the daemon after both
/// upgraded the connection.
async fn relay(client: hyper::upgrade::OnUpgrade, upstream: hyper::upgrade::OnUpgrade) {
    let (client, upstream) = match tokio::try_join!(client, upstream) {
        Ok(both) => both,
        Err(e) => {
            tracing::debug!(error = %e, "upgrade failed");
            return;
        }
    };
    let (mut client, mut upstream) = (TokioIo::new(client), TokioIo::new(upstream));
    if let Err(e) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        tracing::debug!(error = %e, "upgraded connection ended");
    }
}

/// Docker's error shape, so the client prints the reason.
fn refusal(reason: &str) -> Response<Body> {
    error_response(
        StatusCode::FORBIDDEN,
        &format!("aegis-docker-proxy refused the call: {reason}"),
    )
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    let body = serde_json::json!({ "message": message }).to_string();
    let len = body.len();
    let mut resp = Response::new(full(Bytes::from(body)));
    *resp.status_mut() = status;
    let headers = resp.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(CONTENT_LENGTH, len.into());
    resp
}

fn full(bytes: Bytes) -> Body {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

/// Binds `path`, replacing a socket left by an earlier run, readable and
/// writable by its owner only: the aegis user.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket()) {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
