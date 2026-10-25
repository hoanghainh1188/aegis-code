//! Issues and CI failures from GitHub and GitLab, by webhook (ADR 0003 §4,
//! #25). A delivery is checked (signature, rate), read for what it asks
//! (the trigger label put on an issue, a failed CI run on the default
//! branch), and its job is written with the delivery's ID in one
//! transaction; the answer comes right after, the worker runs the job.
//!
//! - `POST /webhooks/github`: `X-Hub-Signature-256` with the secret
//!   `AEGIS_GITHUB_WEBHOOK_SECRET`; events `issues` and `workflow_run`.
//! - `POST /webhooks/gitlab`: `X-Gitlab-Token` equal to the secret
//!   `AEGIS_GITLAB_WEBHOOK_TOKEN`; events `Issue Hook` and `Pipeline Hook`.
//!
//! Both from the server's secret store (#30), as they are at the time of
//! the delivery: a rotated secret is used once read, and a delivery that
//! does not verify makes the store read it again (at most every 30 s). An
//! endpoint whose secret is not in the store answers 404. The label is
//! `AEGIS_WEBHOOK_LABEL` (`aegis` by default), not a secret.
//!
//! A delivery carries the whole issue, so its body has its own limit,
//! `AEGIS_WEBHOOK_MAX_BODY_BYTES` (1 MiB by default), not the API's 64 KiB
//! (#169). Over it: 413, logged with the delivery ID, never the content.
//!
//! When the repo's monthly budget is spent (#31) the job is recorded
//! cancelled with the reason, and the GitHub issue told so.
//!
//! A verified delivery is still untrusted (design §7): only the repo's
//! URL and path and the issue or run number are read from it.

mod events;
mod notify;
mod verify;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aegis_ledger::IssueId;
use aegis_secrets::Secrets;
use aegis_store::{Delivered, JobSource, NewDelivery, NewJob, Store, StoreError, WebhookSource};
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::api::default_ledger_key;
use crate::worker::JobNotifier;
use events::{Kind, Skip, Wanted};

pub use aegis_secrets::Secret;
pub(crate) use notify::pr_url;
pub use notify::{
    COMMENT_TOKEN_ENV, GITHUB_API_ENV, GITLAB_API_ENV, GITLAB_COMMENT_TOKEN_ENV, GitHubNotifier,
    GitLabNotifier, plain_status_comment, status_comment,
};

pub const GITHUB_SECRET_ENV: &str = "AEGIS_GITHUB_WEBHOOK_SECRET";
pub const GITLAB_TOKEN_ENV: &str = "AEGIS_GITLAB_WEBHOOK_TOKEN";
pub const LABEL_ENV: &str = "AEGIS_WEBHOOK_LABEL";
pub const MAX_BODY_ENV: &str = "AEGIS_WEBHOOK_MAX_BODY_BYTES";
/// GitHub sends up to 25 MB, but an issue body is at most 65,536
/// characters: a delivery over 1 MiB is not one worth a job.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

const DEFAULT_LABEL: &str = "aegis";
const MAX_HEADER_LEN: usize = 128;

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    /// Where [`GITHUB_SECRET_ENV`] and [`GITLAB_TOKEN_ENV`] are read.
    pub secrets: Secrets,
    /// The label that asks for a job when put on an issue.
    pub label: String,
    /// Deliveries taken at once, over both endpoints…
    pub burst: u32,
    /// …and refilled at this rate. Beyond, 429: the sender retries.
    pub per_minute: u32,
    /// Tells the issue of a job refused for its repo's budget (#31)…
    pub notifier: Option<GitHubNotifier>,
    /// …on GitLab (#168).
    pub gitlab_notifier: Option<GitLabNotifier>,
    /// The largest body read from a delivery; beyond, 413.
    pub max_body_bytes: usize,
}

impl WebhookConfig {
    /// The secrets of the server's store ([`crate::secrets::installed`]),
    /// the label from the environment.
    pub fn from_env() -> Self {
        let secrets = crate::secrets::installed();
        Self {
            notifier: Some(GitHubNotifier::from_env(secrets.clone())),
            gitlab_notifier: Some(GitLabNotifier::from_env(secrets.clone())),
            secrets,
            label: std::env::var(LABEL_ENV)
                .ok()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| DEFAULT_LABEL.into()),
            burst: 60,
            per_minute: 120,
            max_body_bytes: max_body_from_env(),
        }
    }
}

fn max_body_from_env() -> usize {
    let Ok(value) = std::env::var(MAX_BODY_ENV) else {
        return DEFAULT_MAX_BODY_BYTES;
    };
    match value.trim().parse::<usize>() {
        Ok(n) if n > 0 => n,
        _ => {
            tracing::warn!(
                value,
                default = DEFAULT_MAX_BODY_BYTES,
                "{MAX_BODY_ENV} is not a positive number of bytes; using the default"
            );
            DEFAULT_MAX_BODY_BYTES
        }
    }
}

/// A token bucket for the whole process.
#[derive(Debug)]
struct RateLimit {
    burst: f64,
    per_sec: f64,
    state: Mutex<(f64, Instant)>,
}

