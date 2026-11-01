//! A repo's connection to its forge (#331), for the admin: where the
//! forge sends webhooks and which events to turn on, whether each secret
//! the repo needs is in the store (never its value), what became of the
//! repo's last webhooks, and, on demand, a check of its tokens and URL
//! against the forge's API. With `[web] make_webhooks` (#376), the admin
//! makes the webhooks from here with a token given for that once: used
//! for the request and dropped, never kept, logged, audited or shown; the
//! secret they are signed with is the server's and never reaches the
//! browser.

use aegis_lifecycle::ForkPlatform;
use aegis_pr::{GitHub, GitLab, HookDone, PrError, RepoAccess};
use aegis_store::{BadSignatures, Repo, Store};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::admin::admin_only;
use super::repo_admin::{RepoAdmin, RepoAdminExt, editing, repo_of};
use super::webhooks::{self, WebhookRow};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::{Access, Auth};
use crate::repo_cli::RepoFile;
use crate::webhook::{
    COMMENT_TOKEN_ENV, GITHUB_SECRET_ENV, GITLAB_COMMENT_TOKEN_ENV, GITLAB_TOKEN_ENV,
    owner_and_name, project_path,
};

/// The webhooks shown on the page.
const SHOWN_WEBHOOKS: u32 = 10;
/// Bad signatures counted over this many hours.
const BAD_SIGNATURE_HOURS: i32 = 24;

/// What a repo's forge is, and what it needs there.
struct Forge {
    /// `github` or `gitlab`, as in the webhook's path.
    name: &'static str,
    webhook_secret: &'static str,
    comment_token: &'static str,
    /// Events to turn on for the repo's own project.
    repo_events: &'static [&'static str],
    /// Events to turn on for the fork, for review rounds.
    fork_events: &'static [&'static str],
    /// The same, as the forge's API names them (#376).
    repo_hook_events: &'static [&'static str],
    fork_hook_events: &'static [&'static str],
    /// What a token needs to make webhooks there.
    hook_right: &'static str,
}

const GITHUB: Forge = Forge {
    name: "github",
    webhook_secret: GITHUB_SECRET_ENV,
    comment_token: COMMENT_TOKEN_ENV,
    repo_events: &["Issues", "Workflow runs"],
    fork_events: &[
        "Pull request reviews",
        "Pull request review comments",
        "Issue comments",
    ],
    repo_hook_events: &["issues", "workflow_run"],
    fork_hook_events: &[
        "pull_request_review",
        "pull_request_review_comment",
        "issue_comment",
    ],
    hook_right: "the `admin:repo_hook` scope (fine-grained: Webhooks read and write), or admin of the repository",
};

const GITLAB: Forge = Forge {
    name: "gitlab",
    webhook_secret: GITLAB_TOKEN_ENV,
    comment_token: GITLAB_COMMENT_TOKEN_ENV,
    repo_events: &["Issues events", "Pipeline events"],
    fork_events: &["Comments"],
    repo_hook_events: &["issues_events", "pipeline_events"],
    fork_hook_events: &["note_events"],
    hook_right: "the Maintainer role on the project and the `api` scope",
};

fn forge_of(platform: ForkPlatform) -> &'static Forge {
    match platform {
        ForkPlatform::GitHub => &GITHUB,
        ForkPlatform::GitLab => &GITLAB,
    }
}

/// A secret the repo needs: its name and whether the store has it.
struct SecretRow {
    name: String,
    purpose: &'static str,
    set: bool,
}

/// One check's result.
pub struct CheckRow {
    pub name: &'static str,
    /// `pass`, `fail` or `skip`.
    pub state: &'static str,
    pub detail: String,
}

fn pass(name: &'static str, detail: impl Into<String>) -> CheckRow {
    CheckRow {
        name,
        state: "pass",
        detail: detail.into(),
    }
}

fn fail(name: &'static str, detail: impl Into<String>) -> CheckRow {
    CheckRow {
        name,
        state: "fail",
        detail: detail.into(),
    }
}

fn skip(name: &'static str, detail: impl Into<String>) -> CheckRow {
    CheckRow {
        name,
        state: "skip",
        detail: detail.into(),
    }
}

struct BadRow {
    source: &'static str,
    count: i64,
    last: String,
}

