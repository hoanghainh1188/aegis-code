//! The pilot's report (#700): one period of one repo or of every repo the
//! caller sees, against the period of the same length before it and the
//! KPI thresholds of ADR 0005. The dashboard shows it, and the API gives
//! it as Markdown, CSV and JSON for `aegis report`: all from [`metrics`],
//! so the three never differ. The KPIs are [`Kpi::from_records`]', as
//! `aegis-local report` computes them.

use aegis_ledger::{
    ACCEPT_RATE_MIN, COST_PER_ISSUE_MAX, IssueRecord, Kpi, REVIEW_MINUTES_MAX, TriageCategory,
};
use aegis_store::{KpiJob, RepoId, Store, StoreError};
use serde::Serialize;
use time::{Date, Duration, OffsetDateTime};

/// The longest period a report covers.
pub const MAX_DAYS: i64 = 366;

/// What a report is asked for: from `since` to the day before `until`,
/// in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PilotQuery {
    pub repo: Option<RepoId>,
    pub since: Date,
    pub until: Date,
}

impl PilotQuery {
    /// From the texts a page, the API or the CLI is given, as
    /// `YYYY-MM-DD`: the first day, and the last day included (today when
    /// not given). `Err` says what is wrong.
    pub fn parse(
        repo: Option<i64>,
        since: Option<&str>,
        until: Option<&str>,
        today: Date,
    ) -> Result<Self, String> {
        let format = time::macros::format_description!("[year]-[month]-[day]");
        let date = |field: &str, text: &str| {
            Date::parse(text.trim(), &format)
                .map_err(|_| format!("{field}: `{text}` is not a date as YYYY-MM-DD"))
        };
        let since = date(
            "since",
            since.ok_or("since: a date as YYYY-MM-DD is needed")?,
        )?;
        let last = match until.filter(|u| !u.trim().is_empty()) {
            Some(u) => date("until", u)?,
            None => today,
        };
        let until = last + Duration::days(1);
        let days = (until - since).whole_days();
        if days < 1 {
            return Err("until: on or after since".into());
        }
        if days > MAX_DAYS {
            return Err(format!("at most {MAX_DAYS} days at once"));
        }
        Ok(Self {
            repo: repo.map(RepoId),
            since,
            until,
        })
    }

    /// The period of the same length right before this one.
    pub fn previous(&self) -> Self {
        let length = self.until - self.since;
        Self {
            since: self.since - length,
            until: self.since,
            ..*self
        }
    }
}

/// One period's figures.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Period {
    pub since: String,
    /// The last day, included.
    pub last: String,
    /// Tickets with a job created in the period.
    pub tickets: usize,
    /// Its jobs, review rounds counted with the PR they work on.
    pub jobs: usize,
    /// Jobs by what Light's triage read (#497), `not_triaged` for the rest.
    pub triage: Vec<(String, usize)>,
    /// The KPIs of the jobs that write, as `aegis-local report` has them.
    #[serde(skip)]
    pub kpi: Kpi,
    /// Shadow jobs (#699), apart.
    #[serde(skip)]
    pub shadow: Kpi,
    pub shadow_jobs: usize,
    /// Spent by every job of the period, shadow jobs included.
    pub cost_usd: f64,
    pub review_median_minutes: Option<f64>,
    /// Mean time from a job's creation to the end of its last run, over
    /// the jobs that wrote and ended (as the AMS report, #453).
    pub mttr_s: Option<f64>,
}

/// The repos' monthly caps and what they spent this month (#31).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetLine {
    /// `None` when a repo in the report has no cap.
    pub monthly_usd: Option<f64>,
    pub spent_usd: f64,
}

/// One line of the report: the same on the page, in Markdown and in CSV.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metric {
    pub name: &'static str,
    pub current: String,
    pub previous: String,
    /// The ADR 0005 threshold, or `tracked`.
    pub threshold: &'static str,
    /// Whether this period meets it; `None` when it has none or no data.
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PilotReport {
    /// The repo's slug, or `all repos`.
    pub scope: String,
    pub current: Period,
    pub previous: Period,
    pub budget: BudgetLine,
    pub metrics: Vec<Metric>,
}

