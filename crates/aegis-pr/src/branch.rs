//! The only branches the agent writes: `agent/<issue-id>` (design §7).

use std::fmt;

use crate::PrError;

pub(crate) const PREFIX: &str = "agent/";
const MAX_ID_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBranch(String);

impl AgentBranch {
    /// `agent/<id>`; the id is letters, digits, `-`, `_` and `.`, not
    /// starting with `.` or `-` and without `..`, so it is a plain git ref.
    pub fn for_issue(id: &str) -> Result<Self, PrError> {
        let valid = !id.is_empty()
            && id.chars().count() <= MAX_ID_CHARS
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            && !id.starts_with(['.', '-'])
            && !id.ends_with('.')
            && !id.contains("..")
            && !id.ends_with(".lock");
        if valid {
            Ok(Self(format!("{PREFIX}{id}")))
        } else {
            Err(PrError::InvalidIssueId(id.to_owned()))
        }
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn full_ref(&self) -> String {
        format!("refs/heads/{}", self.0)
    }
}

/// The base an eval run's PR targets: `eval/<run-id>`, set by the platform
/// at the issue's base commit (#98). The agent never pushes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalBranch(String);

impl EvalBranch {
    pub fn for_run(id: &str) -> Result<Self, PrError> {
        let agent = AgentBranch::for_issue(id)?;
        Ok(Self(agent.name().replacen(PREFIX, "eval/", 1)))
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn full_ref(&self) -> String {
        format!("refs/heads/{}", self.0)
    }
}

impl fmt::Display for AgentBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_issue_ids_make_a_branch() {
        assert_eq!(AgentBranch::for_issue("42").unwrap().name(), "agent/42");
        assert_eq!(
            AgentBranch::for_issue("PROJ-7").unwrap().full_ref(),
            "refs/heads/agent/PROJ-7"
        );
        for bad in [
            "", "../main", "a/b", "-x", ".x", "x.", "a..b", "x.lock", "a b", "a:b", "é",
        ] {
            assert!(AgentBranch::for_issue(bad).is_err(), "{bad}");
        }
        assert!(AgentBranch::for_issue(&"x".repeat(65)).is_err());
    }
}
