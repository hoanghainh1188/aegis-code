//! An admin adds and changes repos from the pages (#330), checked as
//! `aegis-server repo add|update` checks them ([`RepoFile`], the modes'
//! configuration) and audited as the admin. The clone is made after, in
//! the background; the repo's page says whether it is being made, is
//! there, or failed, with the end of git's output.

use std::sync::Arc;

use aegis_lang::Language;
use aegis_lifecycle::{LifecycleError, Modes};
use aegis_secrets::Secrets;
use aegis_store::{CloneState, Repo, RepoClone, RepoId, Store, StoreError};
use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};

use super::admin::admin_only;
use super::repo_form::{RepoForm, file_toml};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;
use crate::repo_cli::{RepoFile, fork_token};

/// What adding and changing repos from the pages needs: the modes'
/// configuration a repo is checked against and cloned under, and the
/// secrets a private repo is cloned with. A server that has not loaded
/// its modes shows the repos but cannot add or change them.
#[derive(Clone)]
pub struct RepoAdmin {
    modes: Arc<Modes>,
    pub(super) secrets: Secrets,
    /// The API roots a repo's connection is checked against (#331):
    /// `AEGIS_GITHUB_API`, `AEGIS_GITLAB_API`, as the webhooks use them.
    pub(super) github_api: String,
    pub(super) gitlab_api: String,
}

impl RepoAdmin {
    pub fn new(modes: Modes, secrets: Secrets) -> Self {
        let api =
            |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
        Self {
            modes: Arc::new(modes),
            secrets,
            github_api: api(
                crate::webhook::GITHUB_API_ENV,
                crate::webhook::DEFAULT_GITHUB_API,
            ),
            gitlab_api: api(
                crate::webhook::GITLAB_API_ENV,
                crate::webhook::DEFAULT_GITLAB_API,
            ),
        }
    }

    /// With these API roots for the repos' own projects (tests).
    pub fn with_apis(self, github: &str, gitlab: &str) -> Self {
        Self {
            github_api: github.into(),
            gitlab_api: gitlab.into(),
            ..self
        }
    }
}

/// The admin pages' [`RepoAdmin`], when the server has one.
pub type RepoAdminExt = Extension<Option<RepoAdmin>>;

const MODES: [&str; 2] = ["poc", "customer-jp"];
const PLATFORMS: [&str; 2] = ["github", "gitlab"];

pub(super) fn editing(admin: Option<RepoAdmin>) -> Result<RepoAdmin, WebError> {
    admin.ok_or_else(|| {
        WebError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "this server has not loaded the modes' configuration ([modes]); \
             add and change repos with `aegis-server repo add|update`",
        )
    })
}

#[derive(Template)]
#[template(path = "admin_repo_form.html")]
struct FormPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    /// The repo being changed; `None` when one is added.
    repo: Option<&'a Repo>,
    form: &'a RepoForm,
    error: Option<&'a str>,
    languages: Vec<&'static str>,
    modes: [&'static str; 2],
    platforms: [&'static str; 2],
}

impl FormPage<'_> {
    fn action(&self) -> String {
        match self.repo {
            Some(r) => format!("/admin/repos/{}/edit", r.id.0),
            None => "/admin/repos".into(),
        }
    }
}

fn form_page(
    nonce: &Nonce,
    me: &Me,
    repo: Option<&Repo>,
    form: &RepoForm,
    error: Option<(StatusCode, &str)>,
) -> WebResult {
    let page = render(&FormPage {
        nonce: nonce.as_str(),
        me,
        repo,
        form,
        error: error.map(|(_, e)| e),
        languages: Language::ALL.iter().map(|l| l.id()).collect(),
        modes: MODES,
        platforms: PLATFORMS,
    })?;
    Ok(match error {
        Some((status, _)) => (status, page).into_response(),
        None => page,
    })
}

/// What is wrong with a repo, said on the form: what the admin can fix,
/// not database details.
fn refusal(e: LifecycleError) -> Result<(StatusCode, String), WebError> {
    match e {
        LifecycleError::Config(why) => Ok((StatusCode::BAD_REQUEST, why)),
        LifecycleError::Store(StoreError::Conflict(why)) => Ok((StatusCode::CONFLICT, why)),
        LifecycleError::Store(StoreError::Invalid { field, message }) => {
            Ok((StatusCode::BAD_REQUEST, format!("{field}: {message}")))
        }
        LifecycleError::Store(other) => Err(other.into()),
        other => {
            tracing::error!(error = %other, "web: repo");
            Err(WebError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error",
            ))
        }
    }
}

pub async fn new_form(
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
) -> WebResult {
    admin_only(&access)?;
    editing(admin)?;
    form_page(&nonce, &me, None, &RepoForm::blank(), None)
}

pub async fn create(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
    Form(form): Form<RepoForm>,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    let checked = form.to_file(None).and_then(|f| f.spec());
    let spec = match checked {
        Ok(spec) => spec,
        Err(why) => {
            return form_page(
                &nonce,
                &me,
                None,
                &form,
                Some((StatusCode::BAD_REQUEST, &why)),
            );
        }
    };
    let repo = match admin.modes.register_by(&store, &spec, &access.actor).await {
        Ok(repo) => repo,
        Err(e) => {
            let (status, why) = refusal(e)?;
            return form_page(&nonce, &me, None, &form, Some((status, &why)));
        }
    };
    clone_in_background(&store, &admin, &repo).await?;
    Ok(Redirect::to(&format!("/admin/repos/{}?done=added", repo.id.0)).into_response())
}

