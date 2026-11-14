//! How Light's shadow triage (#497, ADR 0012) compares with what the jobs
//! did: per category, how many jobs opened a PR, how the reviewers judged
//! it, and how many stopped without one. A ticket read as `ops_or_config`
//! whose PR was accepted is a code bug the diagnostic lane (#451) would
//! have missed; this table is what decides whether to turn it on.

use std::fmt;

use crate::{IssueRecord, Outcome, ReviewVerdict, TriageCategory};

/// The jobs of one category (`None`: Light gave no triage, or the ticket's
/// source set its kind).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriageRow {
    pub category: Option<TriageCategory>,
    pub jobs: u32,
    /// Finished with a PR.
    pub prs: u32,
    /// Of those, accepted as is or with edits.
    pub accepted: u32,
    pub rejected: u32,
    /// Finished without a PR (budget, steps, Expert gave up).
    pub no_pr: u32,
}

/// One row per category, in [`TriageCategory::ALL`] order, then the jobs
/// with no triage. Every category has a row, with zeros when it has no job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriageTable {
    pub rows: Vec<TriageRow>,
}

impl TriageTable {
    pub fn from_records<'a>(records: impl IntoIterator<Item = &'a IssueRecord>) -> Self {
        let mut rows: Vec<TriageRow> = TriageCategory::ALL
            .into_iter()
            .map(Some)
            .chain([None])
            .map(|category| TriageRow {
                category,
                ..TriageRow::default()
            })
            .collect();
        for record in records {
            let category = record.triage.as_ref().map(|t| t.category);
            let Some(row) = rows.iter_mut().find(|r| r.category == category) else {
                continue;
            };
            row.jobs += 1;
            match record.outcome {
                Some(Outcome::PrOpened) => {
                    row.prs += 1;
                    match record.review.map(|r| r.verdict) {
                        Some(ReviewVerdict::Accepted | ReviewVerdict::AcceptedWithEdits) => {
                            row.accepted += 1;
                        }
                        Some(ReviewVerdict::Rejected) => row.rejected += 1,
                        None => {}
                    }
                }
                Some(_) => row.no_pr += 1,
                None => {}
            }
        }
        Self { rows }
    }

    /// Whether any job was triaged at all.
    pub fn is_empty(&self) -> bool {
        self.rows
            .iter()
            .all(|r| r.category.is_none() || r.jobs == 0)
    }
}

impl TriageRow {
    pub fn label(&self) -> &'static str {
        self.category.map_or("(none)", TriageCategory::as_str)
    }
}

impl fmt::Display for TriageTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "## Triage (recorded only, #497)\n\
             | triage | jobs | PRs | accepted | rejected | no PR |\n\
             | --- | --- | --- | --- | --- | --- |"
        )?;
        for r in &self.rows {
            writeln!(
                f,
                "| {} | {} | {} | {} | {} | {} |",
                r.label(),
                r.jobs,
                r.prs,
                r.accepted,
                r.rejected,
                r.no_pr
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueId, Review, Triage};

    fn job(
        id: &str,
        category: Option<TriageCategory>,
        outcome: Option<Outcome>,
        verdict: Option<ReviewVerdict>,
    ) -> IssueRecord {
        let mut r = IssueRecord::new(IssueId::parse(id).unwrap());
        r.triage = category.map(|category| Triage {
            category,
            confidence: 0.5,
            reason: String::new(),
        });
        r.outcome = outcome;
        r.review = verdict.map(|verdict| Review {
            verdict,
            lines_edited: 0,
            review_minutes: 1.0,
            broke_tests: false,
        });
        r
    }

    #[test]
    fn jobs_count_by_category_and_what_became_of_them() {
        use ReviewVerdict::*;
        use TriageCategory::*;
        let records = [
            job("a", Some(CodeBug), Some(Outcome::PrOpened), Some(Accepted)),
            job("b", Some(CodeBug), Some(Outcome::PrOpened), Some(Rejected)),
            job("c", Some(CodeBug), Some(Outcome::StoppedBudget), None),
            // A config ticket whose code fix was accepted: a miss of #451.
            job(
                "d",
                Some(OpsOrConfig),
                Some(Outcome::PrOpened),
                Some(AcceptedWithEdits),
            ),
            job("e", Some(NeedsInfo), None, None),
            job("f", None, Some(Outcome::PrOpened), None),
        ];
        let table = TriageTable::from_records(&records);
        let row = |c: Option<TriageCategory>| table.rows.iter().find(|r| r.category == c).unwrap();
        let code = row(Some(CodeBug));
        assert_eq!(
            (
                code.jobs,
                code.prs,
                code.accepted,
                code.rejected,
                code.no_pr
            ),
            (3, 2, 1, 1, 1)
        );
        let ops = row(Some(OpsOrConfig));
        assert_eq!((ops.jobs, ops.prs, ops.accepted), (1, 1, 1));
        // Still running: counted, nothing else.
        assert_eq!(
            row(Some(NeedsInfo)),
            &TriageRow {
                category: Some(NeedsInfo),
                jobs: 1,
                ..TriageRow::default()
            }
        );
        assert_eq!(row(Some(DataIssue)).jobs, 0);
        assert_eq!((row(None).jobs, row(None).prs), (1, 1));
        assert!(!table.is_empty());

        let text = table.to_string();
        assert!(
            text.starts_with("## Triage (recorded only, #497)\n"),
            "{text}"
        );
        assert!(text.contains("| code_bug | 3 | 2 | 1 | 1 | 1 |"), "{text}");
        assert!(
            text.contains("| ops_or_config | 1 | 1 | 1 | 0 | 0 |"),
            "{text}"
        );
        assert!(text.contains("| (none) | 1 | 1 | 0 | 0 | 0 |"), "{text}");
        // Category order, then the untriaged.
        let order: Vec<usize> = [
            "code_bug",
            "ops_or_config",
            "data_issue",
            "needs_info",
            "(none)",
        ]
        .iter()
        .map(|l| text.find(&format!("| {l} |")).unwrap())
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
    }

    #[test]
    fn a_ledger_without_triage_has_an_empty_table() {
        let records = [job("a", None, Some(Outcome::PrOpened), None)];
        assert!(TriageTable::from_records(&records).is_empty());
    }
}
