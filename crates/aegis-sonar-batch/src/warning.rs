//! Reading the warnings: where they are, and whether they flow.

use aegis_gate::SonarIssue;
use serde::Deserialize;

use crate::BatchError;

#[derive(Deserialize)]
struct Search {
    issues: Vec<SonarIssue>,
}

/// The issues of a saved `/api/issues/search` response.
pub fn parse_search(json: &str) -> Result<Vec<SonarIssue>, BatchError> {
    serde_json::from_str::<Search>(json)
        .map(|s| s.issues)
        .map_err(|e| BatchError::Search(e.to_string()))
}

/// Path in the repo of a component `<project key>:<path>`.
pub fn path_of(component: &str) -> &str {
    component.split_once(':').map_or(component, |(_, p)| p)
}

/// A warning with secondary locations: a data flow from source to sink,
/// worked on as a group of its own (design §3.4).
pub fn is_flow(issue: &SonarIssue) -> bool {
    issue.flows.iter().any(|f| !f.locations.is_empty())
}

/// 1-based line the warning starts on; `None` for a file-level warning.
pub(crate) fn line_of(issue: &SonarIssue) -> Option<usize> {
    let line = issue.text_range.map(|r| r.start_line).or(issue.line)?;
    usize::try_from(line).ok().filter(|l| *l > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_search_response_with_ranges_and_flows() {
        let issues = parse_search(
            r#"{"total": 2, "paging": {"total": 2}, "issues": [
              {"key": "A", "rule": "python:S1481", "severity": "MINOR",
               "component": "p:src/a.py", "line": 3, "message": "unused",
               "textRange": {"startLine": 3, "endLine": 3, "startOffset": 4, "endOffset": 5},
               "flows": []},
              {"key": "B", "rule": "pythonsecurity:S3649", "component": "p:src/b.py",
               "message": "injection", "textRange": {"startLine": 9, "endLine": 9},
               "flows": [{"locations": [
                 {"component": "p:src/c.py", "textRange": {"startLine": 2, "endLine": 2},
                  "msg": "source"}]}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(path_of(&issues[0].component), "src/a.py");
        assert_eq!(line_of(&issues[0]), Some(3));
        assert!(!is_flow(&issues[0]));
        assert!(is_flow(&issues[1]));
        assert_eq!(
            path_of(&issues[1].flows[0].locations[0].component),
            "src/c.py"
        );
        assert!(parse_search("[]").is_err());
    }
}
