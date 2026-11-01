//! A repo's monthly cap (#372), read and changed from the pages as
//! `GET/PUT /api/v1/repos/{id}/budget` reads and changes it: through
//! [`Store::set_monthly_budget_by`], which checks the sum and audits the
//! cap before and after. A maintainer of the repo or an admin changes it,
//! anyone with a role reads it, and for the rest it does not exist.

use aegis_store::{RepoId, RepoRole, Store};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::view::{BudgetRow, dollars};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

/// `repo` when `access` may do in it what `needed` allows: not found when
/// it cannot see the repo, refused when it sees it but may not do this.
fn repo_for(access: &Access, id: i64, needed: RepoRole) -> Result<RepoId, WebError> {
    let repo = RepoId(id);
    if !access.can(repo, RepoRole::Viewer) {
        return Err(WebError::not_found(format!("repo {id}")));
    }
    if !access.can(repo, needed) {
        return Err(WebError::new(
            StatusCode::FORBIDDEN,
            format!("you need role {} in repo {id}", needed.as_str()),
        ));
    }
    Ok(repo)
}

#[derive(Template)]
#[template(path = "repo_budget.html")]
struct BudgetPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    id: i64,
    row: BudgetRow,
    /// The cap as the form starts with it, `25.50`; empty for none.
    cap_value: String,
    time_zone: &'a str,
    reserve: String,
    can_edit: bool,
    notice: Option<&'static str>,
    /// Said when the cap leaves less than a job's reserve.
    waits: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct BudgetQuery {
    #[serde(default)]
    done: String,
}

pub async fn show(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Path(id): Path<i64>,
    Query(q): Query<BudgetQuery>,
) -> WebResult {
    let repo = repo_for(&access, id, RepoRole::Viewer)?;
    let budget = store
        .repo_budget(repo)
        .await?
        .ok_or_else(|| WebError::not_found(format!("repo {id}")))?;
    let rules = store.budget_rules();
    let row = BudgetRow::new(&budget, rules);
    render(&BudgetPage {
        nonce: nonce.as_str(),
        me: &me,
        id,
        waits: row.class == "bad",
        cap_value: budget
            .monthly_usd
            .map_or_else(String::new, |c| format!("{c:.2}")),
        row,
        time_zone: &rules.time_zone,
        reserve: dollars(rules.job_reserve_usd),
        can_edit: access.can(repo, RepoRole::Maintainer),
        // A fixed message per word, never the request's own text.
        notice: (q.done == "saved").then_some("Cap saved; it applies to the next job."),
    })
}

#[derive(Debug, Deserialize)]
pub struct BudgetForm {
    /// Dollars a month; empty for no cap of its own.
    #[serde(default)]
    monthly_usd: String,
}

/// The form's cap: `None` when empty, refused when not a finite number.
/// What is negative is left to the store, which says so as the API does.
fn parse_cap(value: &str) -> Result<Option<f64>, WebError> {
    let value = value.trim().trim_start_matches('$');
    if value.is_empty() {
        return Ok(None);
    }
    match value.parse::<f64>() {
        Ok(cap) if cap.is_finite() => Ok(Some(cap)),
        _ => Err(WebError::new(
            StatusCode::BAD_REQUEST,
            format!("monthly_usd: `{value}` is not a number of US dollars"),
        )),
    }
}

pub async fn set(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
    Form(form): Form<BudgetForm>,
) -> WebResult {
    let repo = repo_for(&access, id, RepoRole::Maintainer)?;
    let cap = parse_cap(&form.monthly_usd)?;
    store
        .set_monthly_budget_by(repo, cap, &access.actor)
        .await?;
    Ok(Redirect::to(&format!("/repos/{id}/budget?done=saved")).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cap_is_dollars_or_nothing() {
        assert_eq!(parse_cap("").unwrap(), None);
        assert_eq!(parse_cap("  ").unwrap(), None);
        assert_eq!(parse_cap("25.5").unwrap(), Some(25.5));
        assert_eq!(parse_cap(" $40 ").unwrap(), Some(40.0));
        // Negative: the store refuses it, with the API's words.
        assert_eq!(parse_cap("-1").unwrap(), Some(-1.0));
        for bad in ["abc", "inf", "NaN", "1,5"] {
            assert!(parse_cap(bad).is_err(), "{bad}");
        }
    }
}
