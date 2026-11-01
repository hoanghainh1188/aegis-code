//! The admin pages (#329): every registered repo, and who has which role
//! in each, changed from here as `aegis-server grant|revoke` changes it
//! ([`Store::set_repo_role`], which checks the name and audits). Only an
//! admin: for anyone else `/admin/*` is not found, page or form.

use aegis_store::{Repo, RepoId, RepoMember, RepoRole, Store, UserKind};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::dashboard::all_repos;
use super::repo_admin::{RepoAdminExt, clone_part_of, settings_of};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;
use crate::auth::cli::kind_of;

/// The roles a form may give, as `grant` takes them; the least first, so
/// the form starts on it.
const ROLES: [&str; 3] = ["viewer", "reviewer", "maintainer"];

pub(crate) fn admin_only(access: &Access) -> Result<(), WebError> {
    if access.is_admin() {
        Ok(())
    } else {
        // As if the pages did not exist.
        Err(WebError::not_found("page"))
    }
}

async fn repo_of(store: &Store, id: i64) -> Result<Repo, WebError> {
    store
        .repo(RepoId(id))
        .await?
        .ok_or_else(|| WebError::not_found(format!("repo {id}")))
}

#[derive(Template)]
#[template(path = "admin_repos.html")]
struct ReposPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    repos: Vec<Repo>,
    /// Whether repos can be added here (#330).
    can_edit: bool,
}

pub async fn repos(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
) -> WebResult {
    admin_only(&access)?;
    render(&ReposPage {
        nonce: nonce.as_str(),
        me: &me,
        repos: all_repos(&store).await?.into_values().collect(),
        can_edit: admin.is_some(),
    })
}

struct MemberRow {
    name: String,
    service: bool,
    role: &'static str,
    granted_at: String,
}

impl From<RepoMember> for MemberRow {
    fn from(m: RepoMember) -> Self {
        Self {
            service: m.kind == UserKind::Service,
            name: m.name,
            role: m.role.as_str(),
            granted_at: m.granted_at.format("%Y-%m-%d %H:%M").to_string(),
        }
    }
}

#[derive(Template)]
#[template(path = "admin_repo.html")]
struct RepoPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    repo: Repo,
    members: Vec<MemberRow>,
    roles: [&'static str; 3],
    notice: Option<&'static str>,
    /// The repo as a repo file, in TOML (#330).
    settings: String,
    /// The clone part, rendered: it also answers alone while polled.
    clone: String,
    can_edit: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoQuery {
    #[serde(default)]
    done: String,
}

pub async fn repo(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
    Query(q): Query<RepoQuery>,
) -> WebResult {
    admin_only(&access)?;
    let repo = repo_of(&store, id).await?;
    let members = store.repo_members(repo.id).await?;
    // A fixed message per word, never the request's own text.
    let notice = match q.done.as_str() {
        "granted" => Some("Role given; it applies from their next request."),
        "revoked" => Some("Role taken away; it applies from their next request."),
        "added" => Some("Repo added; its clone is being made below."),
        "updated" => Some("Repo changed; its clone is being fetched below."),
        _ => None,
    };
    let settings = settings_of(&repo);
    let clone = clone_part_of(&store, repo.id)
        .await?
        .render()
        .map_err(|e| {
            tracing::error!(error = %e, "web: clone part");
            WebError::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?;
    render(&RepoPage {
        nonce: nonce.as_str(),
        me: &me,
        repo,
        members: members.into_iter().map(MemberRow::from).collect(),
        roles: ROLES,
        notice,
        settings,
        clone,
        can_edit: admin.is_some(),
    })
}

#[derive(Debug, Deserialize)]
pub struct GrantForm {
    who: String,
    role: String,
}

pub async fn grant(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
    Form(form): Form<GrantForm>,
) -> WebResult {
    admin_only(&access)?;
    let repo = repo_of(&store, id).await?;
    let role = RepoRole::parse(&form.role).ok_or_else(|| {
        WebError::new(
            StatusCode::BAD_REQUEST,
            format!("unknown role `{}`: maintainer, reviewer, viewer", form.role),
        )
    })?;
    let who = form.who.trim();
    store
        .set_repo_role(kind_of(who), who, repo.id, Some(role), &access.actor)
        .await?;
    Ok(Redirect::to(&format!("/admin/repos/{id}?done=granted")).into_response())
}

#[derive(Debug, Deserialize)]
pub struct RevokeForm {
    who: String,
}

pub async fn revoke(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
    Form(form): Form<RevokeForm>,
) -> WebResult {
    admin_only(&access)?;
    let repo = repo_of(&store, id).await?;
    let who = form.who.trim();
    store
        .set_repo_role(kind_of(who), who, repo.id, None, &access.actor)
        .await?;
    Ok(Redirect::to(&format!("/admin/repos/{id}?done=revoked")).into_response())
}
