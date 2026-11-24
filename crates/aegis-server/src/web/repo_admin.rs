//! An admin adds and changes repos from the pages (#330), checked as
//! `aegis-server repo add|update` checks them ([`RepoFile`], the modes'
//! configuration) and audited as the admin. The clone is made after, in
//! the background; the repo's page says whether it is being made, is
//! there, or failed, with the end of git's output.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

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
use crate::repo_cli::RepoFile;

/// What adding and changing repos from the pages needs: the modes'
/// configuration a repo is checked against and cloned under, and the
/// secrets a private repo is cloned with. A server that has not loaded
/// its modes shows the repos but cannot add or change them.
#[derive(Clone)]
pub struct RepoAdmin {
    modes: Arc<Modes>,
    pub(super) secrets: Secrets,
    /// The API roots a repo's connection is checked against (#331):
    /// `AEGIS_GITHUB_API`, `AEGIS_GITLAB_API`, `AEGIS_BITBUCKET_API`, as
    /// the webhooks use them.
    pub(super) github_api: String,
    pub(super) gitlab_api: String,
    pub(super) bitbucket_api: String,
    /// `AEGIS_BITBUCKET_DC_API`; empty when the instance has no Bitbucket
    /// Data Center (#635).
    pub(super) bitbucket_dc_api: String,
    /// `AEGIS_AZURE_DEVOPS_URL`; empty when the instance has no Azure
    /// DevOps organization (#636).
    pub(super) azure_devops_url: String,
    /// When each mode's provider was last checked (#367), to check at
    /// most once a minute: each check costs a little.
    /// When each mode was last checked, by [`aegis_store::ModeName::key`].
    pub(super) ai_checks: Arc<std::sync::Mutex<HashMap<String, Instant>>>,
    /// `[web] make_webhooks` (#376).
    pub(super) make_webhooks: bool,
    /// The instance's GitHub App (ADR 0017), for cloning repos on the app
    /// path.
    pub(super) github_app: Option<Arc<crate::github_app::AppHandle>>,
    /// Slots of the pre-flight checks run from the pages (#599).
    pub(super) preflights: Arc<tokio::sync::Semaphore>,
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
            bitbucket_api: api(crate::webhook::BITBUCKET_API_ENV, aegis_pr::BITBUCKET_API),
            bitbucket_dc_api: api(crate::webhook::BITBUCKET_DC_API_ENV, ""),
            azure_devops_url: api(crate::webhook::AZURE_DEVOPS_URL_ENV, ""),
            ai_checks: Arc::default(),
            make_webhooks: false,
            github_app: None,
            preflights: super::preflight::slots(),
        }
    }

    /// With the instance's GitHub App (ADR 0017).
    pub fn with_github_app(self, app: Option<Arc<crate::github_app::AppHandle>>) -> Self {
        Self {
            github_app: app,
            ..self
        }
    }

    /// Whether an admin may make a repo's webhooks from its connection
    /// page (#376).
    pub fn with_webhook_making(self, on: bool) -> Self {
        Self {
            make_webhooks: on,
            ..self
        }
    }

    /// The instance's GitHub App, shared with the webhooks (ADR 0017).
    pub fn github_app(&self) -> Option<Arc<crate::github_app::AppHandle>> {
        self.github_app.clone()
    }

    /// The modes' configuration the server loaded.
    /// The modes, shared with the webhooks (#456).
    pub fn shared_modes(&self) -> Arc<Modes> {
        Arc::clone(&self.modes)
    }

    /// What syncs a repo's runbooks and business rules (#660), with the
    /// modes, secrets and GitHub App of these pages.
    pub fn memory_sync(&self) -> crate::memory_sync::MemorySync {
        crate::memory_sync::MemorySync {
            modes: Arc::clone(&self.modes),
            secrets: self.secrets.clone(),
            app: self.github_app.clone(),
        }
    }

    pub(super) fn modes(&self) -> &Modes {
        &self.modes
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
const PLATFORMS: [&str; 5] = [
    "github",
    "gitlab",
    "bitbucket",
    "bitbucket-dc",
    "azure-devops",
];

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
    platforms: [&'static str; 5],
    connections: [&'static str; 2],
    /// Repos the GitHub App is installed on and no repo is registered
    /// for, as clone URLs with the same URL encoded for a query (ADR 0017
    /// §5).
    installed: Vec<(String, String)>,
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
    installed: Vec<(String, String)>,
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
        connections: super::repo_form::CONNECTIONS,
        installed,
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

/// What a link from the list of installed repos fills the form with.
#[derive(Debug, Default, serde::Deserialize)]
pub struct Prefill {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    connection: Option<String>,
}

pub async fn new_form(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
    axum::extract::Query(prefill): axum::extract::Query<Prefill>,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    let mut form = RepoForm::blank();
    if let Some(url) = prefill.url.filter(|u| u.len() <= 512) {
        form.slug = aegis_lifecycle::upstream_of(&url)
            .map(|(_, name)| name)
            .unwrap_or_default();
        form.url = url;
    }
    if let Some(c) = prefill
        .connection
        .filter(|c| super::repo_form::CONNECTIONS.contains(&c.as_str()))
    {
        form.connection = c;
    }
    let installed = match &admin.github_app {
        Some(app) => {
            let web = web_root(app.api());
            store
                .installed_unregistered()
                .await?
                .into_iter()
                .map(|r| {
                    let url = format!("{web}/{r}");
                    let query = openidconnect::url::form_urlencoded::byte_serialize(url.as_bytes())
                        .collect();
                    (url, query)
                })
                .collect()
        }
        None => Vec::new(),
    };
    form_page(&nonce, &me, None, &form, None, installed)
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
                Vec::new(),
            );
        }
    };
    // The pages register in the default tenant.
    let tenant = spec
        .tenant
        .as_ref()
        .map_or(aegis_store::TenantId::DEFAULT, |t| t.id);
    if let Err(why) =
        crate::github_app::check_binding(&store, &spec.settings, &spec.url, tenant).await
    {
        let refused = Some((StatusCode::BAD_REQUEST, why.as_str()));
        return form_page(&nonce, &me, None, &form, refused, Vec::new());
    }
    let repo = match admin.modes.register_by(&store, &spec, &access.actor).await {
        Ok(repo) => repo,
        Err(e) => {
            let (status, why) = refusal(e)?;
            return form_page(&nonce, &me, None, &form, Some((status, &why)), Vec::new());
        }
    };
    // It takes no job until its pre-flight check passes and an admin
    // turns it on (#599); the check starts once its clone is made.
    store
        .pause_repo_by(
            repo.id,
            Some(super::preflight::AWAITING_PREFLIGHT),
            false,
            &access.actor,
        )
        .await?;
    clone_in_background(&store, &admin, &repo, Some(&access.actor)).await?;
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
    form_page(&nonce, &me, Some(&repo), &form, None, Vec::new())
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
            return form_page(&nonce, &me, Some(&repo), &form, refused, Vec::new());
        }
    };
    let tenant = spec.tenant.as_ref().map_or(repo.tenant_id, |t| t.id);
    if let Err(why) =
        crate::github_app::check_binding(&store, &spec.settings, &spec.url, tenant).await
    {
        let refused = Some((StatusCode::BAD_REQUEST, why.as_str()));
        return form_page(&nonce, &me, Some(&repo), &form, refused, Vec::new());
    }
    let updated = match admin.modes.update_by(&store, &spec, &access.actor).await {
        Ok(repo) => repo,
        Err(e) => {
            let (status, why) = refusal(e)?;
            return form_page(
                &nonce,
                &me,
                Some(&repo),
                &form,
                Some((status, &why)),
                Vec::new(),
            );
        }
    };
    clone_in_background(&store, &admin, &updated, None).await?;
    Ok(Redirect::to(&format!("/admin/repos/{id}?done=updated")).into_response())
}

