//! The PoC's KPIs (Proposal §8) from issue records. Only finished runs
//! count; the denominator of the acceptance rate is every finished run,
//! failures included, as the Proposal asks.

use std::fmt;

use crate::{IssueRecord, Outcome, ReviewVerdict, Tier};

/// PoC thresholds of Proposal §8.
pub const ACCEPT_RATE_MIN: f64 = 0.40;
/// Each language's own acceptance rate (ADR 0005).
pub const LANGUAGE_ACCEPT_RATE_MIN: f64 = 0.35;
pub const COST_PER_ISSUE_MAX: f64 = 4.0;
pub const REVIEW_MINUTES_MAX: f64 = 15.0;
pub const CACHE_READ_MIN: f64 = 0.70;
/// "Accepted" allows the reviewer this many edited lines.
pub const ACCEPT_MAX_EDITED_LINES: u32 = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct Kpi {
    pub runs: usize,
    pub prs_opened: usize,
    pub reviewed: usize,
    pub accepted: usize,
    pub broke_tests: usize,
    pub mean_cost_usd: Option<f64>,
    pub mean_review_minutes: Option<f64>,
    pub cache_read_ratio: Option<f64>,
    pub expert_runs: usize,
    pub hidden_ran: usize,
    pub hidden_passed: usize,
    /// Runs without a review yet among those that opened a PR.
    pub awaiting_review: usize,
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0u32), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / f64::from(n))
}

fn accepted(record: &IssueRecord) -> bool {
    record.review.is_some_and(|r| match r.verdict {
        ReviewVerdict::Accepted => true,
        ReviewVerdict::AcceptedWithEdits => r.lines_edited <= ACCEPT_MAX_EDITED_LINES,
        ReviewVerdict::Rejected => false,
    })
}

impl Kpi {
    pub fn from_records<'a>(records: impl IntoIterator<Item = &'a IssueRecord>) -> Self {
        let finished: Vec<&IssueRecord> = records
            .into_iter()
            .filter(|r| r.outcome.is_some())
            .collect();
        let prs: Vec<&&IssueRecord> = finished
            .iter()
            .filter(|r| r.outcome == Some(Outcome::PrOpened))
            .collect();
        let (read, total) = finished.iter().flat_map(|r| r.tokens.values()).fold(
            (0u64, 0u64),
            |(read, total), t| {
                let input = t.input + t.cache_read + t.cache_write_5m + t.cache_write_1h;
                (read + t.cache_read, total + input)
            },
        );
        Self {
            runs: finished.len(),
            prs_opened: prs.len(),
            reviewed: finished.iter().filter(|r| r.review.is_some()).count(),
            accepted: finished.iter().filter(|r| accepted(r)).count(),
            broke_tests: finished
                .iter()
                .filter(|r| r.review.is_some_and(|v| v.broke_tests))
                .count(),
            mean_cost_usd: mean(finished.iter().map(|r| r.cost_usd)),
            mean_review_minutes: mean(
                finished
                    .iter()
                    .filter_map(|r| r.review.map(|v| v.review_minutes)),
            ),
            cache_read_ratio: (total > 0).then(|| read as f64 / total as f64),
            expert_runs: finished
                .iter()
                .filter(|r| r.tiers_used.contains(&Tier::Expert))
                .count(),
            hidden_ran: finished
                .iter()
                .filter(|r| r.hidden_tests_passed.is_some())
                .count(),
            hidden_passed: finished
                .iter()
                .filter(|r| r.hidden_tests_passed == Some(true))
                .count(),
            awaiting_review: prs.iter().filter(|r| r.review.is_none()).count(),
        }
    }

    pub fn accept_rate(&self) -> Option<f64> {
        (self.runs > 0).then(|| self.accepted as f64 / self.runs as f64)
    }
}

fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        "—".into()
    } else {
        format!(
            "{:.0}% ({part}/{whole})",
            part as f64 * 100.0 / whole as f64
        )
    }
}

fn verdict(ok: Option<bool>) -> &'static str {
    match ok {
        Some(true) => "✔",
        Some(false) => "✘",
        None => "—",
    }
}

