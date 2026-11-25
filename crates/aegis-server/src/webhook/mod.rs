//! Issues and CI failures from GitHub and GitLab, by webhook (ADR 0003 §4,
//! #25). A delivery is checked (signature, rate), read for what it asks
//! (the trigger label put on an issue, a failed CI run on the default
//! branch), and its job is written with the delivery's ID in one
//! transaction; the answer comes right after, the worker runs the job.
//!
//! - `POST /webhooks/github`: `X-Hub-Signature-256` with the secret
//!   `AEGIS_GITHUB_WEBHOOK_SECRET`; events `issues` and `workflow_run`,
//!   and for review rounds of the agent's PRs (#244)
//!   `pull_request_review` (changes requested), `pull_request_review_comment`
//!   and `issue_comment` (a line `/aegis fix`), from people who can write
//!   to the repo (asked of GitHub with the comment token). A line
//!   `/aegis approve` in an `issue_comment` on an open issue approves its
//!   plan waiting for approval (#675), from the same people.
//! - `POST /webhooks/gitlab`: `X-Gitlab-Token` equal to the secret
//!   `AEGIS_GITLAB_WEBHOOK_TOKEN`; events `Issue Hook` and `Pipeline Hook`,
//!   and `Note Hook` with a line `/aegis fix` on the agent's MR (#254), from
//!   a Developer or above of the repo's project (asked of GitLab with
//!   `AEGIS_GITLAB_COMMENT_TOKEN`); a `Note Hook` with a line
//!   `/aegis approve` on an open issue approves its plan (#675), from the
//!   same people.
//! - `POST /webhooks/bitbucket` (#634): `X-Hub-Signature` with the secret
//!   `AEGIS_BITBUCKET_WEBHOOK_SECRET` (HMAC-SHA256, as GitHub's); events
//!   `pullrequest:comment_created` (a line `/aegis fix`) and
//!   `pullrequest:changes_request_created` on the agent's PR, from people
//!   who can write to the repo (asked of Bitbucket with
//!   `AEGIS_BITBUCKET_COMMENT_TOKEN`), and `pullrequest:fulfilled`.
//! - `POST /webhooks/bitbucket-dc` (#635): Bitbucket Data Center's,
//!   `X-Hub-Signature` with `AEGIS_BITBUCKET_DC_WEBHOOK_SECRET`; events
//!   `pr:comment:added` (a line `/aegis fix`) and `pr:reviewer:needs_work`
//!   on the agent's PR, from people who can write to the repo (asked of
//!   `AEGIS_BITBUCKET_DC_API` with `AEGIS_BITBUCKET_DC_COMMENT_TOKEN`), and
//!   `pr:merged`.
//! - `POST /webhooks/azure-devops` (#636): an Azure DevOps service hook's,
//!   with `AEGIS_AZURE_DEVOPS_WEBHOOK_SECRET` as the Basic auth password
//!   or in `X-Aegis-Webhook-Secret` (service hooks sign nothing); the
//!   delivery's `id` and `eventType` are in its body. Events
//!   `ms.vss-code.git-pullrequest-comment-event` (a line `/aegis fix`) on
//!   the agent's PR, from people who can write to the repo (asked of
//!   `AEGIS_AZURE_DEVOPS_URL` with `AEGIS_AZURE_DEVOPS_COMMENT_TOKEN`), and
//!   `git.pullrequest.updated` to completed.
//!
//! All from the server's secret store (#30), as they are at the time of
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
mod installation;
mod notify;
mod verify;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aegis_ledger::IssueId;
use aegis_lifecycle::{IssueTrustConfig, IssueTrustPolicy};
use aegis_secrets::Secrets;
use aegis_store::{
    Delivered, JobId, JobSource, NewDelivery, NewJob, NewWebhookEvent, Repo, RepoId, Store,
    StoreError, WebhookOutcome, WebhookSource,
};
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
pub use notify::{
    AZURE_DEVOPS_AUTHORITY_ENV, AZURE_DEVOPS_CLIENT_ID_ENV, AZURE_DEVOPS_COMMENT_TOKEN_ENV,
    AZURE_DEVOPS_TENANT_ENV, AZURE_DEVOPS_URL_ENV, AzureDevOpsNotifier, AzureServicePrincipal,
    BITBUCKET_API_ENV, BITBUCKET_COMMENT_TOKEN_ENV, BITBUCKET_DC_API_ENV,
    BITBUCKET_DC_COMMENT_TOKEN_ENV, BitbucketDataCenterNotifier, BitbucketNotifier,
    COMMENT_TOKEN_ENV, GITHUB_API_ENV, GITLAB_API_ENV, GITLAB_COMMENT_TOKEN_ENV, GitHubNotifier,
    GitLabNotifier, comment_with, on_app_path, plain_comment_with, plain_status_comment,
    status_comment,
};
pub(crate) use notify::{DEFAULT_GITHUB_API, DEFAULT_GITLAB_API, pr_url};

pub const GITHUB_SECRET_ENV: &str = "AEGIS_GITHUB_WEBHOOK_SECRET";
pub const GITLAB_TOKEN_ENV: &str = "AEGIS_GITLAB_WEBHOOK_TOKEN";
pub const BITBUCKET_SECRET_ENV: &str = "AEGIS_BITBUCKET_WEBHOOK_SECRET";
pub const BITBUCKET_DC_SECRET_ENV: &str = "AEGIS_BITBUCKET_DC_WEBHOOK_SECRET";
pub const AZURE_DEVOPS_SECRET_ENV: &str = "AEGIS_AZURE_DEVOPS_WEBHOOK_SECRET";
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
    /// …and asks Bitbucket Cloud who may ask for a review round (#634).
    pub bitbucket_notifier: Option<BitbucketNotifier>,
    /// …and Bitbucket Data Center (#635).
    pub bitbucket_dc_notifier: Option<BitbucketDataCenterNotifier>,
    /// …and Azure DevOps (#636).
    pub azure_devops_notifier: Option<AzureDevOpsNotifier>,
    /// The largest body read from a delivery; beyond, 413.
    pub max_body_bytes: usize,
    /// The modes' configurations, for the Light model that extracts a
    /// merged PR's memory (#456); `None`: the ledger's summary is kept.
    pub modes: Option<Arc<aegis_lifecycle::Modes>>,
    /// The instance's GitHub App (ADR 0017 §5): its webhook's secret is a
    /// second one `POST /github` verifies with, and its repos are asked
    /// about and commented on as the app.
    pub app: Option<Arc<crate::github_app::AppHandle>>,
}

/// Which webhook a GitHub delivery came from, by the secret that verified
/// it (ADR 0017 §5), never by what it says: a repo's own webhook, or the
/// GitHub App's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    RepoWebhook,
    App,
}

impl Via {
    /// The path a repo with `config` takes its deliveries from.
    fn of(config: &serde_json::Value) -> Self {
        if on_app_path(config) {
            Self::App
        } else {
            Self::RepoWebhook
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::RepoWebhook => "the repo's own webhook",
            Self::App => "the GitHub App's webhook",
        }
    }
}

