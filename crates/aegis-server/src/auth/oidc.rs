//! Signing in through the company SSO: OpenID Connect, authorization code
//! with PKCE (`openidconnect`).
//!
//! `/auth/login` sends the browser to the SSO with a fresh state, nonce
//! and PKCE verifier, kept in Postgres until it comes back; the browser
//! holds only a random id in a short `HttpOnly` cookie (#174), and the
//! callback takes the row, so it serves once. `/auth/callback` checks the
//! state, trades the code
//! for an ID token, checks the token (signature by the SSO's keys, issuer,
//! audience, nonce, expiry), then opens a session in Postgres. The
//! provider's discovery and keys are read at the first sign-in and kept,
//! and read again when an ID token's signature does not match them (the
//! SSO rotated its keys).

use std::time::Duration;

use aegis_secrets::Secrets;
use aegis_store::{ActorKind, PendingLogin, Store};
use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, HttpRequest, HttpResponse, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use super::{Access, Auth, OIDC_CLIENT_SECRET, SESSION_COOKIE, random_token, token_hash};
use crate::config::{AuthSection, OidcSection};
use crate::web::{SessionToken, WebError};

const LOGIN_COOKIE: &str = "__Host-aegis_login";
/// From `/auth/login` to `/auth/callback`.
const LOGIN_TTL_S: i64 = 600;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_NEXT_LEN: usize = 512;

/// The SSO this server signs people in with.
pub(crate) struct Oidc {
    issuer: IssuerUrl,
    client_id: ClientId,
    /// Holds the client secret, read at each sign-in so it may rotate.
    secrets: Secrets,
    redirect: RedirectUrl,
    http: reqwest::Client,
    metadata: RwLock<Option<CoreProviderMetadata>>,
}

impl Oidc {
    pub(crate) fn new(
        oidc: &OidcSection,
        auth: &AuthSection,
        secrets: Secrets,
    ) -> Result<Self, String> {
        let issuer =
            IssuerUrl::new(oidc.issuer.clone()).map_err(|e| format!("auth.oidc.issuer: {e}"))?;
        let public = auth
            .public_url
            .as_ref()
            .ok_or("auth: oidc needs public_url")?;
        let redirect = public
            .join("/auth/callback")
            .map_err(|e| format!("auth.public_url: {e}"))?;
        // No redirects: the SSO's endpoints are taken as discovered.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| format!("auth.oidc: HTTP client: {e}"))?;
        Ok(Self {
            issuer,
            client_id: ClientId::new(oidc.client_id.clone()),
            secrets,
            redirect: RedirectUrl::from_url(redirect),
            http,
            metadata: RwLock::new(None),
        })
    }

    /// The provider's metadata and keys: kept, or read anew when `fresh`.
    async fn metadata(&self, fresh: bool) -> Result<CoreProviderMetadata, String> {
        if !fresh && let Some(kept) = self.metadata.read().await.as_ref() {
            return Ok(kept.clone());
        }
        let found = CoreProviderMetadata::discover_async(self.issuer.clone(), &|r| {
            send(self.http.clone(), r)
        })
        .await
        .map_err(|e| format!("discovery: {e}"))?;
        *self.metadata.write().await = Some(found.clone());
        Ok(found)
    }

    fn client(&self, metadata: CoreProviderMetadata) -> SsoClient {
        let secret = self
            .secrets
            .get(OIDC_CLIENT_SECRET)
            .map(|s| ClientSecret::new(s.expose().to_owned()));
        CoreClient::from_provider_metadata(metadata, self.client_id.clone(), secret)
            .set_redirect_uri(self.redirect.clone())
    }
}

type SsoClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// `openidconnect`'s HTTP through the workspace's `reqwest`.
async fn send(http: reqwest::Client, request: HttpRequest) -> Result<HttpResponse, reqwest::Error> {
    let (parts, body) = request.into_parts();
    let response = http
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await?.to_vec();
    let mut out = HttpResponse::new(body);
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    Ok(out)
}

fn cookie(name: &str, value: &str, max_age_s: i64) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={max_age_s}"
    ))
    .ok()
}

