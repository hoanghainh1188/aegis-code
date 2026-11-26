//! The pilot report's page (#700): a repo or every repo the person sees,
//! over a period, against the period before it and ADR 0005's thresholds,
//! with the same report to download as Markdown or CSV.

use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;
use crate::pilot_report::{PilotQuery, PilotReport, pilot_report};
use aegis_store::Store;

/// The form's fields, as text: a bad value is said on the page.
#[derive(Debug, Default, Deserialize)]
pub struct PilotForm {
    #[serde(default)]
    repo_id: String,
    #[serde(default)]
    since: String,
    #[serde(default)]
    until: String,
}

pub(crate) struct RepoChoice {
    pub id: i64,
    pub slug: String,
    pub chosen: bool,
}

#[derive(Template)]
#[template(path = "pilot_report.html")]
struct PilotPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    repos: Vec<RepoChoice>,
    since: String,
    until: String,
    error: Option<String>,
    report: Option<PilotReport>,
    /// The query string of the downloads.
    query: String,
}

fn today() -> time::Date {
    OffsetDateTime::now_utc().date()
}

/// The query `form` says: the last 7 days when it gives no start.
fn query_of(form: &PilotForm) -> Result<PilotQuery, String> {
    let repo = match form.repo_id.trim() {
        "" => None,
        id => Some(
            id.parse::<i64>()
                .map_err(|_| format!("repo: `{id}` is not a repo id"))?,
        ),
    };
    let week_ago = (today() - Duration::days(6)).to_string();
    let since = match form.since.trim() {
        "" => week_ago.as_str(),
        s => s,
    };
    PilotQuery::parse(repo, Some(since), Some(form.until.as_str()), today())
}

async fn report_of(
    store: &Store,
    access: &Access,
    form: &PilotForm,
) -> Result<Result<(PilotQuery, PilotReport), String>, WebError> {
    let query = match query_of(form) {
        Ok(q) => q,
        Err(why) => return Ok(Err(why)),
    };
    let within = access.visible();
    match pilot_report(store, within.as_deref(), &query).await {
        Ok(report) => Ok(Ok((query, report))),
        Err(aegis_store::StoreError::NotFound(_)) => Ok(Err("no such repo you can see".into())),
        Err(e) => Err(e.into()),
    }
}

pub async fn page(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Query(form): Query<PilotForm>,
) -> WebResult {
    let visible = access.visible();
    let repos: Vec<RepoChoice> = super::dashboard::all_repos(&store)
        .await?
        .into_values()
        .filter(|r| visible.as_ref().is_none_or(|v| v.contains(&r.id)))
        .map(|r| RepoChoice {
            chosen: form.repo_id.trim() == r.id.0.to_string(),
            id: r.id.0,
            slug: r.slug,
        })
        .collect();
    let (report, error, since, until, query) = match report_of(&store, &access, &form).await? {
        Ok((q, report)) => {
            let last = q.until - Duration::days(1);
            let query = format!(
                "since={}&until={}{}",
                q.since,
                last,
                q.repo
                    .map_or(String::new(), |r| format!("&repo_id={}", r.0))
            );
            (
                Some(report),
                None,
                q.since.to_string(),
                last.to_string(),
                query,
            )
        }
        Err(why) => (
            None,
            Some(why),
            form.since.clone(),
            form.until.clone(),
            String::new(),
        ),
    };
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let html = render(&PilotPage {
        nonce: nonce.as_str(),
        me: &me,
        repos,
        since,
        until,
        error,
        report,
        query,
    })?;
    Ok((status, html).into_response())
}

/// The report to download: `markdown` or `csv`.
async fn download(store: &Store, access: &Access, form: &PilotForm, csv: bool) -> WebResult {
    let (query, report) = match report_of(store, access, form).await? {
        Ok(r) => r,
        Err(why) => return Err(WebError::new(StatusCode::UNPROCESSABLE_ENTITY, why)),
    };
    let (body, kind, ext) = if csv {
        (report.csv(), "text/csv; charset=utf-8", "csv")
    } else {
        (report.markdown(), "text/markdown; charset=utf-8", "md")
    };
    let last = query.until - Duration::days(1);
    let name = format!("pilot-report-{}-{last}.{ext}", query.since);
    Ok((
        [
            (header::CONTENT_TYPE, kind.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response())
}

pub async fn markdown(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Query(form): Query<PilotForm>,
) -> WebResult {
    download(&store, &access, &form, false).await
}

pub async fn csv(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Query(form): Query<PilotForm>,
) -> WebResult {
    download(&store, &access, &form, true).await
}
