//! People and services for the admin (#369): who they are, their roles
//! repo by repo, and the admin right, given or taken as `aegis-server
//! grant|revoke <who> admin` does ([`Store::set_admin`], audited as the
//! admin). The last admin keeps the right (the store refuses), and an
//! admin does not take their own here: another admin does. Only an admin:
//! for anyone else these pages are not found.

use std::collections::HashMap;

use aegis_store::{Page, RepoId, Store, UserId, UserKind, UserSummary};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::admin::admin_only;
use super::dashboard::all_repos;
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;
use crate::auth::cli::kind_of;

const PAGE_SIZE: u32 = 50;
const MAX_SEARCH_LEN: usize = 320;

struct UserRow {
    id: i64,
    name: String,
    service: bool,
    admin: bool,
    repos: i64,
    seen: String,
}

fn seen(u: &UserSummary) -> String {
    match (u.user.kind, u.signed_in, u.last_session) {
        (UserKind::Service, _, _) => "API tokens".into(),
        (_, _, Some(at)) => format!("signed in {}", at.format("%Y-%m-%d %H:%M")),
        (_, true, None) => "signed in before".into(),
        (_, false, None) => "never signed in".into(),
    }
}

impl From<&UserSummary> for UserRow {
    fn from(u: &UserSummary) -> Self {
        Self {
            id: u.user.id.0,
            name: u.user.name.clone(),
            service: u.user.kind == UserKind::Service,
            admin: u.user.is_admin,
            repos: u.repos,
            seen: seen(u),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    cursor: String,
}

#[derive(Template)]
#[template(path = "admin_users.html")]
struct UsersPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    q: &'a str,
    rows: Vec<UserRow>,
    next: Option<String>,
}

/// Percent-encodes `value` for a query string.
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn bad(what: &str) -> WebError {
    WebError::new(
        StatusCode::BAD_REQUEST,
        format!("invalid filter value: {what}"),
    )
}

pub async fn list(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Query(q): Query<ListQuery>,
) -> WebResult {
    admin_only(&access)?;
    let search = q.q.trim();
    if search.len() > MAX_SEARCH_LEN {
        return Err(bad("search"));
    }
    let cursor = match q.cursor.trim() {
        "" => None,
        c => Some(c.parse::<i64>().map_err(|_| bad("cursor"))?),
    };
    let page = cursor.map_or(Page::first(PAGE_SIZE), |c| Page::after(c, PAGE_SIZE));
    let users = store
        .list_users((!search.is_empty()).then_some(search), page)
        .await?;
    let next = users
        .next_page()
        .and_then(Page::cursor)
        .map(|c| format!("/admin/users?q={}&cursor={c}", encoded(search)));
    render(&UsersPage {
        nonce: nonce.as_str(),
        me: &me,
        q: search,
        rows: users.items.iter().map(UserRow::from).collect(),
        next,
    })
}

struct RoleRow {
    repo_id: i64,
    repo: String,
    role: &'static str,
}

#[derive(Template)]
#[template(path = "admin_user.html")]
struct UserPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    user: UserRow,
    created_at: String,
    roles: Vec<RoleRow>,
    /// The page's own admin: they do not take their own right here.
    is_self: bool,
    audit_link: String,
    notice: Option<&'static str>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UserQuery {
    #[serde(default)]
    done: String,
}

async fn summary_of(store: &Store, id: i64) -> Result<UserSummary, WebError> {
    store
        .user_summary(UserId(id))
        .await?
        .ok_or_else(|| WebError::not_found(format!("user {id}")))
}

pub async fn detail(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Path(id): Path<i64>,
    Query(q): Query<UserQuery>,
) -> WebResult {
    admin_only(&access)?;
    let user = summary_of(&store, id).await?;
    let repos = all_repos(&store).await?;
    let slugs: HashMap<RepoId, &str> = repos.iter().map(|(id, r)| (*id, r.slug.as_str())).collect();
    let roles = store
        .repo_roles(user.user.id)
        .await?
        .into_iter()
        .map(|(repo, role)| RoleRow {
            repo_id: repo.0,
            repo: slugs
                .get(&repo)
                .map_or_else(|| format!("repo {}", repo.0), |s| (*s).to_owned()),
            role: role.as_str(),
        })
        .collect();
    // A fixed message per word, never the request's own text.
    let notice = match q.done.as_str() {
        "granted" => Some("Now an admin; it applies from their next request."),
        "revoked" => Some("No longer an admin; it applies from their next request."),
        _ => None,
    };
    render(&UserPage {
        nonce: nonce.as_str(),
        me: &me,
        is_self: access.actor.user == Some(user.user.id),
        audit_link: format!("/admin/audit?actor={}", encoded(&user.user.name)),
        created_at: user.created_at.format("%Y-%m-%d %H:%M").to_string(),
        user: UserRow::from(&user),
        roles,
        notice,
    })
}

#[derive(Debug, Deserialize)]
pub struct AdminForm {
    admin: String,
}

/// Gives or takes the admin right of user `id`.
pub async fn set_admin(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
    Form(form): Form<AdminForm>,
) -> WebResult {
    admin_only(&access)?;
    let user = summary_of(&store, id).await?.user;
    let admin = match form.admin.as_str() {
        "true" => true,
        "false" => false,
        _ => {
            return Err(WebError::new(
                StatusCode::BAD_REQUEST,
                "admin: true or false",
            ));
        }
    };
    if !admin && access.actor.user == Some(user.id) {
        return Err(WebError::new(
            StatusCode::CONFLICT,
            "you cannot take your own admin right here; ask another admin",
        ));
    }
    store
        .set_admin(user.kind, &user.name, admin, &access.actor)
        .await?;
    let done = if admin { "granted" } else { "revoked" };
    Ok(Redirect::to(&format!("/admin/users/{id}?done={done}")).into_response())
}

#[derive(Debug, Deserialize)]
pub struct NewAdminForm {
    who: String,
}

/// Makes `who` an admin, created when missing: a person may be named
/// before their first sign-in.
pub async fn add_admin(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Form(form): Form<NewAdminForm>,
) -> WebResult {
    admin_only(&access)?;
    let who = form.who.trim();
    let user = store
        .set_admin(kind_of(who), who, true, &access.actor)
        .await?;
    Ok(Redirect::to(&format!("/admin/users/{}?done=granted", user.id.0)).into_response())
}