impl RateLimit {
    fn new(burst: u32, per_minute: u32) -> Self {
        let burst = f64::from(burst.max(1));
        Self {
            burst,
            per_sec: f64::from(per_minute) / 60.0,
            state: Mutex::new((burst, Instant::now())),
        }
    }

    fn take(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (tokens, last) = *state;
        let now = Instant::now();
        let tokens =
            (tokens + now.duration_since(last).as_secs_f64() * self.per_sec).min(self.burst);
        let (left, taken) = if tokens >= 1.0 {
            (tokens - 1.0, true)
        } else {
            (tokens, false)
        };
        *state = (left, now);
        taken
    }
}

#[derive(Debug, Clone)]
struct Hooks {
    store: Store,
    config: Arc<WebhookConfig>,
    limit: Arc<RateLimit>,
}

/// The endpoints, under `/github` and `/gitlab`; nested at `/webhooks` by
/// [`crate::api::router`], whose timeout they keep. The body limit is
/// theirs, [`WebhookConfig::max_body_bytes`].
pub fn router(store: Store, config: WebhookConfig) -> Router {
    let limit = Arc::new(RateLimit::new(config.burst, config.per_minute));
    Router::new()
        .route("/github", post(github))
        .route("/gitlab", post(gitlab))
        .with_state(Hooks {
            store,
            config: Arc::new(config),
            limit,
        })
}

fn answer(status: StatusCode, data: Value) -> Response {
    let body = json!({ "data": data, "error": null, "meta": null });
    (status, Json(body)).into_response()
}

/// An error answer, in the API's envelope.
#[derive(Debug)]
struct Refusal {
    status: StatusCode,
    code: &'static str,
    message: String,
}

fn refuse(status: StatusCode, code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        status,
        code,
        message: message.into(),
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let body = json!({
            "data": null,
            "error": { "code": self.code, "message": self.message },
            "meta": null,
        });
        (self.status, Json(body)).into_response()
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= MAX_HEADER_LEN)
}

/// The checks before the body is read; the delivery ID and event when
/// they pass. A delivery that does not verify with the secret as read last
/// is checked again with the secret read now, if it changed (rotation).
async fn admit<'a>(
    hooks: &Hooks,
    secret_name: &str,
    verified: impl Fn(&Secret) -> bool,
    headers: &'a HeaderMap,
    id_header: &str,
    event_header: &str,
) -> Result<(&'a str, &'a str), Refusal> {
    let secrets = &hooks.config.secrets;
    let Some(secret) = secrets.get(secret_name) else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            "not_found",
            "webhook not configured",
        ));
    };
    if !hooks.limit.take() {
        return Err(refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many deliveries",
        ));
    }
    if !verified(&secret) {
        let rotated = secrets.refresh_after_auth_failure(secret_name).await;
        if !rotated.as_ref().is_some_and(&verified) {
            tracing::warn!("webhook delivery with a bad signature");
            return Err(refuse(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "bad signature",
            ));
        }
    }
    match (header(headers, id_header), header(headers, event_header)) {
        (Some(id), Some(event)) => Ok((id, event)),
        _ => Err(refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("{id_header} and {event_header} are required"),
        )),
    }
}

/// The whole body, up to the limit, so that the signature is checked on
/// all that was sent. Over it, 413: logged with the delivery ID only.
async fn read(
    hooks: &Hooks,
    headers: &HeaderMap,
    body: Body,
    id_header: &str,
) -> Result<Bytes, Refusal> {
    let max = hooks.config.max_body_bytes;
    to_bytes(body, max).await.map_err(|_| {
        let delivery = header(headers, id_header).unwrap_or("-");
        tracing::warn!(delivery, max_bytes = max, "webhook delivery too large");
        refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("body over {max} bytes"),
        )
    })
}

async fn github(State(hooks): State<Hooks>, headers: HeaderMap, body: Body) -> Response {
    let body = match read(&hooks, &headers, body, "x-github-delivery").await {
        Ok(body) => body,
        Err(refusal) => return refusal.into_response(),
    };
    let signature = header(&headers, "x-hub-signature-256");
    let admitted = admit(
        &hooks,
        GITHUB_SECRET_ENV,
        |s| verify::github(s.expose().as_bytes(), &body, signature),
        &headers,
        "x-github-delivery",
        "x-github-event",
    )
    .await;
    let (id, event) = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal.into_response(),
    };
    let wanted = events::github(event, &body, &hooks.config.label);
    deliver(&hooks, WebhookSource::Github, id, event, wanted).await
}

async fn gitlab(State(hooks): State<Hooks>, headers: HeaderMap, body: Body) -> Response {
    let body = match read(&hooks, &headers, body, "x-gitlab-event-uuid").await {
        Ok(body) => body,
        Err(refusal) => return refusal.into_response(),
    };
    let token = header(&headers, "x-gitlab-token");
    let admitted = admit(
        &hooks,
        GITLAB_TOKEN_ENV,
        |s| verify::gitlab(s.expose().as_bytes(), token),
        &headers,
        "x-gitlab-event-uuid",
        "x-gitlab-event",
    )
    .await;
    let (id, event) = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal.into_response(),
    };
    let wanted = events::gitlab(event, &body, &hooks.config.label);
    deliver(&hooks, WebhookSource::Gitlab, id, event, wanted).await
}

