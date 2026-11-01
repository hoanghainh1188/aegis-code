//! The dashboard and the reviewer's pages (ADR 0003 §5, #28): HTML
//! rendered here with `askama`, `htmx` for the filters, no SPA. Data comes
//! from `aegis-store` directly, not through the API.
//!
//! Scripts are served from `/static`, never from a CDN, and the CSP allows
//! only them, by nonce; nothing is inline. Every page needs a session
//! (#29) and shows only the repos its user has a role in; see [`guard`].

mod actions;
mod admin;
mod ai;
mod audit;
mod connection;
mod dashboard;
mod guard;
mod jobs;
mod patch;
mod repo_admin;
mod repo_form;
mod review;
mod service_tokens;
mod tokens;
mod users;
mod view;

use aegis_store::{Job, JobId, RepoRole, Store, StoreError};
use askama::Template;
use axum::extract::Query;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router, middleware};
use serde::Deserialize;
use std::sync::LazyLock;

use crate::auth::{Access, Auth, oidc};

pub use dashboard::{Dashboard, Group, dashboard_kpis, dashboard_kpis_within};
pub use guard::{Me, Nonce, SessionToken};
pub use repo_admin::RepoAdmin;
pub use review::{REVIEW_MAX_AGE_MS, opened_at_signature, review_minutes};

const HTMX: &[u8] = include_bytes!("../../static/htmx-2.0.11.min.js");
const CSS: &[u8] = include_bytes!("../../static/app.css");

/// For a URL that names its content: htmx by its version in the file name,
/// the stylesheet by its hash in `?v=`.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// For the stylesheet asked for without the current hash: an old page or a
/// bookmark gets today's CSS, and the browser asks again next time.
const REVALIDATE: &str = "no-cache";

/// Sixteen hex digits of FNV-1a over `bytes`: enough to tell one build's
/// asset from the next, not a security hash.
fn asset_version(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

fn css_version() -> &'static str {
    static VERSION: LazyLock<String> = LazyLock::new(|| asset_version(CSS));
    &VERSION
}

/// The stylesheet's URL in every page: a new build's CSS is a new URL, so
/// the browser never shows a new page with yesterday's styles (#375).
pub(crate) fn css_href() -> &'static str {
    static HREF: LazyLock<String> =
        LazyLock::new(|| format!("/static/app.css?v={}", css_version()));
    &HREF
}

fn css_cache(version: Option<&str>) -> &'static str {
    if version == Some(css_version()) {
        IMMUTABLE
    } else {
        REVALIDATE
    }
}

#[derive(Deserialize)]
struct AssetQuery {
    v: Option<String>,
}

async fn css(Query(q): Query<AssetQuery>) -> Response {
    asset(CSS, "text/css", css_cache(q.v.as_deref()))
}

/// The pages, under the API's state; `api::router` merges them in.
pub fn router(auth: Auth, repos: Option<RepoAdmin>) -> Router<Store> {
    Router::new()
        .route("/auth/login", get(oidc::login))
        .route("/auth/callback", get(oidc::callback))
        .route("/auth/logout", post(oidc::logout))
        .route("/auth/signed-out", get(oidc::signed_out))
        .route("/", get(dashboard::page))
        .route("/jobs", get(jobs::list).post(actions::create))
        .route("/jobs/{id}", get(jobs::detail))
        .route("/jobs/{id}/head", get(jobs::head))
        .route("/jobs/{id}/cancel", post(actions::cancel))
        .route("/jobs/{id}/retry", post(actions::retry))
        .route("/jobs/{id}/review", get(review::form).post(review::submit))
        .route("/admin/ai", get(ai::show))
        .route("/admin/ai/{mode}/check", post(ai::check))
        .route("/admin/audit", get(audit::list))
        .route("/admin/users", get(users::list).post(users::add_admin))
        .route("/admin/users/{id}", get(users::detail))
        .route("/admin/users/{id}/admin", post(users::set_admin))
        .route("/admin/users/{id}/sign-out", post(users::sign_out))
        .route("/admin/services", post(service_tokens::create))
        .route("/admin/users/{id}/tokens", post(service_tokens::make))
        .route(
            "/admin/users/{id}/tokens/revoke-all",
            post(service_tokens::revoke_all),
        )
        .route(
            "/admin/users/{id}/tokens/{token}/revoke",
            post(service_tokens::revoke),
        )
        .route("/admin/repos", get(admin::repos).post(repo_admin::create))
        .route("/admin/repos/new", get(repo_admin::new_form))
        .route("/admin/repos/{id}", get(admin::repo))
        .route(
            "/admin/repos/{id}/edit",
            get(repo_admin::edit_form).post(repo_admin::update),
        )
        .route("/admin/repos/{id}/clone", get(repo_admin::clone_part))
        .route(
            "/admin/repos/{id}/connection",
            get(connection::show).post(connection::check),
        )
        .route("/admin/repos/{id}/roles", post(admin::grant))
        .route("/admin/repos/{id}/roles/revoke", post(admin::revoke))
        .route("/me/tokens", get(tokens::list).post(tokens::create))
        .route("/me/tokens/{id}/revoke", post(tokens::revoke))
        .route(
            "/static/htmx-2.0.11.min.js",
            get(|| async { asset(HTMX, "text/javascript", IMMUTABLE) }),
        )
        .route("/static/app.css", get(css))
        .layer(middleware::from_fn_with_state(auth.clone(), guard::guard))
        .layer(Extension(auth))
        .layer(Extension(repos))
}