fn utc_start(date: Date) -> OffsetDateTime {
    date.midnight().assume_utc()
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// The period `query` covers, from the folded records and their jobs.
fn period(rows: &[(&IssueRecord, &KpiJob)], query: &PilotQuery) -> Period {
    let (from, to) = (utc_start(query.since), utc_start(query.until));
    let rows: Vec<&(&IssueRecord, &KpiJob)> = rows
        .iter()
        .filter(|(_, job)| job.created_at >= from && job.created_at < to)
        .collect();
    let mut tickets: Vec<(RepoId, &str)> = rows
        .iter()
        .map(|(_, job)| (job.repo_id, job.issue_ref.as_str()))
        .collect();
    tickets.sort_unstable();
    tickets.dedup();
    let mut triage: Vec<(String, usize)> = TriageCategory::ALL
        .iter()
        .map(|c| {
            let n = rows
                .iter()
                .filter(|(r, _)| r.triage.as_ref().is_some_and(|t| t.category == *c))
                .count();
            (c.as_str().to_owned(), n)
        })
        .collect();
    triage.push((
        "not_triaged".into(),
        rows.iter().filter(|(r, _)| r.triage.is_none()).count(),
    ));
    let real: Vec<&IssueRecord> = rows
        .iter()
        .filter(|(_, job)| !job.shadow)
        .map(|(r, _)| *r)
        .collect();
    let shadow: Vec<&IssueRecord> = rows
        .iter()
        .filter(|(_, job)| job.shadow)
        .map(|(r, _)| *r)
        .collect();
    let repaired: Vec<f64> = rows
        .iter()
        .filter(|(r, job)| !job.shadow && r.outcome.is_some())
        .filter_map(|(_, job)| job.ended_at.map(|e| (e - job.created_at).as_seconds_f64()))
        .collect();
    Period {
        since: query.since.to_string(),
        last: (query.until - Duration::days(1)).to_string(),
        tickets: tickets.len(),
        jobs: rows.len(),
        triage,
        kpi: Kpi::from_records(real.iter().copied()),
        shadow: Kpi::from_records(shadow.iter().copied()),
        shadow_jobs: shadow.len(),
        // Not `sum()`: a float sum of nothing is -0.0, shown `$-0.00`.
        cost_usd: rows.iter().fold(0.0, |total, (r, _)| total + r.cost_usd),
        review_median_minutes: median(
            real.iter()
                .filter_map(|r| r.review.map(|v| v.review_minutes))
                .collect(),
        ),
        mttr_s: mean(&repaired),
    }
}

fn pct(part: usize, whole: usize) -> String {
    crate::web::view::pct(part, whole)
}

fn dollars(v: Option<f64>) -> String {
    v.map_or("—".into(), |v| format!("${v:.2}"))
}

fn minutes(v: Option<f64>) -> String {
    v.map_or("—".into(), |m| format!("{m:.1} min"))
}

fn triage_of(p: &Period, category: &str) -> String {
    let n = p
        .triage
        .iter()
        .find(|(c, _)| c == category)
        .map_or(0, |(_, n)| *n);
    pct(n, p.jobs)
}

/// The report's lines, in order, each for both periods.
fn metrics(current: &Period, previous: &Period) -> Vec<Metric> {
    type Of = fn(&Period) -> String;
    let line = |name: &'static str, of: Of, threshold: &'static str, ok: Option<bool>| Metric {
        name,
        current: of(current),
        previous: of(previous),
        threshold,
        ok,
    };
    let fixes = |p: &Period| p.kpi.runs - p.kpi.diagnosed;
    let tracked = "tracked";
    let mut lines = vec![
        line("Tickets", |p| p.tickets.to_string(), tracked, None),
        line("Jobs", |p| p.jobs.to_string(), tracked, None),
    ];
    for category in TriageCategory::ALL {
        let name: &'static str = match category {
            TriageCategory::CodeBug => "Triaged code_bug",
            TriageCategory::OpsOrConfig => "Triaged ops_or_config",
            TriageCategory::DataIssue => "Triaged data_issue",
            TriageCategory::NeedsInfo => "Triaged needs_info",
            TriageCategory::FeatureRequest => "Triaged feature_request",
        };
        let of: Of = match category {
            TriageCategory::CodeBug => |p| triage_of(p, "code_bug"),
            TriageCategory::OpsOrConfig => |p| triage_of(p, "ops_or_config"),
            TriageCategory::DataIssue => |p| triage_of(p, "data_issue"),
            TriageCategory::NeedsInfo => |p| triage_of(p, "needs_info"),
            TriageCategory::FeatureRequest => |p| triage_of(p, "feature_request"),
        };
        lines.push(line(name, of, tracked, None));
    }
    lines.extend([
        line(
            "Accepted PRs (≤ 10 lines edited)",
            |p| pct(p.kpi.accepted, p.kpi.runs - p.kpi.diagnosed),
            "≥ 40%",
            current.kpi.accept_rate().map(|a| a >= ACCEPT_RATE_MIN),
        ),
        line(
            "PRs opened",
            |p| p.kpi.prs_opened.to_string(),
            tracked,
            None,
        ),
        line(
            "PRs awaiting review",
            |p| p.kpi.awaiting_review.to_string(),
            tracked,
            None,
        ),
        line(
            "Stopped: budget",
            |p| pct(p.kpi.stopped_budget, p.kpi.runs - p.kpi.diagnosed),
            tracked,
            None,
        ),
        line(
            "Stopped: steps",
            |p| pct(p.kpi.stopped_steps, p.kpi.runs - p.kpi.diagnosed),
            tracked,
            None,
        ),
        line(
            "Stopped: Expert failed",
            |p| pct(p.kpi.stopped_expert_fail, p.kpi.runs - p.kpi.diagnosed),
            tracked,
            None,
        ),
        line(
            "Held: no verifiable sources",
            |p| pct(p.kpi.held_for_sources, p.kpi.runs),
            tracked,
            None,
        ),
        line(
            "Diagnosed, no PR",
            |p| pct(p.kpi.diagnosed, p.kpi.runs),
            tracked,
            None,
        ),
        line(
            "Cost per job",
            |p| dollars(p.kpi.mean_cost_usd),
            "≤ $4",
            current.kpi.mean_cost_usd.map(|c| c <= COST_PER_ISSUE_MAX),
        ),
        line(
            "Cost of the period",
            |p| dollars(Some(p.cost_usd)),
            tracked,
            None,
        ),
        line(
            "Review time per PR (median)",
            |p| minutes(p.review_median_minutes),
            "≤ 15 min",
            current
                .review_median_minutes
                .map(|m| m <= REVIEW_MINUTES_MAX),
        ),
        // As the AMS report shows it: `40s`, `14m`, `2h 05m`.
        line(
            "MTTR",
            |p| crate::web::ams::duration(p.mttr_s),
            tracked,
            None,
        ),
        line("Shadow jobs", |p| p.shadow_jobs.to_string(), tracked, None),
        line(
            "Shadow jobs a reviewer would merge",
            |p| pct(p.shadow.shadow_accepted, p.shadow.shadow_reviewed),
            tracked,
            None,
        ),
    ]);
    // No fix attempt: no rate to hold to its threshold.
    if fixes(current) == 0 {
        for m in &mut lines {
            if m.name.starts_with("Accepted") {
                m.ok = None;
            }
        }
    }
    lines
}