pub(super) async fn repo_of(store: &Store, id: i64) -> Result<Repo, WebError> {
    store
        .repo(RepoId(id))
        .await?
        .ok_or_else(|| WebError::not_found(format!("repo {id}")))
}

fn bad_repo(repo: &Repo, why: String) -> WebError {
    tracing::error!(repo = %repo.slug, error = %why, "web: stored repo does not read back");
    WebError::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

pub async fn edit_form(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
) -> WebResult {
    admin_only(&access)?;
    editing(admin)?;
    let repo = repo_of(&store, id).await?;
    let form = RepoFile::of(&repo)
        .and_then(|f| RepoForm::of(&f))
        .map_err(|e| bad_repo(&repo, e))?;
    form_page(&nonce, &me, Some(&repo), &form, None)
}

pub async fn update(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
    Form(form): Form<RepoForm>,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    let repo = repo_of(&store, id).await?;
    let checked = form.to_file(Some(&repo.slug)).and_then(|f| f.spec());
    let spec = match checked {
        Ok(spec) => spec,
        Err(why) => {
            let refused = Some((StatusCode::BAD_REQUEST, why.as_str()));
            return form_page(&nonce, &me, Some(&repo), &form, refused);
        }
    };
    let updated = match admin.modes.update_by(&store, &spec, &access.actor).await {
        Ok(repo) => repo,
        Err(e) => {
            let (status, why) = refusal(e)?;
            return form_page(&nonce, &me, Some(&repo), &form, Some((status, &why)));
        }
    };
    clone_in_background(&store, &admin, &updated).await?;
    Ok(Redirect::to(&format!("/admin/repos/{id}?done=updated")).into_response())
}

/// Records the clone of `repo` as being made, then makes it (or fetches
/// it) off the request: the page says how it went.
async fn clone_in_background(
    store: &Store,
    admin: &RepoAdmin,
    repo: &Repo,
) -> Result<(), WebError> {
    store.start_clone(repo.id).await?;
    let (store, admin, repo) = (store.clone(), admin.clone(), repo.clone());
    tokio::spawn(async move {
        let failed = make_clone(&admin, &repo).await.err();
        if let Some(why) = &failed {
            tracing::warn!(repo = %repo.slug, error = %why, "clone failed");
        }
        if let Err(e) = store
            .finish_clone(repo.id, failed.as_deref().map_or(Ok(()), Err))
            .await
        {
            tracing::error!(repo = %repo.slug, error = %e, "recording the clone");
        }
    });
    Ok(())
}

/// `repo`'s clone made or fetched; on failure, what git said, without
/// the token should git have echoed it.
async fn make_clone(admin: &RepoAdmin, repo: &Repo) -> Result<(), String> {
    let file = RepoFile::of(repo)?;
    let token = fork_token(&admin.secrets, &file.settings.fork.token_env).await?;
    admin
        .modes
        .clone_repo(repo, token.as_deref())
        .await
        .map_err(|e| match &token {
            Some(t) if !t.is_empty() => e.to_string().replace(t.as_str(), "***"),
            _ => e.to_string(),
        })
}

/// The clone part of a repo's page; while the clone is being made it
/// asks for itself again.
#[derive(Template)]
#[template(path = "admin_repo_clone.html")]
pub struct ClonePart {
    pub repo_id: i64,
    pub clone: Option<CloneView>,
}

pub struct CloneView {
    pub state: &'static str,
    /// [`CloneState::as_str`], for the page's style.
    pub class: &'static str,
    pub live: bool,
    pub failed: bool,
    pub log_tail: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

impl From<RepoClone> for CloneView {
    fn from(c: RepoClone) -> Self {
        let at = |t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%d %H:%M:%S").to_string();
        Self {
            state: match c.state {
                CloneState::Cloning => "being made",
                CloneState::Done => "ready",
                CloneState::Failed => "failed",
            },
            class: c.state.as_str(),
            live: c.state == CloneState::Cloning,
            failed: c.state == CloneState::Failed,
            log_tail: c.log_tail,
            started_at: at(c.started_at),
            finished_at: c.finished_at.map(at),
        }
    }
}

pub async fn clone_part_of(store: &Store, repo: RepoId) -> Result<ClonePart, WebError> {
    Ok(ClonePart {
        repo_id: repo.0,
        clone: store.repo_clone(repo).await?.map(CloneView::from),
    })
}

pub async fn clone_part(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    admin_only(&access)?;
    let repo = repo_of(&store, id).await?;
    render(&clone_part_of(&store, repo.id).await?)
}

/// `repo` as the repo file `aegis-server repo show` prints, in TOML; a
/// row that does not read as one (written before its checks) is shown as
/// stored, with why.
pub fn settings_of(repo: &Repo) -> String {
    match RepoFile::of(repo).and_then(|f| file_toml(&f)) {
        Ok(toml) => toml,
        Err(why) => format!(
            "# not a repo file: {why}\n# as stored:\n{}",
            serde_json::to_string_pretty(&repo.config).unwrap_or_default()
        ),
    }
}
