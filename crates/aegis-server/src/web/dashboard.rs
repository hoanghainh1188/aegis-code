//! `/`: the KPIs of the ledger in total, per language, per repo and per
//! week. The total is `aegis report`'s "All runs"; the groups use the same
//! formula ([`Kpi::grouped`]) on the same records.

use std::collections::BTreeMap;

use aegis_ledger::{ACCEPT_RATE_MIN, IssueRecord, Kpi, LANGUAGE_ACCEPT_RATE_MIN, report};
use aegis_store::{Job, JobFilter, Page, Repo, RepoId, Store};
use askama::Template;
use axum::Extension;
use axum::extract::State;

use super::view::KpiRow;
use super::{Nonce, WebError, WebResult, render};

/// Weeks the dashboard shows, the latest last.
const WEEKS_SHOWN: usize = 12;
/// Runs not started by a job: `aegis run`, eval and CLI runs.
pub const OUTSIDE_JOBS: &str = "ngoài job (CLI, eval)";

/// The KPIs of one group of runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub label: String,
    pub kpi: Kpi,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dashboard {
    pub total: Kpi,
    pub languages: Vec<Group>,
    pub repos: Vec<Group>,
    /// By the ISO week the job was created; jobs only.
    pub weeks: Vec<Group>,
}

async fn all_jobs(store: &Store) -> Result<Vec<Job>, WebError> {
    let mut jobs = Vec::new();
    let mut page = Page::first(Page::MAX_LIMIT);
    loop {
        let batch = store.list_jobs(&JobFilter::default(), page).await?;
        let next = batch.next_page();
        jobs.extend(batch.items);
        match next {
            Some(p) => page = p,
            None => return Ok(jobs),
        }
    }
}

pub(super) async fn all_repos(store: &Store) -> Result<BTreeMap<RepoId, Repo>, WebError> {
    let mut repos = BTreeMap::new();
    let mut page = Page::first(Page::MAX_LIMIT);
    loop {
        let batch = store.list_repos(page).await?;
        let next = batch.next_page();
        repos.extend(batch.items.into_iter().map(|r| (r.id, r)));
        match next {
            Some(p) => page = p,
            None => return Ok(repos),
        }
    }
}

fn groups(map: BTreeMap<String, Kpi>) -> Vec<Group> {
    map.into_iter()
        .map(|(label, kpi)| Group { label, kpi })
        .collect()
}

/// Every KPI the page shows, from the ledger and the jobs in `store`.
pub async fn dashboard_kpis(store: &Store) -> Result<Dashboard, WebError> {
    let records = report::records(&store.ledger()).await?;
    let repos = all_repos(store).await?;
    let jobs: BTreeMap<String, (Job, Option<&Repo>)> = all_jobs(store)
        .await?
        .into_iter()
        .map(|j| {
            let repo = repos.get(&j.repo_id);
            (j.ledger_key.as_str().to_owned(), (j, repo))
        })
        .collect();
    let job_of = |r: &IssueRecord| jobs.get(r.issue_id.as_str());
    let languages = Kpi::grouped(&records, |r| {
        Some(job_of(r).map_or(OUTSIDE_JOBS.into(), |(_, repo)| {
            repo.map_or("?".into(), |repo| repo.language.to_ascii_lowercase())
        }))
    });
    let by_repo = Kpi::grouped(&records, |r| {
        Some(job_of(r).map_or(OUTSIDE_JOBS.into(), |(job, repo)| {
            repo.map_or(format!("repo {}", job.repo_id.0), |repo| repo.slug.clone())
        }))
    });
    let weeks = Kpi::grouped(&records, |r| {
        job_of(r).map(|(job, _)| job.created_at.format("%G-W%V").to_string())
    });
    let mut weeks = groups(weeks);
    weeks.drain(..weeks.len().saturating_sub(WEEKS_SHOWN));
    Ok(Dashboard {
        total: Kpi::from_records(&records),
        languages: groups(languages),
        repos: groups(by_repo),
        weeks,
    })
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardPage<'a> {
    nonce: &'a str,
    total: KpiRow,
    languages: Vec<KpiRow>,
    repos: Vec<KpiRow>,
    weeks: Vec<KpiRow>,
    total_min: u32,
    language_min: u32,
}

/// A threshold on the 0–200 scale of the bars.
fn bar(rate: f64) -> u32 {
    (rate * 200.0).round() as u32
}

pub async fn page(State(store): State<Store>, Extension(nonce): Extension<Nonce>) -> WebResult {
    let d = dashboard_kpis(&store).await?;
    let rows = |groups: &[Group], min: f64| -> Vec<KpiRow> {
        groups
            .iter()
            .map(|g| KpiRow::new(g.label.clone(), &g.kpi, min))
            .collect()
    };
    render(&DashboardPage {
        nonce: nonce.as_str(),
        total: KpiRow::new("Tổng", &d.total, ACCEPT_RATE_MIN),
        languages: rows(&d.languages, LANGUAGE_ACCEPT_RATE_MIN),
        repos: rows(&d.repos, ACCEPT_RATE_MIN),
        weeks: rows(&d.weeks, ACCEPT_RATE_MIN),
        total_min: bar(ACCEPT_RATE_MIN),
        language_min: bar(LANGUAGE_ACCEPT_RATE_MIN),
    })
}