impl WebhookConfig {
    /// The secrets of the server's store ([`crate::secrets::installed`]),
    /// the label from the environment.
    pub fn from_env() -> Self {
        let secrets = crate::secrets::installed();
        Self {
            notifier: Some(GitHubNotifier::from_env(secrets.clone())),
            gitlab_notifier: Some(GitLabNotifier::from_env(secrets.clone())),
            bitbucket_notifier: Some(BitbucketNotifier::from_env(secrets.clone())),
            bitbucket_dc_notifier: BitbucketDataCenterNotifier::from_env(secrets.clone()),
            azure_devops_notifier: AzureDevOpsNotifier::from_env(secrets.clone()),
            secrets,
            label: std::env::var(LABEL_ENV)
                .ok()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| DEFAULT_LABEL.into()),
            burst: 60,
            per_minute: 120,
            max_body_bytes: max_body_from_env(),
            modes: None,
            app: None,
        }
    }

    /// With the instance's GitHub App (ADR 0017).
    pub fn with_github_app(self, app: Option<Arc<crate::github_app::AppHandle>>) -> Self {
        Self { app, ..self }
    }

    /// With the modes whose Light model extracts merged PRs' memory (#456).
    pub fn with_modes(self, modes: Option<Arc<aegis_lifecycle::Modes>>) -> Self {
        Self { modes, ..self }
    }

    /// What syncs a repo's runbooks and business rules (#660), when the
    /// modes are known.
    fn memory_sync(&self) -> Option<crate::memory_sync::MemorySync> {
        Some(crate::memory_sync::MemorySync {
            modes: Arc::clone(self.modes.as_ref()?),
            secrets: self.secrets.clone(),
            app: self.app.clone(),
        })
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
    /// The language of each repo's replies (#589).
    locales: crate::locale::Locales,
}

/// The endpoints, under `/github`, `/gitlab`, `/bitbucket`,
/// `/bitbucket-dc` and `/azure-devops`; nested at
/// `/webhooks` by [`crate::api::router`], whose timeout they keep. The
/// body limit is theirs, [`WebhookConfig::max_body_bytes`].
pub fn router(store: Store, config: WebhookConfig) -> Router {
    let limit = Arc::new(RateLimit::new(config.burst, config.per_minute));
    // Comments in each repo's language (#589), the server's default from
    // its modes (`[web] default_locale`); `en` on an API that could not
    // load them, which runs no job of its own.
    let locales = crate::locale::Locales::new(
        store.clone(),
        config
            .modes
            .as_ref()
            .map_or_else(aegis_pr::Locale::default, |m| m.default_locale),
    );
    let config = WebhookConfig {
        notifier: config.notifier.map(|n| {
            n.with_app(config.app.clone(), store.clone())
                .with_locales(locales.clone())
        }),
        gitlab_notifier: config
            .gitlab_notifier
            .map(|n| n.with_locales(locales.clone())),
        ..config
    };
    Router::new()
        .route("/github", post(github))
        .route("/gitlab", post(gitlab))
        .route("/bitbucket", post(bitbucket))
        .route("/bitbucket-dc", post(bitbucket_dc))
        .route("/azure-devops", post(azure_devops))
        .with_state(Hooks {
            store,
            config: Arc::new(config),
            limit,
            locales,
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
///
/// Each of `secrets` (its name, and what verifying with it says) is
/// checked, in constant time each; exactly one must verify. One the store
/// does not have is left out; none there at all, 404.
async fn admit<'a, T: Copy>(
    hooks: &Hooks,
    source: WebhookSource,
    secrets_named: &[(&str, T)],
    verified: impl Fn(&Secret) -> bool,
    headers: &'a HeaderMap,
    id_header: &str,
    event_header: &str,
) -> Result<(&'a str, &'a str, T), Refusal> {
    let tag = verify_delivery(hooks, source, secrets_named, verified).await?;
    match (header(headers, id_header), header(headers, event_header)) {
        (Some(id), Some(event)) => Ok((id, event, tag)),
        _ => Err(refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("{id_header} and {event_header} are required"),
        )),
    }
}

/// [`admit`]'s checks of who sent a delivery: the tag of the one secret
/// it verifies with. Azure DevOps names a delivery and its event in the
/// body (#636), read once this passes.
async fn verify_delivery<T: Copy>(
    hooks: &Hooks,
    source: WebhookSource,
    secrets_named: &[(&str, T)],
    verified: impl Fn(&Secret) -> bool,
) -> Result<T, Refusal> {
    let secrets = &hooks.config.secrets;
    let present: Vec<(&str, T, Secret)> = secrets_named
        .iter()
        .filter_map(|(name, tag)| secrets.get(name).map(|s| (*name, *tag, s)))
        .collect();
    if present.is_empty() {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            "not_found",
            "webhook not configured",
        ));
    }
    if !hooks.limit.take() {
        return Err(refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many deliveries",
        ));
    }
    // Every secret is checked, so the time taken does not say which one.
    let mut matched: Vec<T> = present
        .iter()
        .filter(|(_, _, secret)| verified(secret))
        .map(|(_, tag, _)| *tag)
        .collect();
    if matched.is_empty() {
        for (name, tag, _) in &present {
            let rotated = secrets.refresh_after_auth_failure(name).await;
            if rotated.as_ref().is_some_and(&verified) {
                matched.push(*tag);
            }
        }
    }
    if matched.len() > 1 {
        tracing::error!("webhook delivery verifies with two secrets: give each webhook its own");
        return Err(refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            "misconfigured",
            "the server's webhook secrets are not distinct",
        ));
    }
    let Some(tag) = matched.pop() else {
        {
            tracing::warn!("webhook delivery with a bad signature");
            // Nothing it says is trusted: not its ID, event or repo.
            record(
                hooks,
                NewWebhookEvent {
                    source,
                    delivery_id: None,
                    event: None,
                    repo: None,
                    outcome: WebhookOutcome::BadSignature,
                    reason: None,
                    job: None,
                },
            )
            .await;
            return Err(refuse(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "bad signature",
            ));
        }
    };
    Ok(tag)
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
    let mut secrets_named = vec![(GITHUB_SECRET_ENV, Via::RepoWebhook)];
    if let Some(app) = &hooks.config.app {
        secrets_named.push((app.webhook_secret_env(), Via::App));
    }
    let admitted = admit(
        &hooks,
        WebhookSource::Github,
        &secrets_named,
        |s| verify::github(s.expose().as_bytes(), &body, signature),
        &headers,
        "x-github-delivery",
        "x-github-event",
    )
    .await;
    let (id, event, via) = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal.into_response(),
    };
    let handled = if installation::is_event(event) {
        installation::handle(&hooks, via, id, event, &body).await
    } else if event == "ping" {
        // Sent by GitHub when the webhook is made (#331): the way is open.
        Handled::new(
            answer(StatusCode::OK, json!({ "status": "pong" })),
            WebhookOutcome::Ping,
            None,
            None,
        )
    } else {
        let wanted = events::github(event, &body, &hooks.config.label);
        deliver(&hooks, WebhookSource::Github, Some(via), id, event, wanted).await
    };
    handled
        .log(&hooks, WebhookSource::Github, id, event, &body)
        .await
}

