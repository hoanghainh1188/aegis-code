//! What every page goes through: a known `Host`, a same-origin check and
//! the CSRF token for forms, the session, then the headers, CSP with a
//! fresh nonce first.
//!
//! Pages need a session; without one a page sends the browser to sign in
//! and a form is refused. `/auth/*` (but logout) and `/static/*` need none.

use aegis_store::ActorKind;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use openidconnect::url::form_urlencoded;
use subtle::ConstantTimeEq;

use crate::auth::oidc::cookie_value;
use crate::auth::{Access, Auth, CSRF_FIELD, CSRF_HEADER, SESSION_COOKIE, token_hash};

/// The largest form the guard reads to find its CSRF token.
const MAX_FORM_BYTES: usize = 64 * 1024;

/// The CSP nonce of one response; templates put it on each `<script>`.
#[derive(Debug, Clone)]
pub struct Nonce(String);

impl Nonce {
    fn fresh() -> Option<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).ok()?;
        Some(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The signed-in person, for the page: their name, and the CSRF token
/// `form_guard.html` puts in every form.
#[derive(Debug, Clone)]
pub struct Me {
    pub name: String,
    pub csrf: String,
    /// The masthead offers the admin pages (#329).
    pub is_admin: bool,
}

/// The hash of the request's session token, for signing out.
#[derive(Debug, Clone)]
pub struct SessionToken(pub aegis_store::TokenHash);

/// The host of a `Host` or `Origin` authority, without its port.
fn host_of(authority: &str) -> &str {
    match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    }
}

/// A browser says where a form came from with `Sec-Fetch-Site` or
/// `Origin`; a tool that sends neither is not a cross-site browser form.
pub fn same_origin(headers: &HeaderMap) -> bool {
    let text = |name| {
        headers
            .get(name)
            .and_then(|v: &HeaderValue| v.to_str().ok())
    };
    if let Some(site) = text(header::HeaderName::from_static("sec-fetch-site")) {
        return site == "same-origin";
    }
    match (text(header::ORIGIN), text(header::HOST)) {
        (Some(origin), Some(host)) => origin
            .split_once("://")
            .is_some_and(|(_, authority)| authority == host),
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn refuse(status: StatusCode, why: &'static str) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], why).into_response()
}

fn needs_session(path: &str) -> bool {
    !(path.starts_with("/static/") || path.starts_with("/auth/") && path != "/auth/logout")
}

/// `/auth/login?next=<this page>`.
fn sign_in_first(request: &Request) -> Response {
    let here = request.uri().path_and_query().map_or("/", |p| p.as_str());
    let next: String = form_urlencoded::byte_serialize(here.as_bytes()).collect();
    Redirect::to(&format!("/auth/login?next={next}")).into_response()
}

/// The answer in place of the page; boxed, as it is the rare path.
type Stop = Box<Response>;

/// The request's CSRF token: the htmx header, else the form field. The
/// body is read and put back for the handler.
async fn csrf_of(request: Request) -> Result<(Request, Option<String>), Stop> {
    if let Some(token) = request
        .headers()
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        let token = token.to_owned();
        return Ok((request, Some(token)));
    }
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, MAX_FORM_BYTES)
        .await
        .map_err(|_| Box::new(refuse(StatusCode::PAYLOAD_TOO_LARGE, "form too large")))?;
    let token = form_urlencoded::parse(&bytes)
        .find(|(k, _)| k == CSRF_FIELD)
        .map(|(_, v)| v.into_owned());
    Ok((Request::from_parts(parts, Body::from(bytes)), token))
}

