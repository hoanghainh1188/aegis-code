//! `/`: the KPIs of the ledger in total, per language, per repo and per
//! week, and each repo's budget this month (#31). The total is `aegis report`'s "All runs"; the groups use the same
//! formula ([`Kpi::grouped`]) on the same records, except that a review
//! round (#245) is not a run: its cost and calls count with its PR, and
//! the rounds an accepted PR took are shown. The records are read from the saved
//! summary records with their jobs in one query ([`Store::kpi_records`],
//! #170). Someone who is not an admin sees only the runs of the repos they
//! have a role in.

use std::collections::BTreeMap;

use aegis_ledger::{ACCEPT_RATE_MIN, IssueRecord, Kpi, LANGUAGE_ACCEPT_RATE_MIN, accepted};
use aegis_store::{KpiJob, KpiRecord, Page, Repo, RepoId, RepoRole, Store};
use askama::Template;
use axum::Extension;
use axum::extract::State;

use super::view::{BudgetRow, KpiRow, dollars};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

/// Weeks the dashboard shows, the latest last.
const WEEKS_SHOWN: usize = 12;
/// Runs not started by a job: `aegis run`, eval and CLI runs.
pub const OUTSIDE_JOBS: &str = "outside jobs (CLI, eval)";

/// The KPIs of one group of runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub label: String,
    pub kpi: Kpi,
    /// Review rounds (#245) a PR had before it was accepted, on average
    /// over the accepted ones.
    pub review_rounds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dashboard {
    pub total: Kpi,
    pub total_review_rounds: Option<f64>,
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

/// The records the KPIs count: a review round (#245) is not a run of its
/// own; its cost, calls and tokens are added to the record of the job
/// that opened its PR. Also the rounds each of those had.
fn fold_rounds(rows: &[KpiRecord]) -> (Vec<IssueRecord>, BTreeMap<String, u32>) {
    let mut records: BTreeMap<String, IssueRecord> = rows
        .iter()
        .filter(|r| r.job.as_ref().is_none_or(|j| j.review_of.is_none()))
        .map(|r| (r.record.issue_id.as_str().to_owned(), r.record.clone()))
        .collect();
    let mut rounds: BTreeMap<String, u32> = BTreeMap::new();
    for row in rows {
        let Some(job) = &row.job else { continue };
        let Some(root) = &job.review_of else { continue };
        // A PR outside the rows (another repo's view) is not shown.
        if let Some(record) = records.get_mut(root.as_str()) {
            *record = record.with_round(&row.record);
            let seen = rounds.entry(root.as_str().to_owned()).or_default();
            *seen = (*seen).max(job.review_round);
        }
    }
    (records.into_values().collect(), rounds)
}

/// [`Group::review_rounds`] of `records`.
fn mean_rounds<'a>(
    records: impl IntoIterator<Item = &'a IssueRecord>,
    rounds: &BTreeMap<String, u32>,
) -> Option<f64> {
    let (sum, n) = records
        .into_iter()
        .filter(|r| accepted(r))
        .map(|r| f64::from(rounds.get(r.issue_id.as_str()).copied().unwrap_or(0)))
        .fold((0.0, 0u32), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / f64::from(n))
}

