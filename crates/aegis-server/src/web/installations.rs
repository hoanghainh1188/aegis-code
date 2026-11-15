//! The GitHub App's installations (ADR 0017 §6, #541): a platform admin
//! binds each to the tenant whose org it is on, as `aegis-server tenant
//! bind-installation` does. An installation bound to no tenant gets no
//! token; a bound one only for its tenant's repos. Audited. Only a
//! platform admin: for anyone else the page and its form are not found.

use aegis_store::{Store, StoreError, Tenant, TenantId};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::admin::admin_only;
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

struct Row {
    id: i64,
    account: String,
    state: &'static str,
    repos: String,
    /// The slug of the tenant it is bound to.
    tenant: Option<String>,
}

#[derive(Template)]
#[template(path = "admin_installations.html")]
struct InstallationsPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    rows: Vec<Row>,
    tenants: Vec<Tenant>,
    notice: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct Done {
    done: Option<String>,
}

fn notice_of(done: Option<&str>) -> Option<&'static str> {
    match done? {
        "bound" => Some("Installation bound: its repos' tokens are for that tenant only."),
        "unbound" => Some("Installation unbound: no token is minted for it until it is bound."),
        _ => None,
    }
}

fn slug_of(tenants: &[Tenant], id: TenantId) -> String {
    tenants
        .iter()
        .find(|t| t.id == id)
        .map_or_else(|| id.0.to_string(), |t| t.slug.clone())
}

pub async fn list(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Query(q): Query<Done>,
) -> WebResult {
    admin_only(&access)?;
    let tenants = store.tenants().await?;
    let rows = store
        .installations()
        .await?
        .into_iter()
        .map(|i| Row {
            id: i.id,
            state: match (i.removed, i.suspended) {
                (true, _) => "removed",
                (false, true) => "suspended",
                (false, false) => "installed",
            },
            repos: match i.selection {
                aegis_store::RepoSelection::All => "all its repos".into(),
                aegis_store::RepoSelection::Selected => i.repositories.join(", "),
            },
            tenant: i.tenant.map(|t| slug_of(&tenants, t)),
            account: i.account,
        })
        .collect();
    render(&InstallationsPage {
        nonce: nonce.as_str(),
        me: &me,
        rows,
        tenants,
        notice: notice_of(q.done.as_deref()),
    })
}

#[derive(Debug, Deserialize)]
pub struct BindForm {
    /// A tenant's slug; empty unbinds.
    #[serde(default)]
    tenant: String,
}

pub async fn bind(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
    Form(form): Form<BindForm>,
) -> WebResult {
    admin_only(&access)?;
    let slug = form.tenant.trim();
    let tenant = if slug.is_empty() {
        None
    } else {
        let t = store
            .tenant_by_slug(slug)
            .await?
            .ok_or_else(|| WebError::new(StatusCode::BAD_REQUEST, format!("no tenant `{slug}`")))?;
        Some(t.id)
    };
    match store.bind_installation_by(id, tenant, &access.actor).await {
        Ok(()) => {}
        Err(StoreError::Conflict(why)) => return Err(WebError::new(StatusCode::CONFLICT, why)),
        Err(StoreError::NotFound(what)) => return Err(WebError::not_found(what)),
        Err(e) => return Err(e.into()),
    }
    tracing::info!(installation = id, tenant = slug, by = %access.actor.name, "installation bound");
    let done = if tenant.is_some() { "bound" } else { "unbound" };
    Ok(Redirect::to(&format!("/admin/installations?done={done}")).into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_known_outcomes_have_a_notice() {
        assert!(super::notice_of(Some("bound")).is_some());
        assert!(super::notice_of(Some("unbound")).is_some());
        assert_eq!(super::notice_of(Some("<b>")), None);
    }
}
