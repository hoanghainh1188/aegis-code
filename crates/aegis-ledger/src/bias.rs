//! Multi-dimensional disparity & bias summary (WORTH – R, #511, ADR 0016).
//!
//! Computes KPI slices and shadow-triage miss rates along four operational
//! dimensions (programming language, ticket natural language, ticket source,
//! and repository) using the exact [`Kpi`] and [`TriageTable`] formulas, and
//! flags slices whose metrics breach investigation thresholds once they have
//! enough finished attempts (`MIN_BIAS_SAMPLE`).

use std::collections::BTreeMap;
use std::fmt;

use crate::{IssueRecord, Kpi, LANGUAGE_ACCEPT_RATE_MIN, TicketMeta, TriageCategory, TriageTable};

/// Minimum finished fix attempts (or reviewed non-code tickets for triage
/// miss) before a slice triggers a disparity flag, avoiding noise at small N.
pub const MIN_BIAS_SAMPLE: usize = 5;
/// Flag when acceptance drops at least this much below the best sibling group.
pub const ACCEPT_GAP_ALERT: f64 = 0.25;
/// Flag when fix stop rate exceeds 30%.
pub const STOP_RATE_ALERT: f64 = 0.30;
/// Flag when stop rate exceeds the lowest sibling group by at least 20 pp.
pub const STOP_GAP_ALERT: f64 = 0.20;
/// Flag when mean cost is at least 2x the peer group median...
pub const COST_RATIO_ALERT: f64 = 2.0;
/// ...provided mean cost also exceeds $0.50 per issue.
pub const COST_FLOOR_ALERT: f64 = 0.50;
/// Flag when accepted-PR rate among non-code triaged tickets exceeds 10%.
pub const TRIAGE_MISS_RATE_ALERT: f64 = 0.10;

const EPS: f64 = 1e-9;

/// Metrics and disparity flags for one value along a dimension.
#[derive(Debug, Clone, PartialEq)]
pub struct BiasRow {
    pub label: String,
    pub kpi: Kpi,
    pub non_code_triaged: usize,
    pub non_code_reviewed: usize,
    pub non_code_accepted: usize,
    pub alerts: Vec<&'static str>,
}

impl BiasRow {
    /// Rate of non-`code_bug` triaged tickets whose PR was reviewed and accepted
    /// (diagnostic lane misses).
    pub fn triage_miss_rate(&self) -> Option<f64> {
        (self.non_code_reviewed > 0)
            .then(|| self.non_code_accepted as f64 / self.non_code_reviewed as f64)
    }

    pub fn fix_attempts(&self) -> usize {
        self.kpi.runs.saturating_sub(self.kpi.diagnosed)
    }
}

/// Breakdown along one operational dimension.
#[derive(Debug, Clone, PartialEq)]
pub struct BiasSection {
    pub heading: &'static str,
    pub rows: Vec<BiasRow>,
}

impl BiasSection {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Complete bias summary across all dimensions present in a set of records.
#[derive(Debug, Clone, PartialEq)]
pub struct BiasReport {
    pub sections: Vec<BiasSection>,
}

impl BiasReport {
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Build bias slices for programming language, ticket language, ticket
    /// source, and repository from `records`, resolving per-record metadata via
    /// `meta_of`. Empty dimensions are omitted.
    pub fn from_records(
        records: &[IssueRecord],
        meta_of: impl Fn(&IssueRecord) -> TicketMeta,
    ) -> Self {
        let metas: Vec<TicketMeta> = records.iter().map(&meta_of).collect();
        let build = |heading, key: &dyn Fn(&TicketMeta) -> Option<String>| -> Option<BiasSection> {
            let mut groups: BTreeMap<String, Vec<&IssueRecord>> = BTreeMap::new();
            for (record, meta) in records.iter().zip(&metas) {
                if record.outcome.is_none() {
                    continue;
                }
                if let Some(k) = key(meta) {
                    groups.entry(k).or_default().push(record);
                }
            }
            if groups.is_empty() {
                return None;
            }
            let mut rows: Vec<BiasRow> = groups
                .into_iter()
                .map(|(label, slice)| summarize_row(label, &slice))
                .collect();
            evaluate_alerts(&mut rows);
            Some(BiasSection { heading, rows })
        };

        let sections = [
            build("By programming language", &|m| {
                m.programming_language
                    .as_deref()
                    .map(str::to_ascii_lowercase)
            }),
            build("By ticket language", &|m| {
                m.ticket_language.map(|l| l.as_str().to_owned())
            }),
            build("By ticket source", &|m| {
                m.ticket_source.as_deref().map(str::to_ascii_lowercase)
            }),
            build("By repo", &|m| m.repo.clone()),
        ]
        .into_iter()
        .flatten()
        .collect();

        Self { sections }
    }
}

fn summarize_row(label: String, slice: &[&IssueRecord]) -> BiasRow {
    let kpi = Kpi::from_records(slice.iter().copied());
    let triage = TriageTable::from_records(slice.iter().copied());
    let (mut non_code_triaged, mut non_code_reviewed, mut non_code_accepted) = (0, 0, 0);
    for row in &triage.rows {
        let Some(cat) = row.category else {
            continue;
        };
        if cat != TriageCategory::CodeBug {
            non_code_triaged += row.jobs as usize;
            non_code_reviewed += (row.accepted + row.rejected) as usize;
            non_code_accepted += row.accepted as usize;
        }
    }
    BiasRow {
        label,
        kpi,
        non_code_triaged,
        non_code_reviewed,
        non_code_accepted,
        alerts: Vec::new(),
    }
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Some(sorted[mid])
    } else {
        Some((sorted[mid - 1] + sorted[mid]) / 2.0)
    }
}

