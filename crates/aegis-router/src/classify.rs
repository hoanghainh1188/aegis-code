//! Light's issue classification, a fixed JSON object (design §5.2).

use aegis_ledger::IssueKind;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Classification {
    pub kind: IssueKind,
    /// `false`: Core must write a reproduction test before patching.
    pub has_repro_info: bool,
    #[serde(default)]
    pub suspected_crates: Vec<String>,
    pub summary: String,
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
    let parsed: Classification = serde_json::from_str(&reply[start..=end])
        .map_err(|e| ClassifyError::Schema(e.to_string()))?;
    if parsed.summary.trim().is_empty() {
        return Err(ClassifyError::EmptySummary);
    }
    Ok(parsed)
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
