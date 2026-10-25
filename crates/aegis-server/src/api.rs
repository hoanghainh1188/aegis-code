//! The internal API for connectors and services (ADR 0003 §4): under
//! `/api/v1`, every answer the same envelope, `{data, error, meta}`.
//!
//! Every route but `health` needs a service token and is rate limited
//! ([`crate::auth::api`]). A service sees and changes only the repos it has
//! a role in: a job of another repo is "not found". Writes are audited.
//!
//! A repo's monthly budget (#31) is read at `GET /api/v1/repos/{id}/budget`
//! and its cap set with `PUT` (maintainer); a job asked for when it is
//! spent is refused with `409 budget_spent` and the reason.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aegis_ledger::IssueId;
use aegis_store::{
    Job, JobFilter, JobId, JobSource, JobStatus, NewJob, Page, PageOf, RepoBudget, RepoId,
    RepoRole, Run, Store, StoreError,
};
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router, middleware};
use serde::{Deserialize, Deserializer, Serialize};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::auth::{Access, Auth};

const MAX_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: u32 = 50;
const MAX_ISSUE_REF_LEN: usize = 256;

pub fn router(store: Store, auth: Auth) -> Router {
    let api = Router::new()
        .route("/api/v1/jobs", post(create_job).get(list_jobs))
        .route("/api/v1/jobs/{id}", get(job))
        .route("/api/v1/jobs/{id}/runs", get(list_runs))
        .route("/api/v1/jobs/{id}/cancel", post(cancel_job))
        .route("/api/v1/jobs/{id}/retry", post(retry_job))
        .route(
            "/api/v1/repos/{id}/budget",
            get(repo_budget).put(set_repo_budget),
        )
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            crate::auth::api::guard,
        ));
    Router::new()
        .route("/api/v1/health", get(health))
        .merge(api)
        .merge(crate::web::router(auth))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such route") })
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        // After the limit above: a delivery carries the whole issue and
        // has its own limit (#169).
        .nest_service(
            "/webhooks",
            crate::webhook::router(store.clone(), crate::webhook::WebhookConfig::from_env()),
        )
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        // The path only: a query may hold an SSO code (`/auth/callback`).
        .layer(
            TraceLayer::new_for_http().make_span_with(|r: &axum::http::Request<_>| {
                tracing::info_span!("request", method = %r.method(), path = %r.uri().path())
            }),
        )
        .with_state(store)
}

#[derive(Debug, Serialize)]
struct Envelope<T> {
    data: Option<T>,
    error: Option<ErrorBody>,
    meta: Option<Meta>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct Meta {
    limit: u32,
    /// Pass as `cursor` for the next page; absent on the last one.
    next_cursor: Option<i64>,
}

fn ok<T: Serialize>(status: StatusCode, data: T) -> Response {
    let body = Envelope {
        data: Some(data),
        error: None,
        meta: None,
    };
    (status, Json(body)).into_response()
}

fn page<T, V: Serialize>(items: PageOf<T>, limit: u32, view: impl Fn(T) -> V) -> Response {
    let meta = Meta {
        limit,
        next_cursor: items.next_page().and_then(Page::cursor),
    };
    let body = Envelope {
        data: Some(items.items.into_iter().map(view).collect::<Vec<_>>()),
        error: None,
        meta: Some(meta),
    };
    Json(body).into_response()
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", message)
    }

    fn job_not_found(id: i64) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("job {id} not found"),
        )
    }

    fn forbidden(repo: RepoId, needed: RepoRole) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            format!("needs the {} role in repo {}", needed.as_str(), repo.0),
        )
    }
}

/// Job `id` when `access` may do what `needed` allows in its repo; a job
/// of a repo it cannot see is not found.
async fn job_for(
    store: &Store,
    access: &Access,
    id: i64,
    needed: RepoRole,
) -> Result<Job, ApiError> {
    let job = store
        .job(JobId(id))
        .await?
        .filter(|j| access.can(j.repo_id, RepoRole::Viewer))
        .ok_or_else(|| ApiError::job_not_found(id))?;
    if !access.can(job.repo_id, needed) {
        return Err(ApiError::forbidden(job.repo_id, needed));
    }
    Ok(job)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Envelope::<()> {
            data: None,
            error: Some(ErrorBody {
                code: self.code,
                message: self.message,
            }),
            meta: None,
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(what) => Self::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("{what} not found"),
            ),
            StoreError::Conflict(why) => Self::new(StatusCode::CONFLICT, "conflict", why),
            StoreError::BudgetSpent(why) => Self::new(StatusCode::CONFLICT, "budget_spent", why),
            StoreError::Invalid { field, message } => Self::invalid(format!("{field}: {message}")),
            StoreError::Ledger(e) => Self::invalid(e.to_string()),
            // Database details stay in the log.
            other => {
                tracing::error!(error = %other, "store");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                )
            }
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        Self::new(e.status(), "bad_request", e.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", e.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(e: PathRejection) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", e.body_text())
    }
}