async fn gitlab(State(hooks): State<Hooks>, headers: HeaderMap, body: Body) -> Response {
    let body = match read(&hooks, &headers, body, "x-gitlab-event-uuid").await {
        Ok(body) => body,
        Err(refusal) => return refusal.into_response(),
    };
    let token = header(&headers, "x-gitlab-token");
    let admitted = admit(
        &hooks,
        WebhookSource::Gitlab,
        &[(GITLAB_TOKEN_ENV, ())],
        |s| verify::gitlab(s.expose().as_bytes(), token),
        &headers,
        "x-gitlab-event-uuid",
        "x-gitlab-event",
    )
    .await;
    let (id, event, ()) = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal.into_response(),
    };
    let wanted = events::gitlab(event, &body, &hooks.config.label);
    let handled = deliver(&hooks, WebhookSource::Gitlab, None, id, event, wanted).await;
    handled
        .log(&hooks, WebhookSource::Gitlab, id, event, &body)
        .await
}

async fn bitbucket(State(hooks): State<Hooks>, headers: HeaderMap, body: Body) -> Response {
    let body = match read(&hooks, &headers, body, "x-request-uuid").await {
        Ok(body) => body,
        Err(refusal) => return refusal.into_response(),
    };
    let signature = header(&headers, "x-hub-signature");
    let admitted = admit(
        &hooks,
        WebhookSource::Bitbucket,
        &[(BITBUCKET_SECRET_ENV, ())],
        |s| verify::github(s.expose().as_bytes(), &body, signature),
        &headers,
        "x-request-uuid",
        "x-event-key",
    )
    .await;
    let (id, event, ()) = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal.into_response(),
    };
    let handled = if event == "diagnostics:ping" {
        // Sent by Bitbucket's "Test connection": the way is open.
        Handled::new(
            answer(StatusCode::OK, json!({ "status": "pong" })),
            WebhookOutcome::Ping,
            None,
            None,
        )
    } else {
        let wanted = events::bitbucket(event, &body);
        deliver(&hooks, WebhookSource::Bitbucket, None, id, event, wanted).await
    };
    handled
        .log(&hooks, WebhookSource::Bitbucket, id, event, &body)
        .await
}

async fn bitbucket_dc(State(hooks): State<Hooks>, headers: HeaderMap, body: Body) -> Response {
    let body = match read(&hooks, &headers, body, "x-request-id").await {
        Ok(body) => body,
        Err(refusal) => return refusal.into_response(),
    };
    let signature = header(&headers, "x-hub-signature");
    let admitted = admit(
        &hooks,
        WebhookSource::BitbucketDc,
        &[(BITBUCKET_DC_SECRET_ENV, ())],
        |s| verify::github(s.expose().as_bytes(), &body, signature),
        &headers,
        "x-request-id",
        "x-event-key",
    )
    .await;
    let (id, event, ()) = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal.into_response(),
    };
    let handled = if event == "diagnostics:ping" {
        // Sent by the webhook's "Test connection": the way is open.
        Handled::new(
            answer(StatusCode::OK, json!({ "status": "pong" })),
            WebhookOutcome::Ping,
            None,
            None,
        )
    } else {
        let wanted = events::bitbucket_dc(event, &body);
        deliver(&hooks, WebhookSource::BitbucketDc, None, id, event, wanted).await
    };
    handled
        .log(&hooks, WebhookSource::BitbucketDc, id, event, &body)
        .await
}

/// The custom header an Azure DevOps service hook may send the secret in,
/// instead of Basic auth (#636).
pub const AZURE_DEVOPS_SECRET_HEADER: &str = "x-aegis-webhook-secret";

async fn azure_devops(State(hooks): State<Hooks>, headers: HeaderMap, body: Body) -> Response {
    let body = match read(&hooks, &headers, body, "x-vss-activityid").await {
        Ok(body) => body,
        Err(refusal) => return refusal.into_response(),
    };
    let authorization = header(&headers, "authorization");
    let custom = header(&headers, AZURE_DEVOPS_SECRET_HEADER);
    let verified = verify_delivery(
        &hooks,
        WebhookSource::AzureDevops,
        &[(AZURE_DEVOPS_SECRET_ENV, ())],
        |s| verify::azure_devops(s.expose().as_bytes(), authorization, custom),
    )
    .await;
    if let Err(refusal) = verified {
        return refusal.into_response();
    }
    // A service hook names its delivery and event in the body.
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Named {
        #[serde(default)]
        id: String,
        #[serde(default)]
        event_type: String,
    }
    let named: Named = serde_json::from_slice(&body).unwrap_or(Named {
        id: String::new(),
        event_type: String::new(),
    });
    let fits = |s: &str| !s.is_empty() && s.len() <= MAX_HEADER_LEN;
    if !(fits(&named.id) && fits(&named.event_type)) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body's id and eventType are required",
        )
        .into_response();
    }
    let (id, event) = (named.id.as_str(), named.event_type.as_str());
    let handled = if event == "service.hook.test" {
        // Sent by the subscription's "Test": the way is open.
        Handled::new(
            answer(StatusCode::OK, json!({ "status": "pong" })),
            WebhookOutcome::Ping,
            None,
            None,
        )
    } else {
        let wanted = events::azure_devops(event, &body);
        deliver(&hooks, WebhookSource::AzureDevops, None, id, event, wanted).await
    };
    handled
        .log(&hooks, WebhookSource::AzureDevops, id, event, &body)
        .await
}

/// The answer to a delivery and what to record of it (#331).
struct Handled {
    response: Response,
    /// `None`: nothing recorded (an internal error, retried by the sender).
    outcome: Option<WebhookOutcome>,
    reason: Option<String>,
    /// The repo it was matched to; found from the delivery when `None`.
    repo: Option<RepoId>,
    /// The job it made or found, for a link on the repo's page (#373).
    job: Option<JobId>,
}

impl Handled {
    fn new(
        response: Response,
        outcome: WebhookOutcome,
        reason: Option<String>,
        repo: Option<RepoId>,
    ) -> Self {
        Self {
            response,
            outcome: Some(outcome),
            reason,
            repo,
            job: None,
        }
    }

    fn with_job(self, job: JobId) -> Self {
        Self {
            job: Some(job),
            ..self
        }
    }

    /// Records the delivery's outcome, then gives the answer. A delivery
    /// matched to no repo yet is matched by the repo it names: its body
    /// verified, and only that URL is read from it.
    async fn log(
        self,
        hooks: &Hooks,
        source: WebhookSource,
        delivery: &str,
        event: &str,
        body: &[u8],
    ) -> Response {
        let Some(outcome) = self.outcome else {
            return self.response;
        };
        let repo = match self.repo {
            Some(repo) => Some(repo),
            None => match events::repo_url(source, body) {
                Some(url) => hooks
                    .store
                    .repo_by_remote(&url)
                    .await
                    .ok()
                    .flatten()
                    .map(|r| r.id),
                None => None,
            },
        };
        let event = NewWebhookEvent {
            source,
            delivery_id: Some(delivery.to_owned()),
            event: Some(event.to_owned()),
            repo,
            outcome,
            reason: self.reason,
            job: self.job,
        };
        record(hooks, event).await;
        self.response
    }
}

/// Records `event`; a failure is logged, the delivery's answer stands.
async fn record(hooks: &Hooks, event: NewWebhookEvent) {
    if let Err(e) = hooks.store.record_webhook(&event).await {
        tracing::warn!(error = %e, "recording a webhook delivery");
    }
}