fn evaluate_alerts(rows: &mut [BiasRow]) {
    let eligible_costs: Vec<(usize, f64)> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.fix_attempts() >= MIN_BIAS_SAMPLE)
        .filter_map(|(idx, r)| r.kpi.mean_cost_usd.map(|c| (idx, c)))
        .collect();
    let best_accept = {
        let rates: Vec<f64> = rows
            .iter()
            .filter(|r| r.fix_attempts() >= MIN_BIAS_SAMPLE)
            .filter_map(|r| r.kpi.accept_rate())
            .collect();
        (rates.len() >= 2)
            .then(|| rates.into_iter().reduce(f64::max))
            .flatten()
    };
    let lowest_stop = {
        let rates: Vec<f64> = rows
            .iter()
            .filter(|r| r.fix_attempts() >= MIN_BIAS_SAMPLE)
            .filter_map(|r| r.kpi.stop_rate())
            .collect();
        (rates.len() >= 2)
            .then(|| rates.into_iter().reduce(f64::min))
            .flatten()
    };
    for (idx, row) in rows.iter_mut().enumerate() {
        if row.fix_attempts() >= MIN_BIAS_SAMPLE {
            if let Some(accept) = row.kpi.accept_rate() {
                let below_floor = accept + EPS < LANGUAGE_ACCEPT_RATE_MIN;
                let wide_gap =
                    best_accept.is_some_and(|best| best - accept + EPS >= ACCEPT_GAP_ALERT);
                if below_floor || wide_gap {
                    row.alerts.push("low-accept");
                }
            }
            if let Some(stop) = row.kpi.stop_rate() {
                let above_cap = stop > STOP_RATE_ALERT + EPS;
                let wide_gap = lowest_stop.is_some_and(|low| stop - low + EPS >= STOP_GAP_ALERT);
                if above_cap || wide_gap {
                    row.alerts.push("high-stop");
                }
            }
            // Compare against sibling eligible groups (excluding self) so binary
            // comparisons (e.g. two languages) can still trigger high-cost.
            let peer_costs: Vec<f64> = eligible_costs
                .iter()
                .filter(|(peer_idx, _)| *peer_idx != idx)
                .map(|(_, c)| *c)
                .collect();
            if let (Some(cost), Some(med)) = (row.kpi.mean_cost_usd, median(&peer_costs))
                && med > 0.0
                && cost > COST_FLOOR_ALERT + EPS
                && cost + EPS >= COST_RATIO_ALERT * med
            {
                row.alerts.push("high-cost");
            }
        }
        if row.non_code_reviewed >= MIN_BIAS_SAMPLE
            && let Some(miss) = row.triage_miss_rate()
            && miss > TRIAGE_MISS_RATE_ALERT + EPS
        {
            row.alerts.push("triage-miss");
        }
    }
}

fn fmt_rate(part: usize, whole: usize) -> String {
    if whole == 0 {
        "—".into()
    } else {
        format!(
            "{:.0}% ({part}/{whole})",
            part as f64 * 100.0 / whole as f64
        )
    }
}

