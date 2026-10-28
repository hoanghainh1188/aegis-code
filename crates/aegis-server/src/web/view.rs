//! Values as the pages show them, formatted like `aegis report` prints
//! them so the two can be compared by eye.

use aegis_ledger::{
    CACHE_READ_MIN, COST_PER_ISSUE_MAX, IssueRecord, Kpi, REVIEW_MINUTES_MAX, Tier,
};
use aegis_store::{BudgetRules, BudgetState, RepoBudget};

/// `50% (1/2)`, or `—` with nothing to divide by.
pub fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        "—".into()
    } else {
        format!(
            "{:.0}% ({part}/{whole})",
            part as f64 * 100.0 / whole as f64
        )
    }
}

pub fn dollars(value: f64) -> String {
    format!("${value:.2}")
}

pub fn tiers(record: &IssueRecord) -> String {
    let names: Vec<&str> = record
        .tiers_used
        .iter()
        .map(|t| match t {
            Tier::Light => "Light",
            Tier::Core => "Core",
            Tier::Expert => "Expert",
        })
        .collect();
    if names.is_empty() {
        "—".into()
    } else {
        names.join(" → ")
    }
}

pub fn seconds(value: Option<f64>) -> String {
    match value {
        Some(s) if s >= 60.0 => format!("{:.1} phút", s / 60.0),
        Some(s) => format!("{s:.0} s"),
        None => "—".into(),
    }
}

/// A threshold check: the CSS class of its cell.
pub fn class(ok: Option<bool>) -> &'static str {
    match ok {
        Some(true) => "ok",
        Some(false) => "bad",
        None => "na",
    }
}

/// One row of a KPI table, and the bar of its acceptance rate.
#[derive(Debug, Clone, PartialEq)]
pub struct KpiRow {
    pub label: String,
    pub runs: usize,
    pub prs_opened: usize,
    pub awaiting_review: usize,
    pub accept: String,
    pub accept_class: &'static str,
    /// Bar length in the chart's 0–200 units.
    pub accept_bar: u32,
    pub broke_tests: usize,
    pub cost: String,
    pub cost_class: &'static str,
    pub review: String,
    pub review_class: &'static str,
    pub cache: String,
    pub cache_class: &'static str,
    pub expert: String,
    /// Review rounds per accepted PR (#245).
    pub rounds: String,
}

impl KpiRow {
    pub fn with_rounds(self, rounds: Option<f64>) -> Self {
        Self {
            rounds: rounds.map_or("—".into(), |r| format!("{r:.1}")),
            ..self
        }
    }

    /// `accept_min` is the acceptance threshold of the row: the total's,
    /// or a language's (ADR 0005).
    pub fn new(label: impl Into<String>, kpi: &Kpi, accept_min: f64) -> Self {
        let accept = kpi.accept_rate();
        Self {
            label: label.into(),
            runs: kpi.runs,
            prs_opened: kpi.prs_opened,
            awaiting_review: kpi.awaiting_review,
            accept: pct(kpi.accepted, kpi.runs),
            accept_class: class(accept.map(|a| a >= accept_min)),
            accept_bar: accept.map_or(0, |a| (a * 200.0).round() as u32),
            broke_tests: kpi.broke_tests,
            cost: kpi.mean_cost_usd.map_or("—".into(), dollars),
            cost_class: class(kpi.mean_cost_usd.map(|c| c <= COST_PER_ISSUE_MAX)),
            review: kpi
                .mean_review_minutes
                .map_or("—".into(), |m| format!("{m:.1} min")),
            review_class: class(kpi.mean_review_minutes.map(|m| m <= REVIEW_MINUTES_MAX)),
            cache: kpi
                .cache_read_ratio
                .map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
            cache_class: class(kpi.cache_read_ratio.map(|r| r >= CACHE_READ_MIN)),
            expert: pct(kpi.expert_runs, kpi.runs),
            rounds: "—".into(),
        }
    }
}

/// One row of the monthly budgets.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetRow {
    pub label: String,
    pub spent: String,
    pub cap: String,
    pub left: String,
    pub capped: bool,
    /// Share of the cap used, `82%`.
    pub used: String,
    /// Bar length in 0–100 units, capped at 100.
    pub bar: u32,
    pub state: &'static str,
    pub class: &'static str,
}

impl BudgetRow {
    pub fn new(budget: &RepoBudget, rules: &BudgetRules) -> Self {
        let share = budget.monthly_usd.map(|cap| {
            if cap > 0.0 {
                budget.spent_usd / cap
            } else {
                1.0
            }
        });
        let (state, class) = match budget.state(rules) {
            BudgetState::Unlimited => ("Không giới hạn", "na"),
            BudgetState::Ok => ("Ổn", "ok"),
            BudgetState::Warn => ("Gần chạm trần", "warn"),
            BudgetState::Spent => ("Hết: không nhận job mới", "bad"),
        };
        Self {
            label: budget.slug.clone(),
            spent: dollars(budget.spent_usd),
            cap: budget.monthly_usd.map_or("—".into(), dollars),
            left: budget.left_usd().map_or("—".into(), dollars),
            capped: budget.monthly_usd.is_some(),
            used: share.map_or("—".into(), |s| format!("{:.0}%", s * 100.0)),
            bar: share.map_or(0, |s| (s * 100.0).round().clamp(0.0, 100.0) as u32),
            state,
            class,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_store::RepoId;

    #[test]
    fn budgets_read_as_spent_of_cap() {
        let rules = BudgetRules::default();
        let budget = |monthly_usd, spent_usd| RepoBudget {
            repo_id: RepoId(1),
            slug: "typst".into(),
            monthly_usd,
            spent_usd,
        };
        let row = BudgetRow::new(&budget(Some(50.0), 41.0), &rules);
        assert_eq!((row.used.as_str(), row.bar, row.class), ("82%", 82, "warn"));
        assert_eq!(row.left, "$9.00");
        let row = BudgetRow::new(&budget(Some(50.0), 70.0), &rules);
        assert_eq!(
            (row.bar, row.class, row.left.as_str()),
            (100, "bad", "$0.00")
        );
        let row = BudgetRow::new(&budget(None, 12.0), &rules);
        assert_eq!(
            (row.cap.as_str(), row.capped, row.class),
            ("—", false, "na")
        );
    }

    #[test]
    fn values_read_like_the_report() {
        assert_eq!(pct(1, 2), "50% (1/2)");
        assert_eq!(pct(0, 0), "—");
        assert_eq!(dollars(2.0), "$2.00");
        assert_eq!(seconds(Some(90.0)), "1.5 phút");
        let row = KpiRow::new("rust", &Kpi::from_records(&[]), 0.35);
        assert_eq!((row.accept.as_str(), row.accept_class), ("—", "na"));
    }
}
