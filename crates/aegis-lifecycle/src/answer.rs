//! The agent's final answer, the JSON object `prompts/system.md` asks for.

use aegis_gate::Repro;
use aegis_pr::{AllowedReferenceChange, AllowedTestChange, ChangeNote};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Fixed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawChange {
    location: String,
    note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawRepro {
    name: String,
    #[serde(default)]
    files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawAllowed {
    test: String,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawGroup {
    references: String,
    #[serde(default = "every_test")]
    tests: String,
    reason: String,
}

fn every_test() -> String {
    "*".into()
}

/// What the agent did about one note of a review (#243).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReviewReply {
    /// The note's number in the review the round was given; a model may
    /// write it as a string.
    #[serde(deserialize_with = "number_or_text")]
    pub note: u32,
    pub done: bool,
    #[serde(default)]
    pub reply: String,
}

fn number_or_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Note {
        Number(u32),
        Text(String),
    }
    match Note::deserialize(d)? {
        Note::Number(n) => Ok(n),
        Note::Text(t) => t
            .trim()
            .trim_start_matches('#')
            .parse()
            .map_err(|_| serde::de::Error::custom(format!("`{t}` is not a note number"))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Raw {
    status: Status,
    #[serde(default)]
    cause: String,
    #[serde(default)]
    changes: Vec<RawChange>,
    #[serde(default)]
    repro_test: Option<RawRepro>,
    #[serde(default)]
    allowed_test_changes: Vec<RawAllowed>,
    #[serde(default)]
    allowed_reference_changes: Vec<RawGroup>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    review_replies: Vec<ReviewReply>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinalAnswer {
    pub status: Status,
    pub cause: String,
    pub changes: Vec<ChangeNote>,
    pub repro: Option<Repro>,
    pub allowed_test_changes: Vec<AllowedTestChange>,
    /// Groups of tests whose references may change together (#147).
    pub allowed_reference_changes: Vec<AllowedReferenceChange>,
    pub stop_reason: Option<String>,
    /// A review round's answer to each note.
    pub review_replies: Vec<ReviewReply>,
}

/// The JSON object in the agent's last reply; text or a fence around it
/// is tolerated. `Err` says what to fix, for one more try.
pub fn parse_answer(reply: &str) -> Result<FinalAnswer, String> {
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err("no JSON object found".into());
    };
    if end < start {
        return Err("no JSON object found".into());
    }
    let raw: Raw = serde_json::from_str(&reply[start..=end]).map_err(|e| e.to_string())?;
    if raw.status == Status::Fixed && raw.changes.is_empty() {
        return Err("`status` is `fixed` but `changes` is empty".into());
    }
    Ok(FinalAnswer {
        status: raw.status,
        cause: raw.cause,
        changes: raw
            .changes
            .into_iter()
            .map(|c| ChangeNote {
                location: c.location,
                note: c.note,
            })
            .collect(),
        repro: raw.repro_test.map(|r| Repro {
            test: r.name,
            files: r.files,
        }),
        allowed_test_changes: raw
            .allowed_test_changes
            .into_iter()
            .map(|a| AllowedTestChange {
                test: a.test,
                reason: a.reason,
            })
            .collect(),
        allowed_reference_changes: raw
            .allowed_reference_changes
            .into_iter()
            .map(|g| AllowedReferenceChange {
                references: g.references,
                tests: g.tests,
                reason: g.reason,
            })
            .collect(),
        stop_reason: raw.stop_reason,
        review_replies: raw.review_replies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_final_answer() {
        let reply = r#"Done.
```json
{"status": "fixed", "cause": "off by one", "changes": [{"location": "src/lib.rs:3", "note": "use >="}],
 "repro_test": {"name": "fourth_item_is_refused", "files": ["tests/fourth.rs"]},
 "allowed_test_changes": [], "stop_reason": null}
```"#;
        let a = parse_answer(reply).unwrap();
        assert_eq!(a.status, Status::Fixed);
        assert_eq!(a.repro.unwrap().files, ["tests/fourth.rs"]);
        let stopped = parse_answer(r#"{"status": "stopped", "stop_reason": "unclear"}"#).unwrap();
        assert_eq!(stopped.stop_reason.as_deref(), Some("unclear"));
        assert!(parse_answer("I fixed it").is_err());
        assert!(
            parse_answer(r#"{"status": "fixed"}"#)
                .unwrap_err()
                .contains("changes")
        );
        assert!(parse_answer(r#"{"status": "maybe"}"#).is_err());
    }

    #[test]
    fn reads_groups_of_reference_changes() {
        let a = parse_answer(
            r#"{"status": "fixed", "changes": [{"location": "a.rs:1", "note": "n"}],
             "allowed_reference_changes": [
               {"references": "tests/ref/html", "reason": "MathML spacing"},
               {"references": "tests/ref/render", "tests": "math-*", "reason": "r"}]}"#,
        )
        .unwrap();
        let groups: Vec<(&str, &str)> = a
            .allowed_reference_changes
            .iter()
            .map(|g| (g.references.as_str(), g.tests.as_str()))
            .collect();
        assert_eq!(
            groups,
            [("tests/ref/html", "*"), ("tests/ref/render", "math-*")]
        );
        assert!(
            parse_answer(
                r#"{"status": "fixed", "changes": [{"location": "a.rs:1", "note": "n"}],
                 "allowed_reference_changes": [{"references": "tests/ref"}]}"#
            )
            .unwrap_err()
            .contains("reason")
        );
    }
}