/// Records the clone of `repo` as being made, then makes it (or fetches
/// it) off the request: the page says how it went.
/// With `then_check`, a pre-flight check by that actor follows a clone
/// that is made (#599).
async fn clone_in_background(
    store: &Store,
    admin: &RepoAdmin,
    repo: &Repo,
    then_check: Option<&aegis_store::Actor>,
) -> Result<(), WebError> {
    store.start_clone(repo.id).await?;
    let (store, admin, repo) = (store.clone(), admin.clone(), repo.clone());
    let then_check = then_check.cloned();
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
        if let (None, Some(actor)) = (&failed, then_check)
            && let Err(e) = super::preflight::start(&store, &admin, &repo, &actor).await
        {
            tracing::error!(repo = %repo.slug, error = ?e, "starting the pre-flight check");
        }
    });
    Ok(())
}

/// `repo`'s clone made or fetched; on failure, what git said, without
/// the token should git have echoed it.
async fn make_clone(admin: &RepoAdmin, repo: &Repo) -> Result<(), String> {
    let file = RepoFile::of(repo)?;
    let token = crate::github_app::read_token(
        &admin.secrets,
        admin.github_app.as_deref(),
        &file.settings,
        &repo.url,
        repo.tenant_id,
    )
    .await?;
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
        let at = |t: time::OffsetDateTime| crate::when::second(t);
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

/// The web root of a GitHub API root: `https://api.github.com` →
/// `https://github.com`, GHES' `https://<host>/api/v3` → `https://<host>`.
fn web_root(api: &str) -> String {
    let api = api.trim_end_matches('/');
    if let Some(host) = api.strip_prefix("https://api.") {
        return format!("https://{host}");
    }
    api.strip_suffix("/api/v3").unwrap_or(api).to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_web_root_of_an_api_root() {
        assert_eq!(
            super::web_root("https://api.github.com"),
            "https://github.com"
        );
        assert_eq!(
            super::web_root("https://api.github.com/"),
            "https://github.com"
        );
        assert_eq!(
            super::web_root("https://ghe.acme.test/api/v3"),
            "https://ghe.acme.test"
        );
    }
}
