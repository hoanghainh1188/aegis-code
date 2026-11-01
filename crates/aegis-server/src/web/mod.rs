//! The dashboard and the reviewer's pages (ADR 0003 §5, #28): HTML
//! rendered here with `askama`, `htmx` for the filters, no SPA. Data comes
//! from `aegis-store` directly, not through the API.
//!
//! Scripts are served from `/static`, never from a CDN, and the CSP allows
//! only them, by nonce; nothing is inline. Every page needs a session
//! (#29) and shows only the repos its user has a role in; see [`guard`].

mod actions;
mod admin;
mod dashboard;
mod guard;
mod jobs;
mod patch;
mod repo_admin;
mod repo_form;
mod review;
mod tokens;
mod view;

use aegis_store::{Job, JobId, RepoRole, Store, StoreError};
use askama::Template;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router, middleware};

use crate::auth::{Access, Auth, oidc};

pub use dashboard::{Dashboard, Group, dashboard_kpis, dashboard_kpis_within};
pub use guard::{Me, Nonce, SessionToken};
pub use repo_admin::RepoAdmin;
pub use review::{REVIEW_MAX_AGE_MS, opened_at_signature, review_minutes};

const HTMX: &[u8] = include_bytes!("../../static/htmx-2.0.11.min.js");
const CSS: &[u8] = include_bytes!("../../static/app.css");

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
        .route("/admin/repos", get(admin::repos).post(repo_admin::create))
        .route("/admin/repos/new", get(repo_admin::new_form))
        .route("/admin/repos/{id}", get(admin::repo))
        .route(
            "/admin/repos/{id}/edit",
            get(repo_admin::edit_form).post(repo_admin::update),
        )
        .route("/admin/repos/{id}/clone", get(repo_admin::clone_part))
        .route("/admin/repos/{id}/roles", post(admin::grant))
        .route("/admin/repos/{id}/roles/revoke", post(admin::revoke))
        .route("/me/tokens", get(tokens::list).post(tokens::create))
        .route("/me/tokens/{id}/revoke", post(tokens::revoke))
        .route(
            "/static/htmx-2.0.11.min.js",
            get(|| asset(HTMX, "text/javascript")),
        )
        .route("/static/app.css", get(|| asset(CSS, "text/css")))
        .layer(middleware::from_fn_with_state(auth.clone(), guard::guard))
        .layer(Extension(auth))
        .layer(Extension(repos))
}

async fn asset(body: &'static [u8], content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
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