type ApiResult = Result<Response, ApiError>;

#[derive(Debug, Serialize)]
struct JobView {
    id: i64,
    repo_id: i64,
    issue_ref: String,
    ledger_key: String,
    source: &'static str,
    status: &'static str,
    outcome: Option<aegis_ledger::Outcome>,
    reason: Option<String>,
    pr_url: Option<String>,
    attempts: u32,
    retries: u32,
    cancel_requested: bool,
    retry_of: Option<i64>,
    locked_by: Option<String>,
    locked_until: Option<String>,
    run_after: String,
    created_at: String,
    updated_at: String,
}

impl From<Job> for JobView {
    fn from(j: Job) -> Self {
        Self {
            id: j.id.0,
            repo_id: j.repo_id.0,
            issue_ref: j.issue_ref,
            ledger_key: j.ledger_key.as_str().to_owned(),
            source: j.source.as_str(),
            status: j.status.as_str(),
            outcome: j.outcome,
            reason: j.reason,
            pr_url: j.pr_url,
            attempts: j.attempts,
            retries: j.retries,
            cancel_requested: j.cancel_requested,
            retry_of: j.retry_of.map(|r| r.0),
            locked_by: j.locked_by,
            locked_until: j.locked_until.map(|t| t.to_rfc3339()),
            run_after: j.run_after.to_rfc3339(),
            created_at: j.created_at.to_rfc3339(),
            updated_at: j.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
struct RunView {
    id: i64,
    job_id: i64,
    attempt: u32,
    status: &'static str,
    error: Option<String>,
    started_at: String,
    ended_at: Option<String>,
}

impl From<Run> for RunView {
    fn from(r: Run) -> Self {
        Self {
            id: r.id.0,
            job_id: r.job_id.0,
            attempt: r.attempt,
            status: r.status.as_str(),
            error: r.error,
            started_at: r.started_at.to_rfc3339(),
            ended_at: r.ended_at.map(|t| t.to_rfc3339()),
        }
    }
}

async fn health() -> Response {
    ok(StatusCode::OK, serde_json::json!({ "status": "ok" }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateJob {
    repo_id: i64,
    issue_ref: String,
    source: String,
    /// By default derived from `issue_ref` and the time.
    #[serde(default)]
    ledger_key: Option<String>,
}

async fn create_job(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    body: Result<Json<CreateJob>, JsonRejection>,
) -> ApiResult {
    let Json(body) = body?;
    let repo_id = RepoId(body.repo_id);
    if !access.can(repo_id, RepoRole::Maintainer) {
        return Err(ApiError::forbidden(repo_id, RepoRole::Maintainer));
    }
    let issue_ref = body.issue_ref.trim();
    if issue_ref.is_empty() || issue_ref.len() > MAX_ISSUE_REF_LEN {
        return Err(ApiError::invalid(format!(
            "issue_ref: 1 to {MAX_ISSUE_REF_LEN} bytes"
        )));
    }
    let source = JobSource::parse(&body.source)
        .ok_or_else(|| ApiError::invalid(format!("source: unknown `{}`", body.source)))?;
    let key = body
        .ledger_key
        .unwrap_or_else(|| default_ledger_key(issue_ref));
    let ledger_key = IssueId::parse(key).map_err(|e| ApiError::invalid(e.to_string()))?;
    let new = NewJob {
        repo_id,
        issue_ref: issue_ref.to_owned(),
        ledger_key,
        source,
    };
    let job = store.create_job_by(&new, &access.actor).await?;
    Ok(ok(StatusCode::CREATED, JobView::from(job)))
}

/// `owner/repo#12` → `owner-repo-12-<ms>`: a ledger of its own for each
/// job, even of the same issue.
pub(crate) fn default_ledger_key(issue_ref: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let stem: String = issue_ref
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(100)
        .collect();
    format!("{}-{millis}", stem.trim_matches('-'))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListJobs {
    repo_id: Option<i64>,
    status: Option<String>,
    limit: Option<u32>,
    cursor: Option<i64>,
}

fn page_of(limit: Option<u32>, cursor: Option<i64>) -> Page {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    match cursor {
        Some(c) => Page::after(c, limit),
        None => Page::first(limit),
    }
}

async fn list_jobs(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    query: Result<Query<ListJobs>, QueryRejection>,
) -> ApiResult {
    let Query(q) = query?;
    let status = q
        .status
        .as_deref()
        .map(|s| {
            JobStatus::parse(s).ok_or_else(|| ApiError::invalid(format!("status: unknown `{s}`")))
        })
        .transpose()?;
    let filter = JobFilter {
        repo_id: q.repo_id.map(RepoId),
        status,
    };
    let p = page_of(q.limit, q.cursor);
    let jobs = store
        .list_jobs_in(&filter, access.visible().as_deref(), p)
        .await?;
    Ok(page(jobs, p.limit(), JobView::from))
}

async fn job(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult {
    let Path(id) = id?;
    let job = job_for(&store, &access, id, RepoRole::Viewer).await?;
    Ok(ok(StatusCode::OK, JobView::from(job)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRuns {
    limit: Option<u32>,
    cursor: Option<i64>,
}

async fn list_runs(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<ListRuns>, QueryRejection>,
) -> ApiResult {
    let Path(id) = id?;
    let Query(q) = query?;
    job_for(&store, &access, id, RepoRole::Viewer).await?;
    let p = page_of(q.limit, q.cursor);
    let runs = store.list_runs(JobId(id), p).await?;
    Ok(page(runs, p.limit(), RunView::from))
}

async fn cancel_job(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult {
    let Path(id) = id?;
    job_for(&store, &access, id, RepoRole::Maintainer).await?;
    let job = store.request_cancel_by(JobId(id), &access.actor).await?;
    // A running job stops when its worker next renews the lease.
    let status = if job.status == JobStatus::Cancelled {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok(ok(status, JobView::from(job)))
}

async fn retry_job(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult {
    let Path(id) = id?;
    job_for(&store, &access, id, RepoRole::Maintainer).await?;
    let job = store.retry_job_by(JobId(id), &access.actor).await?;
    Ok(ok(StatusCode::CREATED, JobView::from(job)))
}

#[derive(Debug, Serialize)]
struct BudgetView {
    repo_id: i64,
    slug: String,
    /// `null`: no monthly cap.
    monthly_usd: Option<f64>,
    spent_usd: f64,
    left_usd: Option<f64>,
    /// `unlimited`, `ok`, `warn` or `spent`: no new job runs.
    state: &'static str,
    time_zone: String,
    warn_ratio: f64,
    /// A repo with less than this left is spent: one job's cap.
    job_reserve_usd: f64,
}

impl BudgetView {
    fn new(store: &Store, b: RepoBudget) -> Self {
        let rules = store.budget_rules();
        Self {
            repo_id: b.repo_id.0,
            state: b.state(rules).as_str(),
            left_usd: b.left_usd(),
            slug: b.slug,
            monthly_usd: b.monthly_usd,
            spent_usd: b.spent_usd,
            time_zone: rules.time_zone.clone(),
            warn_ratio: rules.warn_ratio,
            job_reserve_usd: rules.job_reserve_usd,
        }
    }
}

/// Repo `id` when `access` may do what `needed` allows in it; a repo it
/// cannot see is not found.
fn repo_for(access: &Access, id: i64, needed: RepoRole) -> Result<RepoId, ApiError> {
    let repo = RepoId(id);
    if !access.can(repo, RepoRole::Viewer) {
        return Err(repo_not_found(id));
    }
    if !access.can(repo, needed) {
        return Err(ApiError::forbidden(repo, needed));
    }
    Ok(repo)
}

fn repo_not_found(id: i64) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("repo {id} not found"),
    )
}

async fn repo_budget(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult {
    let Path(id) = id?;
    let repo = repo_for(&access, id, RepoRole::Viewer)?;
    let budget = store
        .repo_budget(repo)
        .await?
        .ok_or_else(|| repo_not_found(id))?;
    Ok(ok(StatusCode::OK, BudgetView::new(&store, budget)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetBudget {
    /// Required; `null` removes the cap.
    #[serde(default, deserialize_with = "present")]
    monthly_usd: Option<Option<f64>>,
}

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<f64>>, D::Error> {
    Option::<f64>::deserialize(d).map(Some)
}

async fn set_repo_budget(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<SetBudget>, JsonRejection>,
) -> ApiResult {
    let Path(id) = id?;
    let Json(body) = body?;
    let repo = repo_for(&access, id, RepoRole::Maintainer)?;
    let monthly_usd = body
        .monthly_usd
        .ok_or_else(|| ApiError::invalid("monthly_usd: required, `null` for no cap"))?;
    let budget = store
        .set_monthly_budget_by(repo, monthly_usd, &access.actor)
        .await?;
    Ok(ok(StatusCode::OK, BudgetView::new(&store, budget)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_ledger_key_is_a_valid_id() {
        let key = default_ledger_key("typst/typst#6123");
        assert!(key.starts_with("typst-typst-6123-"), "{key}");
        IssueId::parse(key).unwrap();
        IssueId::parse(default_ledger_key(&"é#".repeat(200))).unwrap();
    }
}