fn ignored(delivery: &str, why: &str) -> Handled {
    ignored_in(delivery, why, None)
}

fn ignored_in(delivery: &str, why: &str, repo: Option<RepoId>) -> Handled {
    tracing::info!(delivery, why, "webhook delivery ignored");
    Handled::new(
        answer(
            StatusCode::OK,
            json!({ "status": "ignored", "reason": why }),
        ),
        WebhookOutcome::Ignored,
        Some(why.to_owned()),
        repo,
    )
}

fn internal(delivery: &str, e: &StoreError) -> Handled {
    // Nothing was recorded: the sender's retry is taken as new.
    tracing::error!(delivery, error = %e, "webhook delivery");
    Handled {
        response: refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error",
        )
        .into_response(),
        outcome: None,
        reason: None,
        repo: None,
        job: None,
    }
}

fn malformed(why: String) -> Handled {
    Handled::new(
        refuse(StatusCode::BAD_REQUEST, "bad_request", why.clone()).into_response(),
        WebhookOutcome::Malformed,
        Some(why),
        None,
    )
}

fn issue_trust_policy(config: &serde_json::Value) -> IssueTrustPolicy {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        issue_trust: IssueTrustConfig,
    }
    serde_json::from_value::<Partial>(config.clone())
        .map(|p| p.issue_trust.policy())
        .unwrap_or_default()
}

async fn deliver(
    hooks: &Hooks,
    source: WebhookSource,
    via: Option<Via>,
    delivery: &str,
    event: &str,
    wanted: Result<Wanted, Skip>,
) -> Handled {
    let wanted = match wanted {
        Ok(wanted) => wanted,
        Err(Skip::Ignored(why)) => return ignored(delivery, &why),
        Err(Skip::Malformed(why)) => {
            tracing::warn!(delivery, why, "webhook delivery not understood");
            return malformed(why);
        }
    };
    if let Kind::ReviewRound { pr_url, sender } = &wanted.kind {
        let at = DeliveryMeta {
            source,
            via,
            delivery,
            event,
        };
        return review_round(hooks, at, &wanted.issue_ref, pr_url, sender).await;
    }
    if let Kind::PrMerged { pr_url } = &wanted.kind {
        return pr_merged(hooks, source, via, delivery, pr_url).await;
    }
    if let Kind::PrClosed { pr_url } = &wanted.kind {
        return pr_closed(hooks, source, via, delivery, pr_url).await;
    }
    let repo = match hooks.store.repo_by_remote(&wanted.repo_url).await {
        Ok(Some(repo)) => repo,
        Ok(None) => return ignored(delivery, "repository not registered"),
        Err(StoreError::Conflict(why)) => return ignored(delivery, &why),
        Err(e) => return internal(delivery, &e),
    };
    if let Some(skip) = off_path(hooks, via, &repo).await {
        return match skip {
            Ok(why) => ignored_in(delivery, &why, Some(repo.id)),
            Err(e) => internal(delivery, &e),
        };
    }
    if let Kind::Push { branch } = &wanted.kind {
        return pushed(hooks, delivery, &repo, branch);
    }
    if let Kind::IssueClosed { closer } = &wanted.kind {
        let closed = ClosedIssue {
            source,
            via,
            delivery,
            issue_ref: &wanted.issue_ref,
            closer,
        };
        return issue_closed(hooks, closed, &repo).await;
    }
    let gate = match &wanted.kind {
        Kind::Approve { sender, mode } => Some((sender, GateCommand::Approve(mode.as_deref()))),
        Kind::Revise { sender, feedback } => Some((sender, GateCommand::Revise(feedback))),
        _ => None,
    };
    if let Some((sender, command)) = gate {
        let at = DeliveryMeta {
            source,
            via,
            delivery,
            event,
        };
        return design_gate(hooks, at, &repo, &wanted.issue_ref, sender, command).await;
    }
    let on_app = (via == Some(Via::App)).then_some(repo.tenant_id);
    if let Kind::Issue { author } = &wanted.kind {
        let policy = issue_trust_policy(&repo.config);
        if policy == IssueTrustPolicy::Collaborators {
            let allowed = match (source, author.as_deref()) {
                (WebhookSource::Gitlab, Some(author_id)) => {
                    let Some(notifier) = hooks.config.gitlab_notifier.as_ref() else {
                        return ignored_in(
                            delivery,
                            "no GitLab client to check the author's rights",
                            Some(repo.id),
                        );
                    };
                    let (Some(project), Ok(user_id)) =
                        (project_path(&repo.url), author_id.parse::<u64>())
                    else {
                        return ignored_in(
                            delivery,
                            "the repository's URL names no project",
                            Some(repo.id),
                        );
                    };
                    notifier.can_develop(project, user_id).await
                }
                (WebhookSource::Gitlab, None) => {
                    tracing::info!(delivery, repo = %repo.slug, "issue labeled but author is unknown");
                    return ignored_in(delivery, "the issue author is unknown", Some(repo.id));
                }
                (_, Some(login)) => {
                    let Some(notifier) = hooks.config.notifier.as_ref() else {
                        return ignored_in(
                            delivery,
                            "no GitHub client to check the author's rights",
                            Some(repo.id),
                        );
                    };
                    let Some((owner, name)) = owner_and_name(&repo.url) else {
                        return ignored_in(
                            delivery,
                            "the repository's URL names no owner and name",
                            Some(repo.id),
                        );
                    };
                    notifier.can_write(owner, name, login, on_app).await
                }
                (_, None) => {
                    tracing::info!(delivery, repo = %repo.slug, "issue labeled but author is unknown");
                    return ignored_in(delivery, "the issue author is unknown", Some(repo.id));
                }
            };
            match allowed {
                Ok(true) => {}
                Ok(false) => {
                    tracing::info!(delivery, ?author, repo = %repo.slug, "issue labeled but author cannot write");
                    return ignored_in(
                        delivery,
                        "the issue author cannot write to the repository",
                        Some(repo.id),
                    );
                }
                Err(why) => {
                    tracing::warn!(delivery, why, "checking the issue author's rights");
                    return ignored_in(
                        delivery,
                        "the issue author's rights could not be checked",
                        Some(repo.id),
                    );
                }
            }
        }
    }
    let job_source = match (&wanted.kind, source) {
        (Kind::CiFailure { branch }, _) if *branch != repo.default_branch => {
            return ignored_in(
                delivery,
                "CI failed on another branch than the default one",
                Some(repo.id),
            );
        }
        (Kind::CiFailure { .. }, _) => JobSource::Ci,
        (Kind::Issue { .. }, WebhookSource::Github) => JobSource::Github,
        (Kind::Issue { .. }, WebhookSource::Gitlab) => JobSource::Gitlab,
        // Polled (`crate::tracker`), never posted here.
        (Kind::Issue { .. }, WebhookSource::Jira) => JobSource::Jira,
        (Kind::Issue { .. }, WebhookSource::Backlog) => JobSource::Backlog,
        (Kind::Issue { .. }, WebhookSource::Jsm) => JobSource::Jsm,
        (Kind::Issue { .. }, WebhookSource::Servicenow) => JobSource::Servicenow,
        // Bitbucket deliveries ask for review rounds only (#634).
        (Kind::Issue { .. }, WebhookSource::Bitbucket | WebhookSource::BitbucketDc) => {
            return ignored_in(delivery, "Bitbucket issues make no jobs", Some(repo.id));
        }
        // Service hooks ask for review rounds only (#636).
        (Kind::Issue { .. }, WebhookSource::AzureDevops) => {
            return ignored_in(
                delivery,
                "Azure DevOps work items make no jobs",
                Some(repo.id),
            );
        }
        (
            Kind::ReviewRound { .. }
            | Kind::PrMerged { .. }
            | Kind::PrClosed { .. }
            | Kind::Push { .. }
            | Kind::IssueClosed { .. }
            | Kind::Approve { .. }
            | Kind::Revise { .. },
            _,
        ) => {
            unreachable!("handled above")
        }
    };
    let ledger_key = match IssueId::parse(default_ledger_key(&wanted.issue_ref)) {
        Ok(key) => key,
        Err(e) => return malformed(e.to_string()),
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
                priority: None,
                sla_deadline: None,
            },
        )
        .await;
    let issue_ref = wanted.issue_ref.as_str();
    match delivered {
        Ok(Delivered::Queued(job)) => {
            tracing::info!(delivery, job = job.id.0, repo = %repo.slug, issue_ref, "job queued from a webhook");
            let reason = format!("job {} for {issue_ref}", job.id.0);
            Handled::new(
                answer(
                    StatusCode::ACCEPTED,
                    json!({ "status": "queued", "job_id": job.id.0 }),
                ),
                WebhookOutcome::Queued,
                Some(reason),
                Some(repo.id),
            )
            .with_job(job.id)
        }
        Ok(Delivered::AlreadyLive(job)) => {
            tracing::info!(
                delivery,
                job = job.0,
                issue_ref,
                "the issue already has a live job"
            );
            Handled::new(
                answer(
                    StatusCode::OK,
                    json!({ "status": "already_queued", "job_id": job.0 }),
                ),
                WebhookOutcome::AlreadyLive,
                Some(format!("{issue_ref} already has job {}", job.0)),
                Some(repo.id),
            )
            .with_job(job)
        }
        Ok(Delivered::Refused(job)) => {
            tracing::info!(delivery, job = job.id.0, repo = %repo.slug, issue_ref, "job refused: the repo's budget is spent");
            let (job_id, reason) = (job.id.0, job.reason.clone());
            let notifier = (
                hooks.config.notifier.clone(),
                hooks.config.gitlab_notifier.clone(),
            );
            // The answer does not wait for GitHub or GitLab.
            tokio::spawn(async move { notifier.job_ended(&job, None).await });
            let why = reason
                .clone()
                .unwrap_or_else(|| "the repo's budget is spent".into());
            Handled::new(
                answer(
                    StatusCode::OK,
                    json!({ "status": "refused", "job_id": job_id, "reason": reason }),
                ),
                WebhookOutcome::Refused,
                Some(why),
                Some(repo.id),
            )
            .with_job(JobId(job_id))
        }
        Ok(Delivered::Duplicate) => {
            tracing::info!(delivery, "webhook delivery received before");
            Handled::new(
                answer(StatusCode::OK, json!({ "status": "duplicate" })),
                WebhookOutcome::Duplicate,
                None,
                Some(repo.id),
            )
        }
        Err(StoreError::Invalid { field, message }) => malformed(format!("{field}: {message}")),
        Err(e) => internal(delivery, &e),
    }
}