fn asset(body: &'static [u8], content_type: &'static str, cache: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}

/// An error as a page; database details stay in the log.
#[derive(Debug)]
pub struct WebError {
    status: StatusCode,
    message: String,
}

impl WebError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub(crate) fn not_found(what: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("{} not found", what.into()))
    }
}

impl From<StoreError> for WebError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(what) => Self::not_found(what),
            // What the store refuses, said as the API says it (#309).
            StoreError::Conflict(why) | StoreError::BudgetSpent(why) => {
                Self::new(StatusCode::CONFLICT, why)
            }
            StoreError::Invalid { field, message } => {
                Self::new(StatusCode::BAD_REQUEST, format!("{field}: {message}"))
            }
            other => {
                tracing::error!(error = %other, "web: store");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
        }
    }
}

impl From<aegis_ledger::LedgerError> for WebError {
    fn from(e: aegis_ledger::LedgerError) -> Self {
        tracing::error!(error = %e, "web: ledger");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    message: &'a str,
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        // An error page loads no script, so it needs no nonce.
        let page = ErrorPage {
            message: &self.message,
        };
        match page.render() {
            Ok(html) => (self.status, Html(html)).into_response(),
            Err(_) => (self.status, self.message).into_response(),
        }
    }
}

type WebResult = Result<Response, WebError>;

/// Job `id` when `access` may do what `needed` allows in its repo. A job
/// of a repo it cannot see is "not found", as if it did not exist.
async fn job_for(
    store: &Store,
    access: &Access,
    id: i64,
    needed: RepoRole,
) -> Result<Job, WebError> {
    let job = store
        .job(JobId(id))
        .await?
        .filter(|j| access.can(j.repo_id, RepoRole::Viewer))
        .ok_or_else(|| WebError::not_found(format!("job {id}")))?;
    if !access.can(job.repo_id, needed) {
        return Err(WebError::new(
            StatusCode::FORBIDDEN,
            format!("you need role {} in repo of job {id}", needed.as_str()),
        ));
    }
    Ok(job)
}

fn render(page: &impl Template) -> WebResult {
    page.render()
        .map(|html| Html(html).into_response())
        .map_err(|e| {
            tracing::error!(error = %e, "web: template");
            WebError::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_version_follows_the_content() {
        let v = asset_version(b"a { color: red }");
        assert_eq!(v.len(), 16);
        assert!(v.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(v, asset_version(b"a { color: red }"));
        assert_ne!(v, asset_version(b"a { color: blue }"));
    }

    #[test]
    fn pages_link_the_stylesheet_by_its_version() {
        let href = format!("/static/app.css?v={}", asset_version(CSS));
        assert_eq!(css_href(), href);
        let page = ErrorPage { message: "nope" }.render().unwrap();
        assert!(page.contains(&format!(r#"href="{href}""#)), "{page}");
    }

    #[test]
    fn only_the_current_version_is_kept_for_good() {
        assert_eq!(css_cache(Some(&asset_version(CSS))), IMMUTABLE);
        assert_eq!(css_cache(Some("0000000000000000")), REVALIDATE);
        assert_eq!(css_cache(None), REVALIDATE);
    }
}