fn ignored(delivery: &str, why: &str) -> Response {
    tracing::info!(delivery, why, "webhook delivery ignored");
    answer(
        StatusCode::OK,
        json!({ "status": "ignored", "reason": why }),
    )
}

fn internal(delivery: &str, e: &StoreError) -> Response {
    // Nothing was recorded: the sender's retry is taken as new.
    tracing::error!(delivery, error = %e, "webhook delivery");
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
    )
    .into_response()
}

async fn deliver(
    hooks: &Hooks,
    source: WebhookSource,
    delivery: &str,
    event: &str,
    wanted: Result<Wanted, Skip>,
) -> Response {
    let wanted = match wanted {
        Ok(wanted) => wanted,
        Err(Skip::Ignored(why)) => return ignored(delivery, &why),
        Err(Skip::Malformed(why)) => {
            tracing::warn!(delivery, why, "webhook delivery not understood");
            return refuse(StatusCode::BAD_REQUEST, "bad_request", why).into_response();
        }
    };
    let repo = match hooks.store.repo_by_remote(&wanted.repo_url).await {
        Ok(Some(repo)) => repo,
        Ok(None) => return ignored(delivery, "repository not registered"),
        Err(StoreError::Conflict(why)) => return ignored(delivery, &why),
        Err(e) => return internal(delivery, &e),
    };
    let job_source = match (&wanted.kind, source) {
        (Kind::CiFailure { branch }, _) if *branch != repo.default_branch => {
            return ignored(delivery, "CI failed on another branch than the default one");
        }
        (Kind::CiFailure { .. }, _) => JobSource::Ci,
        (Kind::Issue, WebhookSource::Github) => JobSource::Github,
        (Kind::Issue, WebhookSource::Gitlab) => JobSource::Gitlab,
        // Polled (`crate::tracker`), never posted here.
        (Kind::Issue, WebhookSource::Jira) => JobSource::Jira,
        (Kind::Issue, WebhookSource::Backlog) => JobSource::Backlog,
    };
    let ledger_key = match IssueId::parse(default_ledger_key(&wanted.issue_ref)) {
        Ok(key) => key,
        Err(e) => {
            return refuse(StatusCode::BAD_REQUEST, "bad_request", e.to_string()).into_response();
        }
    };
    let delivered = hooks
        .store
        .deliver_job(
            &NewDelivery {
                source,
                delivery_id: delivery.to_owned(),
                event: event.to_owned(),
            },
            &NewJob {
                repo_id: repo.id,
                issue_ref: wanted.issue_ref.clone(),
                ledger_key,
                source: job_source,
            },
        )
        .await;
    let issue_ref = wanted.issue_ref.as_str();
    match delivered {
        Ok(Delivered::Queued(job)) => {
            tracing::info!(delivery, job = job.id.0, repo = %repo.slug, issue_ref, "job queued from a webhook");
            answer(
                StatusCode::ACCEPTED,
                json!({ "status": "queued", "job_id": job.id.0 }),
            )
        }
        Ok(Delivered::AlreadyLive(job)) => {
            tracing::info!(
                delivery,
                job = job.0,
                issue_ref,
                "the issue already has a live job"
            );
            answer(
                StatusCode::OK,
                json!({ "status": "already_queued", "job_id": job.0 }),
            )
        }
        Ok(Delivered::Refused(job)) => {
            tracing::info!(delivery, job = job.id.0, repo = %repo.slug, issue_ref, "job refused: the repo's budget is spent");
            let (job_id, reason) = (job.id.0, job.reason.clone());
            let notifier = (
                hooks.config.notifier.clone(),
                hooks.config.gitlab_notifier.clone(),
            );
            // The answer does not wait for GitHub or GitLab.
            tokio::spawn(async move { notifier.job_ended(&job).await });
            answer(
                StatusCode::OK,
                json!({ "status": "refused", "job_id": job_id, "reason": reason }),
            )
        }
        Ok(Delivered::Duplicate) => {
            tracing::info!(delivery, "webhook delivery received before");
            answer(StatusCode::OK, json!({ "status": "duplicate" }))
        }
        Err(StoreError::Invalid { field, message }) => refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("{field}: {message}"),
        )
        .into_response(),
        Err(e) => internal(delivery, &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_limit_lets_a_burst_through_then_refuses() {
        let limit = RateLimit::new(3, 0);
        assert!((0..3).all(|_| limit.take()));
        assert!(!limit.take());
    }

    #[test]
    fn secrets_are_not_printed() {
        let config = WebhookConfig {
            secrets: Secrets::fixed([(GITHUB_SECRET_ENV, "hunter2")]),
            ..WebhookConfig::from_env()
        };
        assert!(!format!("{config:?}").contains("hunter2"));
    }
}