/// Why a GitHub delivery verified as `via` makes nothing for `repo`, if
/// it does not (ADR 0017 §5): it came from the other path's webhook, or the
/// repo is on the app path and the app's installation on it was
/// suspended, removed or not given it. `Err`: the store failed.
async fn off_path(
    hooks: &Hooks,
    via: Option<Via>,
    repo: &aegis_store::Repo,
) -> Option<Result<String, StoreError>> {
    let via = via?;
    let path = Via::of(&repo.config);
    if via != path {
        return Some(Ok(format!(
            "delivered by {}, but the repo takes {}",
            via.name(),
            path.name()
        )));
    }
    if path != Via::App {
        return None;
    }
    let (owner, name) = owner_and_name(&repo.url)?;
    match hooks.store.app_on_repo(owner, name).await {
        Ok(state) => state
            .and_then(|s| s.refusal(&format!("{owner}/{name}")))
            .map(Ok),
        Err(e) => Some(Err(e)),
    }
}

/// The project path of a GitLab repo URL: what follows the host, without
/// `.git` (`https://gitlab.com/group/sub/app` → `group/sub/app`).
pub(crate) fn project_path(url: &str) -> Option<&str> {
    let path = url.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let after = path.split_once("://").map_or(path, |(_, r)| r);
    let (_, project) = after.split_once('/')?;
    project.contains('/').then_some(project)
}

/// `owner/name` of a repo URL (`https://github.com/owner/name`, with or
/// without `.git`).
/// The project and repository of an Azure Repos URL,
/// `…/<project>/_git/<repo>` (#636), by their names: `%20` read as a
/// space (#641).
pub(crate) fn azure_project_and_repo(url: &str) -> Option<(String, String)> {
    let path = url.trim_end_matches('/');
    let (front, repo) = path.rsplit_once("/_git/")?;
    let (_, project) = front.rsplit_once('/')?;
    Some((
        aegis_pr::azure_unescaped(project)?,
        aegis_pr::azure_unescaped(repo)?,
    ))
}

pub(crate) fn owner_and_name(url: &str) -> Option<(&str, &str)> {
    let path = url.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (rest, name) = path.rsplit_once('/')?;
    let owner = rest.rsplit('/').next()?;
    (!owner.is_empty() && !name.is_empty()).then_some((owner, name))
}

/// A push to `repo`'s default branch syncs its runbooks and business
/// rules (#660), in the background; a push to any other branch, a PR's
/// among them, syncs nothing.
fn pushed(hooks: &Hooks, delivery: &str, repo: &Repo, branch: &str) -> Handled {
    if branch != repo.default_branch {
        return ignored_in(
            delivery,
            "a push to another branch than the default one",
            Some(repo.id),
        );
    }
    if crate::memory_sync::sources_of(repo).is_none() {
        return ignored_in(delivery, "the repo names no memory sources", Some(repo.id));
    }
    let Some(sync) = hooks.config.memory_sync() else {
        return ignored_in(
            delivery,
            "the server has no modes to read the repo with",
            Some(repo.id),
        );
    };
    sync.sync_in_background(&hooks.store, repo);
    Handled::new(
        answer(
            StatusCode::ACCEPTED,
            json!({ "status": "memory_sync_started", "branch": branch }),
        ),
        WebhookOutcome::Recorded,
        Some(format!("syncing memory from {branch}")),
        Some(repo.id),
    )
}