impl fmt::Display for Kpi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let accept = self.accept_rate();
        let rows = [
            (
                "Accepted PRs (≤ 10 lines edited)",
                pct(self.accepted, self.runs),
                "≥ 40%",
                accept.map(|a| a >= ACCEPT_RATE_MIN),
            ),
            (
                "PRs breaking old tests (reached a person)",
                self.broke_tests.to_string(),
                "0",
                (self.reviewed > 0).then_some(self.broke_tests == 0),
            ),
            (
                "Cost per issue, failures included",
                self.mean_cost_usd
                    .map_or("—".into(), |c| format!("${c:.2}")),
                "≤ $4",
                self.mean_cost_usd.map(|c| c <= COST_PER_ISSUE_MAX),
            ),
            (
                "Review time per PR",
                self.mean_review_minutes
                    .map_or("—".into(), |m| format!("{m:.1} min")),
                "≤ 15 min",
                self.mean_review_minutes.map(|m| m <= REVIEW_MINUTES_MAX),
            ),
            (
                "Cache read ratio",
                self.cache_read_ratio
                    .map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
                "≥ 70%",
                self.cache_read_ratio.map(|r| r >= CACHE_READ_MIN),
            ),
            (
                "Runs that went to Expert",
                pct(self.expert_runs, self.runs),
                "tracked",
                None,
            ),
            (
                "Hidden tests pass (eval runs)",
                pct(self.hidden_passed, self.hidden_ran),
                "tracked",
                None,
            ),
        ];
        writeln!(
            f,
            "{} runs, {} PRs opened, {} reviewed, {} awaiting review",
            self.runs, self.prs_opened, self.reviewed, self.awaiting_review
        )?;
        writeln!(f, "| KPI | Value | PoC threshold | |")?;
        writeln!(f, "| --- | --- | --- | --- |")?;
        for (name, value, threshold, ok) in rows {
            writeln!(f, "| {name} | {value} | {threshold} | {} |", verdict(ok))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{IssueId, Review, TokenCounts};

    fn record(
        id: &str,
        outcome: Outcome,
        cost: f64,
        review: Option<Review>,
        expert: bool,
    ) -> IssueRecord {
        let mut r = IssueRecord::new(IssueId::parse(id).unwrap());
        r.outcome = Some(outcome);
        r.cost_usd = cost;
        r.review = review;
        r.tiers_used = if expert {
            vec![Tier::Light, Tier::Core, Tier::Expert]
        } else {
            vec![Tier::Light, Tier::Core]
        };
        r.tokens = BTreeMap::from([(
            "core".into(),
            TokenCounts {
                input: 100,
                cache_read: 300,
                ..TokenCounts::default()
            },
        )]);
        r
    }

    fn review(verdict: ReviewVerdict, lines: u32, minutes: f64) -> Option<Review> {
        Some(Review {
            verdict,
            lines_edited: lines,
            review_minutes: minutes,
            broke_tests: false,
        })
    }

    #[test]
    fn kpis_follow_the_proposal() {
        let mut running = IssueRecord::new(IssueId::parse("running").unwrap());
        running.cost_usd = 9.0;
        let records = [
            record(
                "a",
                Outcome::PrOpened,
                1.0,
                review(ReviewVerdict::Accepted, 0, 5.0),
                false,
            ),
            record(
                "b",
                Outcome::PrOpened,
                2.0,
                review(ReviewVerdict::AcceptedWithEdits, 8, 10.0),
                false,
            ),
            record(
                "c",
                Outcome::PrOpened,
                3.0,
                review(ReviewVerdict::AcceptedWithEdits, 11, 15.0),
                true,
            ),
            record("d", Outcome::PrOpened, 2.0, None, false),
            record("e", Outcome::StoppedBudget, 4.0, None, true),
            running,
        ];
        let kpi = Kpi::from_records(&records);
        assert_eq!(
            (kpi.runs, kpi.prs_opened, kpi.reviewed, kpi.accepted),
            (5, 4, 3, 2)
        );
        assert_eq!(kpi.accept_rate(), Some(0.4));
        assert_eq!(kpi.mean_cost_usd, Some(2.4));
        assert_eq!(kpi.mean_review_minutes, Some(10.0));
        assert_eq!(kpi.cache_read_ratio, Some(0.75));
        assert_eq!((kpi.expert_runs, kpi.awaiting_review), (2, 1));
        let text = kpi.to_string();
        assert!(text.starts_with("5 runs, 4 PRs opened, 3 reviewed, 1 awaiting review"));
        assert!(
            text.contains("| Accepted PRs (≤ 10 lines edited) | 40% (2/5) | ≥ 40% | ✔ |"),
            "{text}"
        );
        assert!(text.contains("| Cost per issue, failures included | $2.40 | ≤ $4 | ✔ |"));
        assert!(text.contains("| Hidden tests pass (eval runs) | — | tracked | — |"));
    }

    #[test]
    fn an_empty_ledger_has_no_values() {
        let kpi = Kpi::from_records(&[]);
        assert_eq!(kpi.accept_rate(), None);
        assert!(
            kpi.to_string()
                .contains("| Cost per issue, failures included | — | ≤ $4 | — |")
        );
    }
}