impl From<BadSignatures> for BadRow {
    fn from(b: BadSignatures) -> Self {
        Self {
            source: b.source.as_str(),
            count: b.count,
            last: b.last.format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    }
}

#[derive(Template)]
#[template(path = "admin_repo_connection.html")]
struct ConnectionPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    repo: &'a Repo,
    forge: &'static Forge,
    payload_url: String,
    /// `[auth] public_url` is not set: the payload URL is only a pattern.
    no_public_url: bool,
    fork: String,
    secrets: Vec<SecretRow>,
    checks: Option<Vec<CheckRow>>,
    webhooks: Vec<WebhookRow>,
    bad_signatures: Vec<BadRow>,
    bad_hours: i32,
    /// `[web] make_webhooks` (#376).
    can_make_webhooks: bool,
    /// What the last making did, said once.
    made: Option<&'static str>,
}

async fn is_set(admin: &RepoAdmin, name: &str) -> bool {
    matches!(admin.secrets.get_or_load(name).await, Ok(Some(_)))
}

async fn token(admin: &RepoAdmin, name: &str) -> Option<String> {
    admin
        .secrets
        .get_or_load(name)
        .await
        .ok()
        .flatten()
        .map(|t| t.expose().to_owned())
        .filter(|t| !t.is_empty())
}