/// Puts the session's [`Access`], [`Me`] and [`SessionToken`] in the
/// request, or answers in its place.
async fn signed_in(auth: &Auth, request: Request, form: bool) -> Result<Request, Stop> {
    let token = cookie_value(request.headers(), SESSION_COOKIE).map(token_hash);
    let session = match token {
        Some(hash) => auth.store().session(&hash).await,
        None => Ok(None),
    };
    let session = match session {
        Ok(Some(s)) => s,
        Ok(None) if form => {
            return Err(Box::new(refuse(StatusCode::UNAUTHORIZED, "sign in again")));
        }
        Ok(None) => return Err(Box::new(sign_in_first(&request))),
        Err(e) => {
            tracing::error!(error = %e, "web: session");
            return Err(Box::new(refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            )));
        }
    };
    let mut request = request;
    if form {
        let (back, given) = csrf_of(request).await?;
        request = back;
        let ok =
            given.is_some_and(|g| bool::from(g.as_bytes().ct_eq(session.csrf_token.as_bytes())));
        if !ok {
            return Err(Box::new(refuse(
                StatusCode::FORBIDDEN,
                "CSRF token missing or wrong; reload the page",
            )));
        }
    }
    let access = Access::load(auth.store(), &session.user, ActorKind::User)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "web: roles");
            Box::new(refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
        })?;
    let extensions = request.extensions_mut();
    extensions.insert(access);
    extensions.insert(Me {
        name: session.user.name.clone(),
        csrf: session.csrf_token,
        is_admin: session.user.is_admin,
    });
    if let Some(hash) = token {
        extensions.insert(SessionToken(hash));
    }
    Ok(request)
}

pub async fn guard(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(host_of);
    if !host.is_some_and(|h| auth.serves_host(h)) {
        return refuse(StatusCode::FORBIDDEN, "unknown host");
    }
    let form = !matches!(*request.method(), Method::GET | Method::HEAD);
    if form && !same_origin(request.headers()) {
        return refuse(StatusCode::FORBIDDEN, "cross-site form refused");
    }
    let mut request = if needs_session(request.uri().path()) {
        match signed_in(&auth, request, form).await {
            Ok(request) => request,
            Err(response) => return *response,
        }
    } else {
        request
    };
    let Some(nonce) = Nonce::fresh() else {
        tracing::error!("web: no randomness for a CSP nonce");
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    };
    request.extensions_mut().insert(nonce.clone());
    let mut response = next.run(request).await;
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{}'; style-src 'self'; img-src 'self'; \
         connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        nonce.as_str()
    );
    let headers = response.headers_mut();
    if let Ok(csp) = HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    for (name, value) in [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::STRICT_TRANSPORT_SECURITY,
            "max-age=31536000; includeSubDomains",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    header::HeaderName::from_static(k),
                    HeaderValue::from_static(v),
                )
            })
            .collect()
    }

    #[test]
    fn hosts_lose_their_port() {
        assert_eq!(host_of("localhost:8080"), "localhost");
        assert_eq!(host_of("[::1]:8080"), "::1");
        assert_eq!(host_of("aegis.example.com"), "aegis.example.com");
    }

    #[test]
    fn forms_from_other_sites_are_refused() {
        let host = ("host", "localhost:8080");
        assert!(same_origin(&headers(&[
            host,
            ("sec-fetch-site", "same-origin")
        ])));
        assert!(!same_origin(&headers(&[
            host,
            ("sec-fetch-site", "cross-site")
        ])));
        assert!(same_origin(&headers(&[
            host,
            ("origin", "http://localhost:8080")
        ])));
        assert!(!same_origin(&headers(&[
            host,
            ("origin", "http://evil.test")
        ])));
        assert!(same_origin(&headers(&[host])));
    }

    #[test]
    fn only_sign_in_and_assets_need_no_session() {
        for open in ["/auth/login", "/auth/callback", "/static/app.css"] {
            assert!(!needs_session(open), "{open}");
        }
        for closed in ["/", "/jobs", "/jobs/1/review", "/auth/logout"] {
            assert!(needs_session(closed), "{closed}");
        }
    }

    #[test]
    fn nonces_differ() {
        let (a, b) = (Nonce::fresh().unwrap(), Nonce::fresh().unwrap());
        assert_eq!(a.as_str().len(), 32);
        assert_ne!(a.as_str(), b.as_str());
    }
}
