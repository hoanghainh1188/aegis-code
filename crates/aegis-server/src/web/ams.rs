//! The AMS report page (#526, ADR 0011 §5): MTTR, SLA on time, triage or
//! PR, by tenant and SLA level, and each contract's spend, this month. A
//! person sees the tenants of the repos they have a role in; the platform's
//! admins see every one.

use aegis_store::{AmsReport, AmsRow, BudgetState, Store, TenantBudget};
use askama::Template;
use axum::Extension;
use axum::extract::State;

use super::{Me, Nonce, WebResult, render};
use crate::auth::Access;

pub(crate) struct Row {
    pub tenant: String,
    pub priority: String,
    pub done: i64,
    pub mttr: String,
    pub on_time: String,
    pub overdue_open: i64,
    pub triage: String,
    pub prs: String,
    pub total: bool,
}

fn pct(ratio: Option<f64>) -> String {
    ratio.map_or("—".into(), |r| format!("{:.0}%", r * 100.0))
}

/// `2h 05m`, `14m`, `40s`.
pub(crate) fn duration(seconds: Option<f64>) -> String {
    let Some(s) = seconds.filter(|s| s.is_finite() && *s >= 0.0) else {
        return "—".into();
    };
    let s = s.round() as u64;
    match (s / 3600, (s % 3600) / 60) {
        (0, 0) => format!("{s}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m:02}m"),
    }
}

impl Row {
    fn of(r: &AmsRow, total: bool) -> Self {
        Self {
            tenant: r.tenant.clone(),
            priority: r.priority.clone(),
            done: r.done,
            mttr: duration(r.mttr_s),
            on_time: match r.with_deadline {
                0 => "—".into(),
                n => format!("{} ({}/{n})", pct(r.on_time_ratio()), r.on_time),
            },
            overdue_open: r.overdue_open,
            triage: pct(r.triage_ratio()),
            prs: pct(r.pr_ratio()),
            total,
        }
    }
}

pub(crate) struct Contract {
    pub tenant: String,
    pub spent: String,
    pub cap: String,
    pub state: &'static str,
}

impl Contract {
    fn of(b: &TenantBudget, store: &Store) -> Self {
        let state = b.state(store.budget_rules());
        Self {
            tenant: b.slug.clone(),
            spent: format!("${:.2}", b.spent_usd),
            cap: b
                .monthly_usd
                .map_or("no cap".into(), |c| format!("${c:.2}")),
            state: match state {
                BudgetState::Spent => "spent",
                BudgetState::Warn => "warn",
                BudgetState::Ok | BudgetState::Unlimited => "",
            },
        }
    }
}

/// Each tenant's levels, then its total.
pub(crate) fn rows(report: &AmsReport) -> Vec<Row> {
    let mut tenants: Vec<&str> = report.rows.iter().map(|r| r.tenant.as_str()).collect();
    tenants.dedup();
    let mut out = Vec::new();
    for t in tenants {
        let levels: Vec<&AmsRow> = report.rows.iter().filter(|r| r.tenant == t).collect();
        out.extend(levels.iter().map(|r| Row::of(r, false)));
        if levels.len() > 1 {
            out.push(Row::of(&report.tenant_total(t), true));
        }
    }
    out
}

#[derive(Template)]
#[template(path = "ams.html")]
struct AmsPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    rows: Vec<Row>,
    contracts: Vec<Contract>,
}

pub async fn page(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
) -> WebResult {
    let visible = access.visible();
    let report = store.ams_report(visible.as_deref()).await?;
    render(&AmsPage {
        nonce: nonce.as_str(),
        me: &me,
        rows: rows(&report),
        contracts: report
            .contracts
            .iter()
            .map(|c| Contract::of(c, &store))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(duration(None), "—");
        assert_eq!(duration(Some(40.4)), "40s");
        assert_eq!(duration(Some(14.0 * 60.0)), "14m");
        assert_eq!(duration(Some(7500.0)), "2h 05m");
        assert_eq!(duration(Some(f64::NAN)), "—");
    }
}
