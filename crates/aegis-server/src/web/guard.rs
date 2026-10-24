//! What every page goes through: a loopback `Host`, a same-origin check
//! for forms, and the headers, CSP with a fresh nonce first.
//!
//! The form check is where #29 adds its CSRF token: the forms already
//! include `form_guard.html`, which will carry the token field.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

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

/// The host of a `Host` or `Origin` authority, without its port.
fn host_of(authority: &str) -> &str {
    match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    }
}

fn is_loopback(headers: &HeaderMap) -> bool {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    host.map(host_of)
        .is_some_and(|h| matches!(h, "localhost" | "127.0.0.1" | "::1"))
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

pub async fn guard(mut request: Request, next: Next) -> Response {
    if !is_loopback(request.headers()) {
        return refuse(
            StatusCode::FORBIDDEN,
            "dashboard answers on loopback only (#29)",
        );
    }
    let form = !matches!(*request.method(), Method::GET | Method::HEAD);
    if form && !same_origin(request.headers()) {
        return refuse(StatusCode::FORBIDDEN, "cross-site form refused");
    }
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
    fn only_loopback_hosts_pass() {
        for host in ["localhost:8080", "127.0.0.1", "[::1]:8080"] {
            assert!(is_loopback(&headers(&[("host", host)])), "{host}");
        }
        for host in ["example.com", "127.0.0.1.evil.test", "10.0.0.1:8080"] {
            assert!(!is_loopback(&headers(&[("host", host)])), "{host}");
        }
        assert!(!is_loopback(&HeaderMap::new()));
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
    fn nonces_differ() {
        let (a, b) = (Nonce::fresh().unwrap(), Nonce::fresh().unwrap());
        assert_eq!(a.as_str().len(), 32);
        assert_ne!(a.as_str(), b.as_str());
    }
}
