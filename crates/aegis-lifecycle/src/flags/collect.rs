//! What the job read, scanned (#444): the issue (or the CI log a CI job
//! is), a review round's notes, and the repo lines the agent's tools
//! showed it ([`crate::shown`]), read at the job's base.

use std::collections::BTreeMap;
use std::path::Path;

use aegis_aci::redact;
use aegis_ledger::{InputFlag, IssueKind, LedgerEvent};

use super::detect::detect;
use crate::shown::shown;
use crate::{IssueInput, LifecycleError, ReviewRound, job};

/// Flags kept for one job; a flood of hits is shown as its first ones.
pub const MAX_FLAGS: usize = 20;

/// Every embedded instruction in what the job read, the issue first, then
/// the review, then files in the order the agent read them; at most
/// [`MAX_FLAGS`]. Each excerpt is redacted.
pub fn collect(
    issue: &IssueInput,
    round: Option<&ReviewRound>,
    events: &[LedgerEvent],
    clone: &Path,
    base: &str,
) -> Result<Vec<InputFlag>, LifecycleError> {
    let mut flags = issue_flags(issue);
    if let Some(round) = round {
        for (n, note) in round.notes.iter().enumerate() {
            flags.extend(only_flags(scan(&note.body, "review", |line| {
                format!("note {}, line {line}", n + 1)
            })));
        }
    }
    flags.extend(file_flags(events, clone, base)?);
    flags.truncate(MAX_FLAGS);
    Ok(flags)
}

/// The issue's title and body; a CI failure's body is the CI log (#250).
fn issue_flags(issue: &IssueInput) -> Vec<InputFlag> {
    let source = match issue.kind {
        Some(IssueKind::CiFailure) => "CI log",
        _ => "issue",
    };
    let mut flags: Vec<InputFlag> =
        only_flags(scan(&issue.title, source, |_| "title".to_owned())).collect();
    flags.extend(only_flags(scan(&issue.body, source, |line| {
        format!("line {line}")
    })));
    flags
}

/// The lines of files the agent was shown that match, each once, read at
/// `base`: what the repo said before the agent changed anything.
fn file_flags(
    events: &[LedgerEvent],
    clone: &Path,
    base: &str,
) -> Result<Vec<InputFlag>, LifecycleError> {
    let mut lines_of: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for s in shown(events) {
        if !lines_of.contains_key(&s.path) {
            order.push(s.path.clone());
        }
        lines_of.entry(s.path).or_default().push((s.from, s.to));
    }
    let mut flags = Vec::new();
    for path in order {
        let Some(bytes) = job::show(clone, base, &path)? else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let ranges = &lines_of[&path];
        flags.extend(
            scan(&text, "file", |line| format!("{path}:{line}"))
                .into_iter()
                .filter(|(line, _)| ranges.iter().any(|(a, b)| (*a..=*b).contains(line)))
                .map(|(_, flag)| flag),
        );
    }
    Ok(flags)
}

/// Each hit of `text`, by line, as a flag of `source` located by
/// `at(line)`.
fn scan(text: &str, source: &str, at: impl Fn(usize) -> String) -> Vec<(usize, InputFlag)> {
    detect(text)
        .into_iter()
        .map(|hit| {
            let flag = InputFlag {
                source: source.to_owned(),
                location: at(hit.line),
                rule: hit.rule.to_owned(),
                excerpt: redact(&hit.excerpt).text,
            };
            (hit.line, flag)
        })
        .collect()
}

fn only_flags(hits: Vec<(usize, InputFlag)>) -> impl Iterator<Item = InputFlag> {
    hits.into_iter().map(|(_, flag)| flag)
}

#[cfg(test)]
mod tests;
