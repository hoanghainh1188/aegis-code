//! `/`: the KPIs of the ledger in total, per language, per repo and per
//! week, and each repo's budget this month (#31). The total is `aegis report`'s "All runs"; the groups use the same
//! formula ([`Kpi::grouped`]) on the same records, read from the saved
//! summary records with their jobs in one query ([`Store::kpi_records`],
//! #170). Someone who is not an admin sees only the runs of the repos they
//! have a role in.

use std::collections::BTreeMap;

use aegis_ledger::{ACCEPT_RATE_MIN, IssueRecord, Kpi, LANGUAGE_ACCEPT_RATE_MIN};
use aegis_store::{KpiJob, Page, Repo, RepoId, Store};
use askama::Template;
use axum::Extension;
use axum::extract::State;

use super::view::{BudgetRow, KpiRow, dollars};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

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

/// Every KPI the page shows, from the ledger's records and the jobs in `store`.
pub async fn dashboard_kpis(store: &Store) -> Result<Dashboard, WebError> {
    dashboard_kpis_within(store, None).await
}

/// [`dashboard_kpis`] of the jobs of the repos `within` only, when given;
/// runs outside jobs are then left out too.
pub async fn dashboard_kpis_within(
    store: &Store,
    within: Option<&[RepoId]>,
) -> Result<Dashboard, WebError> {
    let rows = store.kpi_records(within).await?;
    let records: Vec<IssueRecord> = rows.iter().map(|r| r.record.clone()).collect();
    let jobs: BTreeMap<&str, &KpiJob> = rows
        .iter()
        .filter_map(|r| Some((r.record.issue_id.as_str(), r.job.as_ref()?)))
        .collect();
    let job_of = |r: &IssueRecord| jobs.get(r.issue_id.as_str()).copied();
    let languages = Kpi::grouped(&records, |r| {
        Some(job_of(r).map_or(OUTSIDE_JOBS.into(), |job| job.language.to_ascii_lowercase()))
    });
    let by_repo = Kpi::grouped(&records, |r| {
        Some(job_of(r).map_or(OUTSIDE_JOBS.into(), |job| job.repo_slug.clone()))
    });
    let weeks = Kpi::grouped(&records, |r| {
        job_of(r).map(|job| job.created_at.format("%G-W%V").to_string())
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
    me: &'a Me,
    total: KpiRow,
    languages: Vec<KpiRow>,
    repos: Vec<KpiRow>,
    weeks: Vec<KpiRow>,
    budgets: Vec<BudgetRow>,
    time_zone: &'a str,
    /// The warning threshold, in % of the cap.
    warn_at: u32,
    reserve: String,
    total_min: u32,
    language_min: u32,
}

/// A threshold on the 0–200 scale of the bars.
fn bar(rate: f64) -> u32 {
    (rate * 200.0).round() as u32
}

pub async fn page(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
) -> WebResult {
    let within = access.visible();
    let d = dashboard_kpis_within(&store, within.as_deref()).await?;
    let rules = store.budget_rules();
    let budgets = store
        .repo_budgets(within.as_deref())
        .await?
        .iter()
        .map(|b| BudgetRow::new(b, rules))
        .collect();
    let rows = |groups: &[Group], min: f64| -> Vec<KpiRow> {
        groups
            .iter()
            .map(|g| KpiRow::new(g.label.clone(), &g.kpi, min))
            .collect()
    };
    render(&DashboardPage {
        nonce: nonce.as_str(),
        me: &me,
        total: KpiRow::new("Tổng", &d.total, ACCEPT_RATE_MIN),
        languages: rows(&d.languages, LANGUAGE_ACCEPT_RATE_MIN),
        repos: rows(&d.repos, ACCEPT_RATE_MIN),
        weeks: rows(&d.weeks, ACCEPT_RATE_MIN),
        budgets,
        time_zone: &rules.time_zone,
        warn_at: (rules.warn_ratio * 100.0).round() as u32,
        reserve: dollars(rules.job_reserve_usd),
        total_min: bar(ACCEPT_RATE_MIN),
        language_min: bar(LANGUAGE_ACCEPT_RATE_MIN),
    })
}
