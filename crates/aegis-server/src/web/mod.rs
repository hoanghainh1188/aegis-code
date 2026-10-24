//! The dashboard and the reviewer's pages (ADR 0003 §5, #28): HTML
//! rendered here with `askama`, `htmx` for the filters, no SPA. Data comes
//! from `aegis-store` directly, not through the API.
//!
//! Scripts are served from `/static`, never from a CDN, and the CSP allows
//! only them, by nonce; nothing is inline. No login yet (#29): every page
//! answers only to a loopback `Host`, which also stops DNS rebinding.

mod dashboard;
mod guard;
mod jobs;
mod review;
mod view;

use aegis_store::{Store, StoreError};
use askama::Template;
use axum::Router;
use axum::http::{StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

pub use dashboard::{Dashboard, Group, dashboard_kpis};
pub use guard::Nonce;
pub use review::{REVIEW_MAX_AGE_MS, review_minutes};

const HTMX: &[u8] = include_bytes!("../../static/htmx-2.0.11.min.js");
const CSS: &[u8] = include_bytes!("../../static/app.css");

/// The pages, under the API's state; `api::router` merges them in.
pub fn router() -> Router<Store> {
    Router::new()
        .route("/", get(dashboard::page))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::detail))
        .route("/jobs/{id}/review", get(review::form).post(review::submit))
        .route(
            "/static/htmx-2.0.11.min.js",
            get(|| asset(HTMX, "text/javascript")),
        )
        .route("/static/app.css", get(|| asset(CSS, "text/css")))
        .layer(middleware::from_fn(guard::guard))
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
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn not_found(what: impl Into<String>) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            format!("{} không tồn tại", what.into()),
        )
    }
}

impl From<StoreError> for WebError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(what) => Self::not_found(what),
            other => {
                tracing::error!(error = %other, "web: store");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Lỗi nội bộ")
            }
        }
    }
}

impl From<aegis_ledger::LedgerError> for WebError {
    fn from(e: aegis_ledger::LedgerError) -> Self {
        tracing::error!(error = %e, "web: ledger");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Lỗi nội bộ")
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

fn render(page: &impl Template) -> WebResult {
    page.render()
        .map(|html| Html(html).into_response())
        .map_err(|e| {
            tracing::error!(error = %e, "web: template");
            WebError::new(StatusCode::INTERNAL_SERVER_ERROR, "Lỗi nội bộ")
        })
}