/// Extracts `merged_pr` and `review_lesson` repository memories when an
/// agent PR is merged (#456); the merge moved the main branch, so the
/// repo's runbooks and business rules are synced too (#660).
async fn pr_merged(
    hooks: &Hooks,
    source: WebhookSource,
    via: Option<Via>,
    delivery: &str,
    pr_url: &str,
) -> Handled {
    if let Some(skip) = pr_off_path(hooks, via, delivery, pr_url).await {
        return skip;
    }
    let actor = aegis_store::Actor::webhook(source.as_str());
    let modes = hooks.config.modes.as_deref();
    let extracted = crate::memory::extract_and_store_merged_pr(
        &hooks.store,
        pr_url,
        modes,
        &hooks.config.secrets,
        &actor,
    );
    match extracted.await {
        Ok(saved) if saved.is_empty() => ignored(delivery, "no job opened this pull request"),
        Ok(saved) => {
            let repo_id = saved[0].repo_id;
            let job_id = saved[0].job_id;
            if let (Some(sync), Ok(Some(repo))) =
                (hooks.config.memory_sync(), hooks.store.repo(repo_id).await)
            {
                sync.sync_in_background(&hooks.store, &repo);
            }
            let count = saved.len();
            let mut handled = Handled::new(
                answer(
                    StatusCode::OK,
                    json!({
                        "status": "memory_recorded",
                        "memories": count,
                        "pr_url": pr_url,
                    }),
                ),
                WebhookOutcome::Recorded,
                Some(format!("recorded {count} memory entries from {pr_url}")),
                Some(repo_id),
            );
            if let Some(id) = job_id {
                handled = handled.with_job(id);
            }
            handled
        }
        Err(StoreError::Invalid { field, message }) => malformed(format!("{field}: {message}")),
        Err(e) => internal(delivery, &e),
    }
}

/// Whether `sender` may write to `repo`, asked of the forge of `source`
/// (#244). `Err`: why it could not be asked, or `Ok(Err)` when the forge
/// failed to answer.
async fn sender_can_write(
    hooks: &Hooks,
    source: WebhookSource,
    repo: &Repo,
    sender: &str,
    on_app: Option<aegis_store::TenantId>,
) -> Result<Result<bool, String>, &'static str> {
    Ok(match source {
        WebhookSource::Gitlab => {
            let Some(notifier) = hooks.config.gitlab_notifier.as_ref() else {
                return Err("no GitLab client to check the sender's rights");
            };
            let (Some(project), Ok(user_id)) = (project_path(&repo.url), sender.parse::<u64>())
            else {
                return Err("the repository's URL names no project");
            };
            notifier.can_develop(project, user_id).await
        }
        WebhookSource::Bitbucket => {
            let Some(notifier) = hooks.config.bitbucket_notifier.as_ref() else {
                return Err("no Bitbucket client to check the sender's rights");
            };
            let Some((workspace, name)) = owner_and_name(&repo.url) else {
                return Err("the repository's URL names no workspace and name");
            };
            notifier.can_write(workspace, name, sender).await
        }
        WebhookSource::BitbucketDc => {
            let Some(notifier) = hooks.config.bitbucket_dc_notifier.as_ref() else {
                return Err("no Bitbucket Data Center client to check the sender's rights");
            };
            // `https://<host>/scm/<key>/<slug>.git`.
            let Some((project, name)) = owner_and_name(&repo.url) else {
                return Err("the repository's URL names no project and slug");
            };
            notifier.can_write(project, name, sender).await
        }
        WebhookSource::AzureDevops => {
            let Some(notifier) = hooks.config.azure_devops_notifier.as_ref() else {
                return Err("no Azure DevOps client to check the sender's rights");
            };
            // `https://dev.azure.com/<org>/<project>/_git/<repo>`.
            let Some((project, name)) = azure_project_and_repo(&repo.url) else {
                return Err("the repository's URL names no project and repository");
            };
            notifier.can_write(&project, &name, sender).await
        }
        WebhookSource::Github
        | WebhookSource::Jira
        | WebhookSource::Backlog
        | WebhookSource::Jsm
        | WebhookSource::Servicenow => {
            let Some(notifier) = hooks.config.notifier.as_ref() else {
                return Err("no GitHub client to check the sender's rights");
            };
            let Some((owner, name)) = owner_and_name(&repo.url) else {
                return Err("the repository's URL names no owner and name");
            };
            notifier.can_write(owner, name, sender, on_app).await
        }
    })
}

/// The answer to a delivery about the PR `pr_url` that came by the path
/// its repo is not on (#244): counted from the webhook of the path its
/// repo is on only, so a repo with both webhooks does not record a PR
/// twice.
async fn pr_off_path(
    hooks: &Hooks,
    via: Option<Via>,
    delivery: &str,
    pr_url: &str,
) -> Option<Handled> {
    via?;
    let repo = match hooks.store.job_by_pr_url(pr_url).await {
        Ok(Some(job)) => hooks.store.repo(job.repo_id).await,
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    };
    match repo {
        Ok(Some(repo)) => match off_path(hooks, via, &repo).await? {
            Ok(why) => Some(ignored_in(delivery, &why, Some(repo.id))),
            Err(e) => Some(internal(delivery, &e)),
        },
        Ok(None) => None,
        Err(e) => Some(internal(delivery, &e)),
    }
}

/// The answer for a memory learned from a delivery (#657).
fn learned(memory: &aegis_store::RepoMemory, reason: String) -> Handled {
    let handled = Handled::new(
        answer(
            StatusCode::OK,
            json!({
                "status": "memory_recorded",
                "memories": 1,
                "kind": memory.kind.as_str(),
            }),
        ),
        WebhookOutcome::Recorded,
        Some(reason),
        Some(memory.repo_id),
    );
    match memory.job_id {
        Some(job) => handled.with_job(job),
        None => handled,
    }
}

/// An agent PR closed without being merged (#657): what its reviewers
/// said, in its review rounds, is kept as a `rejected_pattern`. A PR
/// closed without a word teaches nothing, and a reason a reviewer gave
/// on the dashboard is not overwritten.
async fn pr_closed(
    hooks: &Hooks,
    source: WebhookSource,
    via: Option<Via>,
    delivery: &str,
    pr_url: &str,
) -> Handled {
    if let Some(skip) = pr_off_path(hooks, via, delivery, pr_url).await {
        return skip;
    }
    let actor = aegis_store::Actor::webhook(source.as_str());
    let sync = hooks.config.memory_sync();
    let learnt =
        crate::memory_learn::rejected_pr_with(&hooks.store, pr_url, None, sync.as_ref(), &actor);
    match learnt.await {
        Ok(Some(memory)) => learned(
            &memory,
            format!("recorded a rejected pattern from {pr_url}"),
        ),
        Ok(None) => ignored(
            delivery,
            "no job opened this pull request, no review said why it was closed, \
             or a reviewer's reason is kept already",
        ),
        Err(StoreError::Invalid { field, message }) => malformed(format!("{field}: {message}")),
        Err(e) => internal(delivery, &e),
    }
}

/// An issue closed as done, and where its delivery came from.
#[derive(Clone, Copy)]
struct ClosedIssue<'a> {
    source: WebhookSource,
    via: Option<Via>,
    delivery: &'a str,
    issue_ref: &'a str,
    closer: &'a str,
}

