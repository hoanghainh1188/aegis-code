//! The KPI report of a ledger (#99, #28): every run, or the latest run of
//! each issue of an eval set, in total and per language (ADR 0004, 0005).
//! `aegis report` prints it; the dashboard groups the same records by repo,
//! language and week with [`Kpi::grouped`], so both use one formula.

use std::collections::BTreeMap;
use std::fmt;

use crate::{IssueRecord, Kpi, LedgerError, LedgerStore};

/// Every run's record, rebuilt from its events, in the store's key order.
pub async fn records<S: LedgerStore>(store: &S) -> Result<Vec<IssueRecord>, LedgerError> {
    let mut out = Vec::new();
    for issue in store.issues().await? {
        let events = store.events(&issue).await?;
        out.push(IssueRecord::from_events(issue, &events));
    }
    Ok(out)
}

/// One issue of an eval set: its id (runs are `<id>-r<n>`) and language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetIssue<L> {
    pub id: String,
    pub language: L,
}

/// The eval id and run number of `record` when it is a run of one of
/// `ids` (run ids are `<eval id>-r<n>`, `ids` in lower case).
fn run_of<'a>(record: &IssueRecord, ids: &'a [String]) -> Option<(&'a str, u32)> {
    let id = record.issue_id.as_str().to_ascii_lowercase();
    ids.iter().find_map(|i| {
        let n = id.strip_prefix(i.as_str())?.strip_prefix("-r")?;
        if !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some((i.as_str(), n.parse().ok()?))
    })
}

/// The latest run of each of `ids` among `records`: a rerun replaces the
/// run before it, whatever the ledger's order.
fn latest_runs<'a>(records: &'a [IssueRecord], ids: &[String]) -> Vec<&'a IssueRecord> {
    let mut latest: BTreeMap<&str, (u32, &IssueRecord)> = BTreeMap::new();
    for record in records {
        if let Some((id, n)) = run_of(record, ids)
            && latest.get(id).is_none_or(|(seen, _)| n > *seen)
        {
            latest.insert(id, (n, record));
        }
    }
    latest.into_values().map(|(_, record)| record).collect()
}

impl Kpi {
    /// The KPIs of `records` per key, in key order; a record whose key is
    /// `None` counts in no group.
    pub fn grouped<'a, K: Ord>(
        records: impl IntoIterator<Item = &'a IssueRecord>,
        key: impl Fn(&IssueRecord) -> Option<K>,
    ) -> BTreeMap<K, Kpi> {
        let mut groups: BTreeMap<K, Vec<&IssueRecord>> = BTreeMap::new();
        for record in records {
            if let Some(k) = key(record) {
                groups.entry(k).or_default().push(record);
            }
        }
        groups
            .into_iter()
            .map(|(k, records)| (k, Kpi::from_records(records)))
            .collect()
    }
}

/// What `aegis report` prints.
#[derive(Debug, Clone, PartialEq)]
pub enum Report<L> {
    /// Every run in the ledger.
    All(Kpi),
    /// The latest run of each issue of the set, in total and per language
    /// of the set (a language with no run yet shows empty KPIs).
    Set {
        total: Kpi,
        languages: Vec<(L, Kpi)>,
    },
}

impl<L: Ord + Clone> Report<L> {
    pub fn all(records: &[IssueRecord]) -> Self {
        Self::All(Kpi::from_records(records))
    }

    /// Other runs in the ledger (a live API check, a manual `aegis run`)
    /// would skew the set's numbers, so only runs of the set count.
    pub fn of_set(records: &[IssueRecord], set: &[SetIssue<L>]) -> Self {
        let ids_of = |language: Option<&L>| -> Vec<String> {
            set.iter()
                .filter(|i| language.is_none_or(|l| i.language == *l))
                .map(|i| i.id.to_ascii_lowercase())
                .collect()
        };
        let mut languages: Vec<L> = set.iter().map(|i| i.language.clone()).collect();
        languages.sort();
        languages.dedup();
        Self::Set {
            total: Kpi::from_records(latest_runs(records, &ids_of(None))),
            languages: languages
                .into_iter()
                .map(|l| {
                    let kpi = Kpi::from_records(latest_runs(records, &ids_of(Some(&l))));
                    (l, kpi)
                })
                .collect(),
        }
    }
}

impl<L: fmt::Display> fmt::Display for Report<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All(kpi) => write!(f, "## All runs\n{kpi}"),
            Self::Set { total, languages } => {
                write!(f, "## Latest run of each issue in the set\n{total}")?;
                for (language, kpi) in languages {
                    write!(f, "\n## {language}\n{kpi}")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueId, LedgerEvent, MemoryLedger, Outcome};

    fn finished(id: &str, outcome: Outcome) -> IssueRecord {
        let mut r = IssueRecord::new(IssueId::parse(id).unwrap());
        r.outcome = Some(outcome);
        r
    }

    #[test]
    fn only_the_latest_run_of_each_set_issue_counts() {
        let records = [
            finished("scrapy-7604-r2", Outcome::PrOpened),
            finished("scrapy-7604-r10", Outcome::StoppedBudget),
            finished("scrapy-76040-r1", Outcome::PrOpened),
            finished("SCRAPY-7604-R1X", Outcome::PrOpened),
            finished("typst-1-r1", Outcome::PrOpened),
        ];
        let ids = ["scrapy-7604".to_owned()];
        let latest: Vec<_> = latest_runs(&records, &ids)
            .iter()
            .map(|r| r.issue_id.as_str())
            .collect();
        assert_eq!(latest, ["SCRAPY-7604-R10"]);

        let set = [
            SetIssue {
                id: "SCRAPY-7604".into(),
                language: "python",
            },
            SetIssue {
                id: "go-1".into(),
                language: "go",
            },
        ];
        let text = Report::of_set(&records, &set).to_string();
        assert!(
            text.starts_with("## Latest run of each issue in the set\n1 runs, 0 PRs"),
            "{text}"
        );
        // Every language of the set, in order, even one without a run.
        let go = text.find("\n## go\n0 runs").unwrap();
        let python = text.find("\n## python\n1 runs").unwrap();
        assert!(go < python, "{text}");
    }

    #[test]
    fn grouping_uses_the_same_formula_per_group() {
        let records = [
            finished("a", Outcome::PrOpened),
            finished("b", Outcome::StoppedSteps),
            finished("c", Outcome::PrOpened),
        ];
        let groups = Kpi::grouped(&records, |r| match r.issue_id.as_str() {
            "C" => None,
            id => Some(id.to_owned()),
        });
        assert_eq!(groups.keys().collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(groups["A"], Kpi::from_records(&records[..1]));
    }

    #[tokio::test]
    async fn records_are_folded_from_the_events() {
        let store = MemoryLedger::new();
        let issue = IssueId::parse("x-r1").unwrap();
        let event = LedgerEvent::Finished {
            outcome: Outcome::PrOpened,
            wall_time_s: 2.0,
        };
        store.append(&issue, &event).await.unwrap();
        let records = records(&store).await.unwrap();
        assert_eq!(records, [IssueRecord::from_events(issue, &[event])]);
        assert!(
            Report::<&str>::all(&records)
                .to_string()
                .starts_with("## All runs\n1 runs")
        );
    }
}