pub(crate) fn clear_cookie(name: &str) -> Option<HeaderValue> {
    cookie(name, "", 0)
}

/// The value of cookie `name` in the request.
pub(crate) fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(k, v)| (k == name).then_some(v))
}

/// A path on this site, or `/`: never another site to send people to.
fn local_path(next: Option<&str>) -> String {
    match next {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && p.len() <= MAX_NEXT_LEN
                && !p.chars().any(char::is_control) =>
        {
            p.to_owned()
        }
        _ => "/".into(),
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage<'a> {
    message: &'a str,
    retry: bool,
}

fn login_page(status: StatusCode, message: &str, retry: bool) -> Response {
    match (LoginPage { message, retry }).render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(_) => (status, message.to_owned()).into_response(),
    }
}

fn internal(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "sign-in: {what}");
    login_page(
        StatusCode::BAD_GATEWAY,
        "Không đăng nhập được qua SSO; thử lại sau.",
        true,
    )
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

pub async fn login(Extension(auth): Extension<Auth>, Query(q): Query<LoginQuery>) -> Response {
    let Some(oidc) = auth.oidc() else {
        return login_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Chưa cấu hình SSO ([auth.oidc] trong server.toml) nên chưa đăng nhập được.",
            false,
        );
    };
    let client = match oidc.metadata(false).await {
        Ok(m) => oidc.client(m),
        Err(e) => return internal("discovery", e),
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".into()))
        .add_scope(Scope::new("profile".into()))
        .set_pkce_challenge(challenge)
        .url();
    let pending = PendingLogin {
        state: state.secret().clone(),
        nonce: nonce.secret().clone(),
        verifier: verifier.secret().clone(),
        next: local_path(q.next.as_deref()),
    };
    let Ok(id) = random_token() else {
        return internal("state", "no randomness");
    };
    let expires = chrono::Utc::now() + chrono::Duration::seconds(LOGIN_TTL_S);
    if let Err(e) = auth
        .store()
        .start_login(&token_hash(&id), &pending, expires)
        .await
    {
        return internal("state", e);
    }
    let mut response = Redirect::to(url.as_str()).into_response();
    if let Some(c) = cookie(LOGIN_COOKIE, &id, LOGIN_TTL_S) {
        response.headers_mut().insert(header::SET_COOKIE, c);
    }
    response
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn refused(message: &str) -> Response {
    login_page(StatusCode::FORBIDDEN, message, true)
}

/// Whatever the outcome, the pending sign-in is spent.
pub async fn callback(
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let mut response = finish_sign_in(&auth, &headers, q).await;
    if let Some(c) = clear_cookie(LOGIN_COOKIE) {
        response.headers_mut().append(header::SET_COOKIE, c);
    }
    response
}

async fn finish_sign_in(auth: &Auth, headers: &HeaderMap, q: CallbackQuery) -> Response {
    let Some(oidc) = auth.oidc() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Taken before anything else, so even a refused callback spends it.
    let pending = match cookie_value(headers, LOGIN_COOKIE) {
        Some(id) => match auth.store().take_login(&token_hash(id)).await {
            Ok(p) => p,
            Err(e) => return internal("state", e),
        },
        None => None,
    };
    if q.error.is_some() {
        return refused("SSO không cho đăng nhập.");
    }
    let (Some(pending), Some(code), Some(state)) = (pending, q.code, q.state) else {
        return refused("Phiên đăng nhập đã hết hạn hoặc không hợp lệ.");
    };
    if !bool::from(state.as_bytes().ct_eq(pending.state.as_bytes())) {
        return refused("Phiên đăng nhập không khớp.");
    }
    let client = match oidc.metadata(false).await {
        Ok(m) => oidc.client(m),
        Err(e) => return internal("discovery", e),
    };
    let request = match client.exchange_code(AuthorizationCode::new(code)) {
        Ok(r) => r,
        Err(e) => return internal("no token endpoint", e),
    };
    let token = match request
        .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
        .request_async(&|r| send(oidc.http.clone(), r))
        .await
    {
        Ok(t) => t,
        Err(e) => return internal("code exchange", e),
    };
    let Some(id_token) = token.id_token() else {
        return internal("code exchange", "no ID token");
    };
    let nonce = Nonce::new(pending.nonce);
    let mut claims = id_token.claims(&client.id_token_verifier(), &nonce);
    if claims.is_err() {
        // Perhaps signed with a key newer than those kept.
        if let Ok(m) = oidc.metadata(true).await {
            claims = id_token.claims(&oidc.client(m).id_token_verifier(), &nonce);
        }
    }
    let claims = match claims {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "sign-in: ID token refused");
            return refused("SSO trả về thông tin đăng nhập không hợp lệ.");
        }
    };
    let Some(email) = claims.email() else {
        return refused("Tài khoản SSO không có e-mail.");
    };
    // Only an e-mail the SSO says it verified may take over roles granted
    // by e-mail before the first sign-in.
    let verified = claims.email_verified() == Some(true);
    let signed_in = auth
        .store()
        .sign_in(claims.subject().as_str(), email.as_str(), verified)
        .await;
    let user = match signed_in {
        Ok(user) => user,
        Err(aegis_store::StoreError::Conflict(e)) => {
            tracing::warn!(error = %e, "sign-in: e-mail taken");
            return refused("E-mail này đã gắn với một tài khoản khác, hoặc SSO chưa xác minh nó.");
        }
        Err(e) => return internal("user", e),
    };
    start_session(auth, &user, &pending.next).await
}