/// An issue of `repo` closed as done (#657): when its last job was a
/// diagnosis (#451), not followed by a job that changes code, and the
/// person who closed it can write to the repo, the diagnosis is kept as a
/// `confirmed_diagnosis`.
async fn issue_closed(hooks: &Hooks, closed: ClosedIssue<'_>, repo: &Repo) -> Handled {
    let ClosedIssue {
        source,
        via,
        delivery,
        issue_ref,
        closer,
    } = closed;
    let ignored = |why: &str| ignored_in(delivery, why, Some(repo.id));
    let job = match hooks.store.latest_job_for_issue(repo.id, issue_ref).await {
        Ok(Some(job)) => job,
        Ok(None) => return ignored("no job was made for this issue"),
        Err(e) => return internal(delivery, &e),
    };
    if job.outcome != Some(aegis_ledger::Outcome::Diagnosed) {
        return ignored("the issue's last job was not a diagnosis");
    }
    match crate::memory_learn::diagnosis_confirmed(&hooks.store, &job).await {
        Ok(true) => return ignored("the diagnosis is kept already"),
        Ok(false) => {}
        Err(e) => return internal(delivery, &e),
    }
    let on_app = (via == Some(Via::App)).then_some(repo.tenant_id);
    match sender_can_write(hooks, source, repo, closer, on_app).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            tracing::info!(delivery, closer, repo = %repo.slug, "issue closed by someone who cannot write");
            return ignored("the issue was closed by someone who cannot write to the repository");
        }
        Ok(Err(why)) => {
            tracing::warn!(delivery, why, "checking the closer's rights");
            return ignored("the closer's rights could not be checked");
        }
        Err(why) => return ignored(why),
    }
    let actor = aegis_store::Actor::webhook(source.as_str());
    let sync = hooks.config.memory_sync();
    let learnt =
        crate::memory_learn::confirmed_diagnosis_with(&hooks.store, &job, sync.as_ref(), &actor);
    match learnt.await {
        Ok(Some(memory)) => learned(
            &memory,
            format!("recorded the diagnosis of job {} as confirmed", job.id.0),
        ),
        Ok(None) => ignored("the diagnosis kept no report"),
        Err(StoreError::Invalid { field, message }) => malformed(format!("{field}: {message}")),
        Err(e) => internal(delivery, &e),
    }
}

/// A design-gate command on an issue (#675, #677).
#[derive(Clone, Copy)]
enum GateCommand<'a> {
    /// `/aegis approve`, with the `mode=` it named.
    Approve(Option<&'a str>),
    /// `/aegis revise`, with its feedback.
    Revise(&'a str),
}

/// `/aegis approve` or `/aegis revise` on an issue of `repo` (#675, #677,
/// ADR 0019): the issue's plan waiting for approval is approved, or sent
/// back with the feedback to plan anew within the repo's cap, by the
/// sender, who must be able to write to the repo. What the store refuses
/// (an unknown mode, approved already, the cap reached, another live job of
/// the ticket, a spent budget) is told on the issue.
async fn design_gate(
    hooks: &Hooks,
    at: DeliveryMeta<'_>,
    repo: &Repo,
    issue_ref: &str,
    sender: &str,
    command: GateCommand<'_>,
) -> Handled {
    let DeliveryMeta {
        source,
        via,
        delivery,
        ..
    } = at;
    let ignored = |why: &str| ignored_in(delivery, why, Some(repo.id));
    let plan = match hooks
        .store
        .awaiting_approval_for_issue(repo.id, issue_ref)
        .await
    {
        Ok(Some(job)) => job,
        Ok(None) => return ignored("no plan of this issue waits for approval"),
        Err(e) => return internal(delivery, &e),
    };
    let on_app = (via == Some(Via::App)).then_some(repo.tenant_id);
    match sender_can_write(hooks, source, repo, sender, on_app).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            tracing::info!(delivery, sender, repo = %repo.slug, "design gate used by someone who cannot write");
            return ignored("the sender cannot write to the repository");
        }
        Ok(Err(why)) => {
            tracing::warn!(delivery, why, "checking the sender's rights");
            return ignored("the sender's rights could not be checked");
        }
        Err(why) => return ignored(why),
    }
    let actor = aegis_store::Actor::forge_user(source.as_str(), sender);
    let done = match command {
        GateCommand::Approve(mode) => match mode.map(crate::feature_gate::parse_mode).transpose() {
            Ok(mode) => hooks
                .store
                .approve_job_by(plan.id, mode, &actor)
                .await
                .map(|job| {
                    (
                        job,
                        "approved",
                        format!("plan of job {} approved", plan.id.0),
                    )
                }),
            Err(why) => Err(StoreError::Invalid {
                field: "mode",
                message: why,
            }),
        },
        GateCommand::Revise(feedback) => {
            let cap = crate::feature_gate::max_revisions(&repo.config);
            hooks
                .store
                .revise_job_by(plan.id, feedback, cap, &actor)
                .await
                .map(|(job, asked)| {
                    let why = format!(
                        "plan of job {} sent back for revision {}",
                        job.id.0,
                        asked.revision + 1
                    );
                    (job, "revising", why)
                })
        }
    };
    let why = match done {
        Ok((job, status, reason)) => {
            tracing::info!(delivery, job = job.id.0, repo = %repo.slug, status, "design gate from a webhook");
            return Handled::new(
                answer(
                    StatusCode::ACCEPTED,
                    json!({ "status": status, "job_id": job.id.0 }),
                ),
                WebhookOutcome::Queued,
                Some(reason),
                Some(repo.id),
            )
            .with_job(job.id);
        }
        Err(StoreError::Conflict(why) | StoreError::BudgetSpent(why)) => why,
        Err(StoreError::Invalid { message, .. }) => message,
        Err(e) => return internal(delivery, &e),
    };
    tracing::info!(delivery, why, "design gate refused");
    let locale = hooks.locales.of(repo.id).await;
    let text = format!(
        "{}\n{}",
        aegis_pr::REPLY_MARKER,
        (match command {
            GateCommand::Approve(_) => locale.strings().approve_refused,
            GateCommand::Revise(_) => locale.strings().revise_refused,
        })(&aegis_pr::inert(&why))
    );
    match source {
        WebhookSource::Gitlab => {
            if let (Some(n), Some((project, Ok(iid)))) = (
                &hooks.config.gitlab_notifier,
                issue_ref.rsplit_once('#').map(|(p, i)| (p, i.parse())),
            ) {
                n.comment_on_issue(project, iid, &text).await;
            }
        }
        // Only GitHub and GitLab issues carry `/aegis approve`.
        WebhookSource::Github => {
            if let (Some(n), Ok((owner, name, number))) = (
                &hooks.config.notifier,
                crate::lifecycle::parse_issue_ref(issue_ref),
            ) {
                n.comment(&owner, &name, number, &text, on_app).await;
            }
        }
        _ => {}
    }
    Handled::new(
        answer(
            StatusCode::OK,
            json!({ "status": "refused", "job_id": plan.id.0, "reason": why }),
        ),
        WebhookOutcome::Refused,
        Some(why),
        Some(repo.id),
    )
    .with_job(plan.id)
}

/// Where a delivery came from: the forge, the way it reached the server
/// (#244), its id and its event name.
#[derive(Clone, Copy)]
struct DeliveryMeta<'a> {
    source: WebhookSource,
    via: Option<Via>,
    delivery: &'a str,
    event: &'a str,
}