/// The report `query` asks for, of the repos `within` only when given.
/// `NotFound` when `query.repo` is not among them.
pub async fn pilot_report(
    store: &Store,
    within: Option<&[RepoId]>,
    query: &PilotQuery,
) -> Result<PilotReport, StoreError> {
    let scope: Vec<RepoId> = match query.repo {
        Some(repo) => {
            if within.is_some_and(|w| !w.contains(&repo)) {
                return Err(StoreError::NotFound(format!("repo {}", repo.0)));
            }
            vec![repo]
        }
        None => match within {
            Some(w) => w.to_vec(),
            None => Vec::new(),
        },
    };
    let only = (query.repo.is_some() || within.is_some()).then_some(scope.as_slice());
    let rows = store.kpi_records(only).await?;
    let (records, _) = crate::web::dashboard::fold_rounds(&rows);
    let jobs: std::collections::BTreeMap<&str, &KpiJob> = rows
        .iter()
        .filter_map(|r| Some((r.record.issue_id.as_str(), r.job.as_ref()?)))
        .collect();
    let paired: Vec<(&IssueRecord, &KpiJob)> = records
        .iter()
        .filter_map(|r| Some((r, *jobs.get(r.issue_id.as_str())?)))
        .collect();
    let current = period(&paired, query);
    let previous = period(&paired, &query.previous());
    let budgets = store.repo_budgets(only).await?;
    let budget = BudgetLine {
        monthly_usd: budgets.iter().map(|b| b.monthly_usd).sum::<Option<f64>>(),
        spent_usd: budgets.iter().map(|b| b.spent_usd).sum(),
    };
    let scope = match query.repo {
        Some(repo) => {
            store
                .repo(repo)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("repo {}", repo.0)))?
                .slug
        }
        None => "all repos".into(),
    };
    Ok(PilotReport {
        scope,
        metrics: metrics(&current, &previous),
        current,
        previous,
        budget,
    })
}