impl fmt::Display for BiasReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, section) in self.sections.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            writeln!(
                f,
                "## Bias check: {} (#511)\n\
                 | group | runs | accepted | stopped | expert | mean cost | triage miss | flags |\n\
                 | --- | --- | --- | --- | --- | --- | --- | --- |",
                section.heading
            )?;
            for row in &section.rows {
                let fixes = row.fix_attempts();
                let accepted = fmt_rate(row.kpi.accepted, fixes);
                let stopped = fmt_rate(row.kpi.stopped(), fixes);
                let expert = fmt_rate(row.kpi.expert_runs, row.kpi.runs);
                let cost = row
                    .kpi
                    .mean_cost_usd
                    .map_or("—".into(), |c| format!("${c:.2}"));
                let triage_miss = fmt_rate(row.non_code_accepted, row.non_code_reviewed);
                let flags = if row.alerts.is_empty() {
                    "—".into()
                } else {
                    row.alerts.join(", ")
                };
                writeln!(
                    f,
                    "| {} | {} | {accepted} | {stopped} | {expert} | {cost} | {triage_miss} | {flags} |",
                    row.label, row.kpi.runs
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueId, Outcome, Review, ReviewVerdict, TicketLanguage, Tier, Triage};

    fn make_record(
        id: &str,
        outcome: Outcome,
        cost_usd: f64,
        review: Option<ReviewVerdict>,
        meta: TicketMeta,
        triage: Option<TriageCategory>,
    ) -> IssueRecord {
        let mut r = IssueRecord::new(IssueId::parse(id).unwrap());
        r.outcome = Some(outcome);
        r.cost_usd = cost_usd;
        r.tiers_used = vec![Tier::Light, Tier::Core];
        r.ticket_meta = meta;
        r.review = review.map(|verdict| Review {
            verdict,
            lines_edited: 0,
            review_minutes: 5.0,
            broke_tests: false,
        });
        r.triage = triage.map(|category| Triage {
            category,
            confidence: 0.9,
            reason: "test".into(),
        });
        r
    }

    #[test]
    fn disparity_flags_trigger_only_at_min_sample_and_breach_thresholds() {
        let mut records = Vec::new();
        // Group "en": 5 accepted runs @ $0.40
        for i in 0..5 {
            records.push(make_record(
                &format!("en-{i}"),
                Outcome::PrOpened,
                0.40,
                Some(ReviewVerdict::Accepted),
                TicketMeta {
                    programming_language: Some("rust".into()),
                    ticket_source: Some("github".into()),
                    ticket_language: Some(TicketLanguage::En),
                    repo: Some("org/a".into()),
                },
                Some(TriageCategory::CodeBug),
            ));
        }
        // Group "ja": 5 runs, 1 accepted, 4 stopped_steps @ $1.60 -> low-accept, high-stop, high-cost
        for i in 0..5 {
            let (outcome, verdict) = if i == 0 {
                (Outcome::PrOpened, Some(ReviewVerdict::Accepted))
            } else {
                (Outcome::StoppedSteps, None)
            };
            records.push(make_record(
                &format!("ja-{i}"),
                outcome,
                1.60,
                verdict,
                TicketMeta {
                    programming_language: Some("rust".into()),
                    ticket_source: Some("backlog".into()),
                    ticket_language: Some(TicketLanguage::Ja),
                    repo: Some("org/a".into()),
                },
                // Shadow triage missed 1/5 -> wait, add a separate triage-miss slice below.
                Some(TriageCategory::CodeBug),
            ));
        }
        // Group "vi": small sample (N = 2), 0% accepted, 100% stopped -> must NOT trigger flags yet.
        for i in 0..2 {
            records.push(make_record(
                &format!("vi-{i}"),
                Outcome::StoppedBudget,
                3.50,
                None,
                TicketMeta {
                    programming_language: Some("python".into()),
                    ticket_source: Some("jira".into()),
                    ticket_language: Some(TicketLanguage::Vi),
                    repo: Some("org/b".into()),
                },
                Some(TriageCategory::OpsOrConfig),
            ));
        }
        // Group "jsm": 5 non_code_bug triaged runs where 2/5 PRs were accepted -> triage-miss (40% > 10%).
        for i in 0..5 {
            let verdict = if i < 2 {
                ReviewVerdict::Accepted
            } else {
                ReviewVerdict::Rejected
            };
            records.push(make_record(
                &format!("jsm-{i}"),
                Outcome::PrOpened,
                0.35,
                Some(verdict),
                TicketMeta {
                    programming_language: Some("python".into()),
                    ticket_source: Some("jsm".into()),
                    ticket_language: Some(TicketLanguage::Other),
                    repo: Some("org/b".into()),
                },
                Some(TriageCategory::OpsOrConfig),
            ));
        }

        let report = BiasReport::from_records(&records, |r| r.ticket_meta.clone());
        let lang_section = report
            .sections
            .iter()
            .find(|s| s.heading == "By ticket language")
            .unwrap();
        fn row<'a>(sec: &'a BiasSection, name: &str) -> &'a BiasRow {
            sec.rows.iter().find(|r| r.label == name).unwrap()
        }

        let en = row(lang_section, "en");
        assert!(en.alerts.is_empty(), "{:?}", en.alerts);

        let ja = row(lang_section, "ja");
        assert_eq!(
            ja.alerts,
            vec!["low-accept", "high-stop", "high-cost"],
            "{:?}",
            ja.alerts
        );

        let vi = row(lang_section, "vi");
        assert!(vi.alerts.is_empty(), "small sample N=2 should not alert");

        let source_section = report
            .sections
            .iter()
            .find(|s| s.heading == "By ticket source")
            .unwrap();
        let jsm = row(source_section, "jsm");
        assert!(jsm.alerts.contains(&"triage-miss"), "{:?}", jsm.alerts);

        let rendered = report.to_string();
        assert!(rendered.contains("## Bias check: By ticket language (#511)"));
        assert!(rendered.contains("| ja | 5 | 20% (1/5) | 80% (4/5) |"));
    }

    #[test]
    fn empty_metadata_yields_empty_report() {
        let records = [make_record(
            "plain-1",
            Outcome::PrOpened,
            0.2,
            Some(ReviewVerdict::Accepted),
            TicketMeta::default(),
            None,
        )];
        let report = BiasReport::from_records(&records, |r| r.ticket_meta.clone());
        assert!(report.is_empty());
        assert_eq!(report.to_string(), "");
    }
}
