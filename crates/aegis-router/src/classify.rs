//! Light's issue classification, a fixed JSON object (design §5.2).

use aegis_ledger::{IssueKind, Triage, TriageCategory};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

/// Longest triage reason kept.
const MAX_REASON_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub kind: IssueKind,
    /// `false`: Core must write a reproduction test before patching.
    pub has_repro_info: bool,
    pub suspected_crates: Vec<String>,
    pub summary: String,
    /// What the ticket needs (#497, ADR 0012); `None` when Light gave no
    /// usable triage. Recorded only: the job works on the code anyway.
    pub triage: Option<Triage>,
}

/// The reply as Light writes it; the triage fields are read leniently, so
/// a reply without them, or with them wrong, still classifies the issue.
#[derive(Deserialize)]
struct Reply {
    kind: IssueKind,
    has_repro_info: bool,
    #[serde(default)]
    suspected_crates: Vec<String>,
    summary: String,
    #[serde(default)]
    triage: Value,
    #[serde(default)]
    triage_confidence: Value,
    #[serde(default)]
    triage_reason: Value,
}

impl Reply {
    fn triage(&self) -> Option<Triage> {
        let category = TriageCategory::parse(self.triage.as_str()?)?;
        let confidence = self
            .triage_confidence
            .as_f64()
            .filter(|c| (0.0..=1.0).contains(c))?;
        let reason: String = self
            .triage_reason
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_REASON_CHARS)
            .collect();
        Some(Triage {
            category,
            confidence,
            reason,
        })
    }
}

impl Classification {
    /// Sonar issues go through the batching step first (design §3.4).
    pub fn needs_sonar_batching(&self) -> bool {
        matches!(self.kind, IssueKind::Sonar)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClassifyError {
    #[error("no JSON object in Light's reply")]
    NoJson,
    #[error("Light's JSON does not match the schema: {0}")]
    Schema(String),
    #[error("Light returned an empty summary")]
    EmptySummary,
}

/// Parses Light's reply. Text or a code fence around the object is
/// tolerated; the object itself must match the schema.
pub fn parse_classification(reply: &str) -> Result<Classification, ClassifyError> {
    let start = reply.find('{').ok_or(ClassifyError::NoJson)?;
    let end = reply.rfind('}').ok_or(ClassifyError::NoJson)?;
    if end < start {
        return Err(ClassifyError::NoJson);
    }
    let reply: Reply = serde_json::from_str(&reply[start..=end])
        .map_err(|e| ClassifyError::Schema(e.to_string()))?;
    if reply.summary.trim().is_empty() {
        return Err(ClassifyError::EmptySummary);
    }
    let triage = reply.triage();
    Ok(Classification {
        kind: reply.kind,
        has_repro_info: reply.has_repro_info,
        suspected_crates: reply.suspected_crates,
        summary: reply.summary,
        triage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_fixed_object() {
        let c = parse_classification(
            r#"{"kind": "bug", "has_repro_info": true, "suspected_crates": ["core", "io"], "summary": "panic on empty input"}"#,
        )
        .unwrap();
        assert_eq!(c.kind, IssueKind::Bug);
        assert!(c.has_repro_info);
        assert_eq!(c.suspected_crates, vec!["core", "io"]);
        assert!(!c.needs_sonar_batching());
    }

    #[test]
    fn tolerates_a_code_fence() {
        let reply = "Here it is:\n```json\n{\"kind\": \"sonar\", \"has_repro_info\": false, \"summary\": \"S1481\"}\n```";
        let c = parse_classification(reply).unwrap();
        assert_eq!(c.kind, IssueKind::Sonar);
        assert!(c.suspected_crates.is_empty());
        assert!(c.needs_sonar_batching());
    }

    #[test]
    fn reads_the_triage_when_light_gives_one() {
        let c = parse_classification(
            r#"{"kind": "bug", "has_repro_info": false, "summary": "timeouts",
                "triage": "ops_or_config", "triage_confidence": 0.8,
                "triage_reason": "The gateway\n times out;   no code path changed."}"#,
        )
        .unwrap();
        let t = c.triage.unwrap();
        assert_eq!(t.category, TriageCategory::OpsOrConfig);
        assert!((t.confidence - 0.8).abs() < 1e-9);
        assert_eq!(t.reason, "The gateway times out; no code path changed.");
    }

    #[test]
    fn a_reply_without_a_usable_triage_still_classifies() {
        for extra in [
            "",
            r#", "triage": "feature", "triage_confidence": 0.9"#,
            r#", "triage": "code_bug", "triage_confidence": 1.5"#,
            r#", "triage": "code_bug""#,
            r#", "triage": 3, "triage_confidence": 0.5"#,
        ] {
            let reply =
                format!(r#"{{"kind": "bug", "has_repro_info": true, "summary": "x"{extra}}}"#);
            let c = parse_classification(&reply).unwrap();
            assert_eq!(c.kind, IssueKind::Bug, "{extra}");
            assert_eq!(c.triage, None, "{extra}");
        }
        // No reason is an empty one.
        let c = parse_classification(
            r#"{"kind": "bug", "has_repro_info": true, "summary": "x",
                "triage": "needs_info", "triage_confidence": 0}"#,
        )
        .unwrap();
        assert_eq!(c.triage.unwrap().reason, "");
        // A long reason is cut.
        let long = "a".repeat(1000);
        let c = parse_classification(&format!(
            r#"{{"kind": "bug", "has_repro_info": true, "summary": "x",
                "triage": "data_issue", "triage_confidence": 0.5, "triage_reason": "{long}"}}"#
        ))
        .unwrap();
        assert_eq!(c.triage.unwrap().reason.chars().count(), MAX_REASON_CHARS);
    }

    #[test]
    fn rejects_bad_replies() {
        assert_eq!(parse_classification("no idea"), Err(ClassifyError::NoJson));
        assert_eq!(parse_classification("} {"), Err(ClassifyError::NoJson));
        assert!(matches!(
            parse_classification(r#"{"kind": "feature", "has_repro_info": true, "summary": "x"}"#),
            Err(ClassifyError::Schema(_))
        ));
        assert!(matches!(
            parse_classification(r#"{"kind": "bug", "summary": "x"}"#),
            Err(ClassifyError::Schema(_))
        ));
        assert_eq!(
            parse_classification(
                r#"{"kind": "ci_failure", "has_repro_info": true, "summary": " "}"#
            ),
            Err(ClassifyError::EmptySummary)
        );
    }
}