/// The groups `label` makes of `records`, each with its KPIs and rounds.
fn groups(
    records: &[IssueRecord],
    rounds: &BTreeMap<String, u32>,
    label: impl Fn(&IssueRecord) -> Option<String>,
) -> Vec<Group> {
    let mut members: BTreeMap<String, Vec<&IssueRecord>> = BTreeMap::new();
    for r in records {
        if let Some(l) = label(r) {
            members.entry(l).or_default().push(r);
        }
    }
    Kpi::grouped(records, &label)
        .into_iter()
        .map(|(label, kpi)| {
            let in_group = members.get(&label).map(Vec::as_slice).unwrap_or_default();
            Group {
                review_rounds: mean_rounds(in_group.iter().copied(), rounds),
                label,
                kpi,
            }
        })
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
    let (records, rounds) = fold_rounds(&rows);
    let jobs: BTreeMap<&str, &KpiJob> = rows
        .iter()
        .filter_map(|r| Some((r.record.issue_id.as_str(), r.job.as_ref()?)))
        .collect();
    let job_of = |r: &IssueRecord| jobs.get(r.issue_id.as_str()).copied();
    let languages = groups(&records, &rounds, |r| {
        Some(job_of(r).map_or(OUTSIDE_JOBS.into(), |job| job.language.to_ascii_lowercase()))
    });
    let repos = groups(&records, &rounds, |r| {
        Some(job_of(r).map_or(OUTSIDE_JOBS.into(), |job| job.repo_slug.clone()))
    });
    let mut weeks = groups(&records, &rounds, |r| {
        job_of(r).map(|job| job.created_at.format("%G-W%V").to_string())
    });
    weeks.drain(..weeks.len().saturating_sub(WEEKS_SHOWN));
    Ok(Dashboard {
        total: Kpi::from_records(&records),
        total_review_rounds: mean_rounds(&records, &rounds),
        languages,
        repos,
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
        .map(|b| BudgetRow::new(b, rules).editable(access.can(b.repo_id, RepoRole::Maintainer)))
        .collect();
    let rows = |groups: &[Group], min: f64| -> Vec<KpiRow> {
        groups
            .iter()
            .map(|g| KpiRow::new(g.label.clone(), &g.kpi, min).with_rounds(g.review_rounds))
            .collect()
    };
    render(&DashboardPage {
        nonce: nonce.as_str(),
        me: &me,
        total: KpiRow::new("Total", &d.total, ACCEPT_RATE_MIN).with_rounds(d.total_review_rounds),
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

#[cfg(test)]
mod tests {
    use aegis_ledger::{IssueId, LedgerEvent, Outcome, Review, ReviewVerdict, Tier, TokenCounts};
    use aegis_store::{KpiJob, KpiRecord, RepoId};

    use super::*;

    fn record(key: &str, cost: f64, review: Option<ReviewVerdict>) -> IssueRecord {
        let mut events = vec![
            LedgerEvent::ModelCall {
                tier: Tier::Core,
                model: "core".into(),
                tokens: TokenCounts::default(),
                cost_usd: cost,
            },
            LedgerEvent::Finished {
                outcome: Outcome::PrOpened,
                wall_time_s: 1.0,
            },
        ];
        if let Some(verdict) = review {
            events.push(LedgerEvent::Reviewed(Review {
                verdict,
                lines_edited: 0,
                review_minutes: 5.0,
                broke_tests: false,
            }));
        }
        IssueRecord::from_events(IssueId::parse(key.to_owned()).unwrap(), &events)
    }

    fn row(record: IssueRecord, review_of: Option<&str>, round: u32) -> KpiRecord {
        KpiRecord {
            record,
            job: Some(KpiJob {
                repo_id: RepoId(1),
                repo_slug: "demo".into(),
                language: "go".into(),
                created_at: Default::default(),
                review_of: review_of.map(|k| IssueId::parse(k.to_owned()).unwrap()),
                review_round: round,
            }),
        }
    }

    #[test]
    fn review_rounds_count_with_their_pr_not_as_runs() {
        let rows = vec![
            row(record("A-1", 1.0, Some(ReviewVerdict::Accepted)), None, 0),
            row(record("A-1-V1", 0.5, None), Some("A-1"), 1),
            row(record("A-1-V2", 0.25, None), Some("A-1"), 2),
            row(record("B-1", 2.0, Some(ReviewVerdict::Accepted)), None, 0),
            row(record("C-1", 1.0, None), None, 0),
            // A round of a PR not shown here is not shown either.
            row(record("Z-1-V1", 9.0, None), Some("Z-1"), 1),
        ];
        let (records, rounds) = fold_rounds(&rows);
        let kpi = Kpi::from_records(&records);
        assert_eq!((kpi.runs, kpi.prs_opened, kpi.accepted), (3, 3, 2));
        let a = records
            .iter()
            .find(|r| r.issue_id.as_str() == "A-1")
            .unwrap();
        assert!((a.cost_usd - 1.75).abs() < 1e-12);
        assert_eq!(rounds.get("A-1"), Some(&2));
        // Accepted PRs: A after 2 rounds, B after none.
        assert_eq!(mean_rounds(&records, &rounds), Some(1.0));
        let by_repo = groups(&records, &rounds, |_| Some("demo".into()));
        assert_eq!(by_repo[0].review_rounds, Some(1.0));
        assert_eq!(by_repo[0].kpi.runs, 3);
    }
}