fn mark(ok: Option<bool>) -> &'static str {
    match ok {
        Some(true) => "✔",
        Some(false) => "✘",
        None => "—",
    }
}

impl PilotReport {
    fn budget_text(&self) -> String {
        match self.budget.monthly_usd {
            Some(cap) => format!(
                "${:.2} of ${cap:.2} spent this month",
                self.budget.spent_usd
            ),
            None => format!(
                "${:.2} spent this month, no monthly cap",
                self.budget.spent_usd
            ),
        }
    }

    /// For a pilot report: a title, the period, the table, the budget.
    pub fn markdown(&self) -> String {
        let (c, p) = (&self.current, &self.previous);
        let mut out = format!(
            "# Pilot report: {}\n\n{} to {} (UTC), against {} to {}.\n\n",
            self.scope, c.since, c.last, p.since, p.last
        );
        out.push_str("| Metric | This period | Previous period | Threshold | |\n");
        out.push_str("| --- | --- | --- | --- | --- |\n");
        for m in &self.metrics {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                m.name,
                m.current,
                m.previous,
                m.threshold,
                mark(m.ok)
            ));
        }
        out.push_str(&format!("\nMonthly budget: {}.\n", self.budget_text()));
        out.push_str(
            "\nShadow jobs (#699) are counted apart: the other lines are the jobs that write.\n",
        );
        out
    }

    /// One line per metric: `metric,current,previous,threshold,ok`.
    pub fn csv(&self) -> String {
        let field = |text: &str| {
            if text.contains([',', '"', '\n']) {
                format!("\"{}\"", text.replace('"', "\"\""))
            } else {
                text.to_owned()
            }
        };
        let mut out = String::from("metric,current,previous,threshold,ok\n");
        for m in &self.metrics {
            let ok = match m.ok {
                Some(true) => "yes",
                Some(false) => "no",
                None => "",
            };
            out.push_str(&format!(
                "{},{},{},{},{ok}\n",
                field(m.name),
                field(&m.current),
                field(&m.previous),
                field(m.threshold)
            ));
        }
        out.push_str(&format!(
            "Monthly budget,{},,,\n",
            field(&self.budget_text())
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn a_query_needs_a_start_and_a_period_of_a_day_or_more() {
        let today = date!(2026 - 11 - 26);
        let q = PilotQuery::parse(Some(3), Some("2026-11-19"), None, today).unwrap();
        assert_eq!(
            (q.since, q.until),
            (date!(2026 - 11 - 19), date!(2026 - 11 - 27))
        );
        let p = q.previous();
        assert_eq!(
            (p.since, p.until),
            (date!(2026 - 11 - 11), date!(2026 - 11 - 19))
        );
        assert!(PilotQuery::parse(None, None, None, today).is_err());
        assert!(PilotQuery::parse(None, Some("19/11"), None, today).is_err());
        let one = PilotQuery::parse(None, Some("2026-11-19"), Some("2026-11-19"), today).unwrap();
        assert_eq!(one.until, date!(2026 - 11 - 20));
        assert!(PilotQuery::parse(None, Some("2026-11-19"), Some("2026-11-18"), today).is_err());
        assert!(PilotQuery::parse(None, Some("2024-01-01"), None, today).is_err());
    }

    #[test]
    fn the_median_of_an_even_count_is_the_middle_pair_s_mean() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn a_period_without_jobs_costs_nothing_and_has_no_mttr() {
        let q = PilotQuery::parse(None, Some("2026-11-19"), None, date!(2026 - 11 - 26)).unwrap();
        let empty = period(&[], &q);
        assert_eq!(empty.cost_usd.to_bits(), 0.0f64.to_bits());
        let lines = metrics(&empty, &empty);
        let of = |name: &str| {
            lines
                .iter()
                .find(|m| m.name == name)
                .unwrap()
                .current
                .clone()
        };
        assert_eq!(of("Cost of the period"), "$0.00");
        assert_eq!(of("MTTR"), "—");
    }
}