/// A review round asked for on the agent's PR (#244): the job that opened
/// it is found by the PR's URL, the sender must be able to write to the
/// job's repo, and the store queues the round, within the PR's rounds.
/// A refused round is told on the PR.
async fn review_round(
    hooks: &Hooks,
    at: DeliveryMeta<'_>,
    issue_ref: &str,
    pr_url: &str,
    sender: &str,
) -> Handled {
    let DeliveryMeta {
        source,
        via,
        delivery,
        event,
    } = at;
    let root = match hooks.store.job_by_pr_url(pr_url).await {
        Ok(Some(job)) => job,
        Ok(None) => return ignored(delivery, "no job opened this pull request"),
        Err(e) => return internal(delivery, &e),
    };
    let repo = match hooks.store.repo(root.repo_id).await {
        Ok(Some(repo)) => repo,
        Ok(None) => return ignored(delivery, "repository not registered"),
        Err(e) => return internal(delivery, &e),
    };
    if let Some(skip) = off_path(hooks, via, &repo).await {
        return match skip {
            Ok(why) => ignored_in(delivery, &why, Some(repo.id)),
            Err(e) => internal(delivery, &e),
        };
    }
    let on_app = (via == Some(Via::App)).then_some(repo.tenant_id);
    let ignored = |delivery: &str, why: &str| ignored_in(delivery, why, Some(repo.id));
    let allowed = match sender_can_write(hooks, source, &repo, sender, on_app).await {
        Ok(allowed) => allowed,
        Err(why) => return ignored(delivery, why),
    };
    match allowed {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(delivery, sender, repo = %repo.slug, "review round asked by someone who cannot write");
            return ignored(delivery, "the sender cannot write to the repository");
        }
        Err(why) => {
            tracing::warn!(delivery, why, "checking the sender's rights");
            return ignored(delivery, "the sender's rights could not be checked");
        }
    }
    let delivered = hooks
        .store
        .deliver_review(
            &NewDelivery {
                source,
                delivery_id: delivery.to_owned(),
                event: event.to_owned(),
            },
            root.id,
        )
        .await;
    let locale = hooks.locales.of(root.repo_id).await;
    let refused = |why: String| async move {
        tracing::info!(delivery, why, "review round refused");
        let text = format!(
            "{}\n{}",
            aegis_pr::REPLY_MARKER,
            (locale.strings().review_refused)(&aegis_pr::inert(&why))
        );
        // `group/project!iid` on GitLab, `owner/repo#n` on GitHub and
        // `workspace/repo#n` on Bitbucket, `KEY/slug#n` on its Data Center,
        // `project/repo#n` on Azure DevOps.
        match source {
            WebhookSource::Gitlab => {
                if let (Some(n), Some((project, Ok(iid)))) = (
                    &hooks.config.gitlab_notifier,
                    issue_ref.rsplit_once('!').map(|(p, i)| (p, i.parse())),
                ) {
                    n.comment_on_merge_request(project, iid, &text).await;
                }
            }
            WebhookSource::AzureDevops => {
                if let (Some(n), Ok((project, name, id))) = (
                    &hooks.config.azure_devops_notifier,
                    crate::lifecycle::parse_issue_ref(issue_ref),
                ) {
                    n.comment_on_pull_request(&project, &name, id, &text).await;
                }
            }
            WebhookSource::BitbucketDc => {
                if let (Some(n), Ok((project, name, id))) = (
                    &hooks.config.bitbucket_dc_notifier,
                    crate::lifecycle::parse_issue_ref(issue_ref),
                ) {
                    n.comment_on_pull_request(&project, &name, id, &text).await;
                }
            }
            WebhookSource::Bitbucket => {
                if let (Some(n), Ok((workspace, name, id))) = (
                    &hooks.config.bitbucket_notifier,
                    crate::lifecycle::parse_issue_ref(issue_ref),
                ) {
                    n.comment_on_pull_request(&workspace, &name, id, &text)
                        .await;
                }
            }
            WebhookSource::Github
            | WebhookSource::Jira
            | WebhookSource::Backlog
            | WebhookSource::Jsm
            | WebhookSource::Servicenow => {
                if let (Some(n), Ok((owner, name, number))) = (
                    &hooks.config.notifier,
                    crate::lifecycle::parse_issue_ref(issue_ref),
                ) {
                    n.comment(&owner, &name, number, &text, on_app).await;
                }
            }
        }
        Handled::new(
            answer(
                StatusCode::OK,
                json!({ "status": "refused", "job_id": root.id.0, "reason": why }),
            ),
            WebhookOutcome::Refused,
            Some(why),
            Some(repo.id),
        )
        .with_job(root.id)
    };
    match delivered {
        Ok(Delivered::Queued(job)) => {
            tracing::info!(delivery, job = job.id.0, repo = %repo.slug, round = job.review_round, "review round queued from a webhook");
            let reason = format!("review round {} of job {}", job.review_round, root.id.0);
            Handled::new(
                answer(
                    StatusCode::ACCEPTED,
                    json!({ "status": "queued", "job_id": job.id.0 }),
                ),
                WebhookOutcome::Queued,
                Some(reason),
                Some(repo.id),
            )
            .with_job(job.id)
        }
        Ok(Delivered::Duplicate) => {
            tracing::info!(delivery, "webhook delivery received before");
            Handled::new(
                answer(StatusCode::OK, json!({ "status": "duplicate" })),
                WebhookOutcome::Duplicate,
                None,
                Some(repo.id),
            )
        }
        Ok(other) => internal(
            delivery,
            &StoreError::Conflict(format!("unexpected delivery result {other:?}")),
        ),
        // A live round is not a refusal: the review is read when it runs.
        Err(StoreError::Conflict(why)) if why.contains("live review round") => Handled::new(
            answer(
                StatusCode::OK,
                json!({ "status": "already_queued", "job_id": root.id.0, "reason": why }),
            ),
            WebhookOutcome::AlreadyLive,
            Some(why),
            Some(repo.id),
        )
        .with_job(root.id),
        Err(StoreError::Conflict(why) | StoreError::BudgetSpent(why)) => refused(why).await,
        Err(e) => internal(delivery, &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_urls_name_their_project_and_repo_by_name() {
        assert_eq!(
            azure_project_and_repo("https://dev.azure.com/acme/My%20Shop/_git/the%20cart"),
            Some(("My Shop".into(), "the cart".into()))
        );
        assert_eq!(
            azure_project_and_repo("https://dev.azure.com/acme/Shop/_git/cart/"),
            Some(("Shop".into(), "cart".into()))
        );
        for bad in [
            "https://dev.azure.com/acme/My%2FShop/_git/cart",
            "https://dev.azure.com/acme/Shop/_git/a/b",
            "https://dev.azure.com/acme/../_git/cart",
            "https://github.com/acme/cart",
        ] {
            assert_eq!(azure_project_and_repo(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_gitlab_repo_url_gives_its_project_path() {
        assert_eq!(
            project_path("https://gitlab.com/acme/platform/app.git"),
            Some("acme/platform/app")
        );
        assert_eq!(project_path("https://gitlab.com/app"), None);
    }

    #[test]
    fn a_repo_url_gives_its_owner_and_name() {
        assert_eq!(
            owner_and_name("https://github.com/typst/typst.git/"),
            Some(("typst", "typst"))
        );
        assert_eq!(
            owner_and_name("file:///srv/trial/upstream/demo"),
            Some(("upstream", "demo"))
        );
        assert_eq!(owner_and_name("demo"), None);
    }

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