/// A new session for `user`, then on to `next`.
async fn start_session(auth: &Auth, user: &aegis_store::User, next: &str) -> Response {
    let (Ok(token), Ok(csrf)) = (random_token(), random_token()) else {
        return internal("session", "no randomness");
    };
    let ttl = auth.session_ttl();
    let expires = chrono::Utc::now()
        + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::hours(12));
    if let Err(e) = auth
        .store()
        .start_session(user, &token_hash(&token), &csrf, expires)
        .await
    {
        return internal("session", e);
    }
    tracing::info!(user = %user.name, "signed in");
    let mut response = Redirect::to(next).into_response();
    let headers = response.headers_mut();
    let max_age = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);
    if let Some(c) = cookie(SESSION_COOKIE, &token, max_age) {
        headers.append(header::SET_COOKIE, c);
    }
    response
}

/// Ends the session; the guard has checked it and its CSRF token.
pub async fn logout(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Extension(SessionToken(token)): Extension<SessionToken>,
) -> Result<Response, WebError> {
    debug_assert_eq!(access.actor.kind, ActorKind::User);
    store.end_session(&token, &access.actor).await?;
    let mut response = Redirect::to("/auth/signed-out").into_response();
    if let Some(c) = clear_cookie(SESSION_COOKIE) {
        response.headers_mut().insert(header::SET_COOKIE, c);
    }
    Ok(response)
}

pub async fn signed_out() -> Response {
    login_page(StatusCode::OK, "Đã đăng xuất.", true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_paths_are_followed_after_sign_in() {
        assert_eq!(
            local_path(Some("/jobs?status=failed")),
            "/jobs?status=failed"
        );
        for bad in [
            "https://evil.test/",
            "//evil.test",
            "/\\evil.test",
            "jobs",
            "/a\nb",
        ] {
            assert_eq!(local_path(Some(bad)), "/", "{bad}");
        }
        assert_eq!(local_path(None), "/");
    }

    #[test]
    fn cookies_are_read_by_name() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; __Host-aegis_session=t"),
        );
        headers.append(header::COOKIE, HeaderValue::from_static("b=2"));
        assert_eq!(cookie_value(&headers, SESSION_COOKIE), Some("t"));
        assert_eq!(cookie_value(&headers, "b"), Some("2"));
        assert_eq!(cookie_value(&headers, "c"), None);
        let set = cookie(SESSION_COOKIE, "t", 60).unwrap();
        let set = set.to_str().unwrap();
        for part in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
            assert!(set.contains(part), "{set}");
        }
    }
}