#[allow(clippy::too_many_arguments)]
async fn page(
    store: &Store,
    admin: &RepoAdmin,
    auth: &Auth,
    nonce: &Nonce,
    me: &Me,
    repo: &Repo,
    checks: Option<Vec<CheckRow>>,
    made: Option<&'static str>,
) -> WebResult {
    let file = RepoFile::of(repo).map_err(|e| {
        tracing::error!(repo = %repo.slug, error = %e, "web: stored repo does not read back");
        WebError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
        )
    })?;
    let fork = &file.settings.fork;
    let forge = forge_of(fork.platform);
    let mut secrets = Vec::new();
    for (name, purpose) in [
        (
            forge.webhook_secret,
            "checks that a webhook comes from the forge",
        ),
        (
            fork.token_env.as_str(),
            "the fork: pushes the agent's branches, opens PRs",
        ),
        (
            forge.comment_token,
            "comments on the repo's issues; checks who asks for a review round",
        ),
    ] {
        secrets.push(SecretRow {
            name: name.to_owned(),
            purpose,
            set: is_set(admin, name).await,
        });
    }
    let public = auth.public_url();
    let base = if public.is_empty() {
        "https://<public_url>"
    } else {
        public.as_str()
    };
    render(&ConnectionPage {
        nonce: nonce.as_str(),
        me,
        repo,
        forge,
        payload_url: format!("{}/webhooks/{}", base, forge.name),
        no_public_url: public.is_empty(),
        fork: fork.path(),
        secrets,
        checks,
        webhooks: webhooks::rows(store, repo.id, SHOWN_WEBHOOKS).await?,
        bad_signatures: store
            .bad_signatures(BAD_SIGNATURE_HOURS)
            .await?
            .into_iter()
            .map(BadRow::from)
            .collect(),
        bad_hours: BAD_SIGNATURE_HOURS,
        can_make_webhooks: admin.make_webhooks,
        made,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn show(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(auth): Extension<Auth>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
    Query(q): Query<MadeQuery>,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    let repo = repo_of(&store, id).await?;
    // A fixed message per word, never the request's own text.
    let made = match q.made.as_str() {
        "created" => Some(
            "Webhook created. GitHub sends a ping at once: it shows below as \"ping: connected\".",
        ),
        "updated" => Some(
            "Webhook updated: the one already pointing at this server now has the events and secret it needs.",
        ),
        _ => None,
    };
    page(&store, &admin, &auth, &nonce, &me, &repo, None, made).await
}

#[derive(Debug, Default, Deserialize)]
pub struct MadeQuery {
    #[serde(default)]
    made: String,
}

pub async fn check(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(auth): Extension<Auth>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    let repo = repo_of(&store, id).await?;
    let checks = run_checks(&admin, &repo).await;
    tracing::info!(repo = %repo.slug, by = %access.actor.name, "repo connection checked");
    page(
        &store,
        &admin,
        &auth,
        &nonce,
        &me,
        &repo,
        Some(checks),
        None,
    )
    .await
}

/// The webhook form (#376). Not `Debug`: the token is never printed.
#[derive(Deserialize)]
pub struct HookForm {
    /// `repo` or `fork`.
    #[serde(default)]
    place: String,
    #[serde(default)]
    token: String,
}

fn bad(message: impl Into<String>) -> WebError {
    WebError::new(StatusCode::BAD_REQUEST, message)
}

/// Makes, or updates, the repo's or the fork's webhook to this server,
/// with the token the admin gave for that once.
pub async fn make_webhooks(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Extension(auth): Extension<Auth>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
    Form(form): Form<HookForm>,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    if !admin.make_webhooks {
        // As if the form did not exist: it is off unless turned on.
        return Err(WebError::not_found("page"));
    }
    let repo = repo_of(&store, id).await?;
    let given = form.token.trim();
    if given.is_empty() {
        return Err(bad(
            "give a token that may manage the webhooks: it is used once and not kept",
        ));
    }
    let file =
        RepoFile::of(&repo).map_err(|_| bad("the repo's settings do not read as a repo file"))?;
    let fork = &file.settings.fork;
    let forge = forge_of(fork.platform);
    // The token and the secret go to the forge the server is configured
    // with, never to an API root a repo's settings name.
    let api = match fork.platform {
        ForkPlatform::GitHub => admin.github_api.clone(),
        ForkPlatform::GitLab => admin.gitlab_api.clone(),
    };
    let (path, events) = match form.place.as_str() {
        "repo" => {
            let path = upstream_path(fork.platform, &repo.url).ok_or_else(|| {
                bad(format!(
                    "`{}` is not a {} repository URL",
                    repo.url, forge.name
                ))
            })?;
            (path, forge.repo_hook_events)
        }
        "fork" if fork.api.trim_end_matches('/') == api.trim_end_matches('/') => {
            (fork.path(), forge.fork_hook_events)
        }
        "fork" => {
            return Err(bad(format!(
                "the fork is on `{}`, not on the forge this server is configured with: make its webhook by hand",
                fork.api
            )));
        }
        _ => return Err(bad("place: `repo` or `fork`")),
    };
    let public = auth.public_url();
    if public.is_empty() {
        return Err(bad(
            "set the server's `[auth] public_url` first: it is where the forge sends webhooks",
        ));
    }
    let Some(secret) = token(&admin, forge.webhook_secret).await else {
        return Err(bad(format!(
            "the secret store has no `{}`, the webhook's secret: add it first",
            forge.webhook_secret
        )));
    };
    let url = format!("{public}/webhooks/{}", forge.name);
    let done = match fork.platform {
        ForkPlatform::GitHub => {
            let (owner, name) = path.split_once('/').unwrap_or((path.as_str(), ""));
            match GitHub::new(&api, owner, name, given) {
                Ok(gh) => gh.ensure_hook(&url, &secret, events).await,
                Err(e) => Err(e),
            }
        }
        ForkPlatform::GitLab => match GitLab::new(&api, &path, given) {
            Ok(gl) => gl.ensure_hook(&url, &secret, events).await,
            Err(e) => Err(e),
        },
    };
    let (hook, created) = match done {
        Ok(HookDone::Created(hook)) => (hook, true),
        Ok(HookDone::Updated(hook)) => (hook, false),
        Err(e) => {
            let said = hook_refusal(&e, forge, &path, given, &secret);
            tracing::info!(repo = %repo.slug, place = %form.place, why = %said, "webhook not made");
            return Err(WebError::new(StatusCode::BAD_GATEWAY, said));
        }
    };
    store
        .audit_webhook_made_by(repo.id, &form.place, hook, created, &access.actor)
        .await?;
    tracing::info!(repo = %repo.slug, place = %form.place, hook, created, by = %access.actor.name, "webhook made");
    let word = if created { "created" } else { "updated" };
    Ok(Redirect::to(&format!("/admin/repos/{id}/connection?made={word}")).into_response())
}

/// Why the forge would not make the webhook, said plainly; neither the
/// token nor the secret appears.
fn hook_refusal(e: &PrError, forge: &Forge, path: &str, token: &str, secret: &str) -> String {
    let status = match e {
        PrError::GitHub { status, .. } | PrError::GitLab { status, .. } => *status,
        _ => 0,
    };
    let said = match status {
        403 | 404 => format!(
            "the token may not manage the webhooks of `{path}`: it needs {}",
            forge.hook_right
        ),
        _ => refusal(e, &format!("the webhooks of `{path}`"), token),
    };
    said.replace(token, "***")
        .replace(secret, "***")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// What a forge's refusal means, said plainly; `secret` never appears.
fn refusal(e: &PrError, what: &str, secret: &str) -> String {
    let (status, message) = match e {
        PrError::GitHub { status, message } | PrError::GitLab { status, message } => {
            (*status, message.as_str())
        }
        _ => (0, ""),
    };
    let said = match status {
        401 => format!("the forge refused the token (wrong or expired) for {what}"),
        403 => format!("the token may not read {what}"),
        404 => format!("{what} is not there, or the token cannot see it"),
        0 => format!("the forge could not be reached for {what}: {e}"),
        _ => format!("the forge answered {status} for {what}: {message}"),
    };
    if secret.is_empty() {
        said
    } else {
        said.replace(secret, "***")
    }
}

async fn access(
    platform: ForkPlatform,
    api: &str,
    path: &str,
    token: &str,
) -> Result<RepoAccess, PrError> {
    match platform {
        ForkPlatform::GitHub => {
            let (owner, name) = path.split_once('/').unwrap_or((path, ""));
            GitHub::new(api, owner, name, token)?.access().await
        }
        ForkPlatform::GitLab => GitLab::new(api, path, token)?.access().await,
    }
}

/// The repo's path on its forge, from its URL.
fn upstream_path(platform: ForkPlatform, url: &str) -> Option<String> {
    match platform {
        ForkPlatform::GitHub => owner_and_name(url).map(|(o, n)| format!("{o}/{n}")),
        ForkPlatform::GitLab => project_path(url).map(str::to_owned),
    }
}

/// Lower case, without a trailing `/` or `.git`.
fn plain_url(url: &str) -> String {
    let lower = url.trim().to_ascii_lowercase();
    let bare = lower.trim_end_matches('/');
    bare.strip_suffix(".git").unwrap_or(bare).to_owned()
}

/// The checks against the forge's API, each said plainly.
pub(super) async fn run_checks(admin: &RepoAdmin, repo: &Repo) -> Vec<CheckRow> {
    let Ok(file) = RepoFile::of(repo) else {
        return vec![fail("Repo", "its settings do not read as a repo file")];
    };
    let fork = &file.settings.fork;
    let forge = forge_of(fork.platform);
    let mut rows = Vec::new();

    const FORK: &str = "Fork token";
    match token(admin, &fork.token_env).await {
        None => rows.push(fail(
            FORK,
            format!("the secret store has no `{}`", fork.token_env),
        )),
        Some(t) => match access(fork.platform, &fork.api, &fork.path(), &t).await {
            Err(e) => rows.push(fail(
                FORK,
                refusal(&e, &format!("the fork `{}`", fork.path()), &t),
            )),
            Ok(a) if a.archived => {
                rows.push(fail(FORK, format!("the fork `{}` is archived", a.path)))
            }
            Ok(a) if !a.push => rows.push(fail(
                FORK,
                format!(
                    "the token reads `{}` but may not push to it: give it write access to contents",
                    a.path
                ),
            )),
            Ok(a) if !a.path.eq_ignore_ascii_case(&fork.path()) => rows.push(fail(
                FORK,
                format!(
                    "the fork is now `{}` (renamed or moved): change the repo's fork",
                    a.path
                ),
            )),
            Ok(a) => rows.push(pass(
                FORK,
                format!("may push `agent/*` branches to `{}`", a.path),
            )),
        },
    }

    const COMMENT: &str = "Comment token";
    const URL: &str = "Repo URL";
    let api = match fork.platform {
        ForkPlatform::GitHub => admin.github_api.as_str(),
        ForkPlatform::GitLab => admin.gitlab_api.as_str(),
    };
    let Some(path) = upstream_path(fork.platform, &repo.url) else {
        rows.push(skip(COMMENT, "the repo's URL names no repository"));
        rows.push(fail(
            URL,
            format!("`{}` is not a {} repository URL", repo.url, forge.name),
        ));
        return rows;
    };
    let Some(t) = token(admin, forge.comment_token).await else {
        rows.push(fail(
            COMMENT,
            format!("the secret store has no `{}`", forge.comment_token),
        ));
        rows.push(skip(
            URL,
            format!("needs `{}` to ask the forge", forge.comment_token),
        ));
        return rows;
    };
    match access(fork.platform, api, &path, &t).await {
        Err(e) => {
            let why = refusal(&e, &format!("`{path}`"), &t);
            rows.push(fail(COMMENT, why.clone()));
            rows.push(fail(URL, why));
        }
        Ok(a) => {
            rows.push(if !a.issues_enabled {
                fail(COMMENT, format!("`{}` has its issues turned off", a.path))
            } else if !a.comment {
                fail(
                    COMMENT,
                    format!("the token may not comment on the issues of `{}`", a.path),
                )
            } else {
                pass(
                    COMMENT,
                    format!("may comment on the issues of `{}`", a.path),
                )
            });
            rows.push(if a.path.eq_ignore_ascii_case(&path) && plain_url(&a.web_url) == plain_url(&repo.url) {
                pass(URL, format!("`{}` is the repository on the forge", repo.url))
            } else {
                fail(
                    URL,
                    format!(
                        "the forge names it `{}` now (renamed or moved): change the repo's URL to `{}`",
                        a.path, a.web_url
                    ),
                )
            });
        }
    }
    rows
}
