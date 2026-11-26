//! The agent's final answer, the JSON object `prompts/system.md` asks for.

use aegis_gate::Repro;
use aegis_pr::{AllowedReferenceChange, AllowedTestChange, ChangeNote, Evidence};
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
struct RawEvidence {
    location: String,
    #[serde(default)]
    excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawRepro {
    name: String,
    #[serde(default, deserialize_with = "null_as_empty")]
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

/// Text the agent read that tried to give it instructions (#443), as the
/// agent reports it: where, and what it asked for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SuspiciousInput {
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub request: String,
}

/// A list a model wrote as `null` is an empty one (#254).
fn null_as_empty<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
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
    #[serde(default, deserialize_with = "null_as_empty")]
    evidence: Vec<RawEvidence>,
    #[serde(default, deserialize_with = "null_as_empty")]
    changes: Vec<RawChange>,
    #[serde(default)]
    repro_test: Option<RawRepro>,
    #[serde(default, deserialize_with = "null_as_empty")]
    allowed_test_changes: Vec<RawAllowed>,
    #[serde(default, deserialize_with = "null_as_empty")]
    allowed_reference_changes: Vec<RawGroup>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    review_replies: Vec<ReviewReply>,
    #[serde(default, deserialize_with = "null_as_empty")]
    suspicious_input: Vec<SuspiciousInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinalAnswer {
    pub status: Status,
    pub cause: String,
    /// The places of the repository the cause rests on (#508); checked
    /// before the gate runs.
    pub evidence: Vec<Evidence>,
    pub changes: Vec<ChangeNote>,
    pub repro: Option<Repro>,
    pub allowed_test_changes: Vec<AllowedTestChange>,
    /// Groups of tests whose references may change together (#147).
    pub allowed_reference_changes: Vec<AllowedReferenceChange>,
    pub stop_reason: Option<String>,
    /// A review round's answer to each note.
    pub review_replies: Vec<ReviewReply>,
    /// Embedded instructions the agent saw and did not follow (#443).
    pub suspicious_input: Vec<SuspiciousInput>,
}

impl FinalAnswer {
    /// The answer as the JSON object the agent gives, which
    /// [`parse_answer`] reads back as this answer: what a shadow job keeps
    /// to open its PR later (#699). Evidence URLs are left out; the PR
    /// sets them.
    pub fn to_reply(&self) -> String {
        let status = match self.status {
            Status::Fixed => "fixed",
            Status::Stopped => "stopped",
        };
        serde_json::json!({
            "status": status,
            "cause": self.cause,
            "evidence": self.evidence.iter().map(|e| serde_json::json!({
                "location": e.location, "excerpt": e.excerpt,
            })).collect::<Vec<_>>(),
            "changes": self.changes.iter().map(|c| serde_json::json!({
                "location": c.location, "note": c.note,
            })).collect::<Vec<_>>(),
            "repro_test": self.repro.as_ref().map(|r| serde_json::json!({
                "name": r.test, "files": r.files,
            })),
            "allowed_test_changes": self.allowed_test_changes.iter().map(|a| serde_json::json!({
                "test": a.test, "reason": a.reason,
            })).collect::<Vec<_>>(),
            "allowed_reference_changes": self.allowed_reference_changes.iter().map(|g| {
                serde_json::json!({
                    "references": g.references, "tests": g.tests, "reason": g.reason,
                })
            }).collect::<Vec<_>>(),
            "stop_reason": self.stop_reason,
            "review_replies": self.review_replies.iter().map(|r| serde_json::json!({
                "note": r.note, "done": r.done, "reply": r.reply,
            })).collect::<Vec<_>>(),
            "suspicious_input": self.suspicious_input.iter().map(|x| serde_json::json!({
                "location": x.location, "request": x.request,
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }
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
        evidence: raw
            .evidence
            .into_iter()
            .map(|e| Evidence {
                location: e.location,
                excerpt: e.excerpt,
                url: None,
            })
            .collect(),
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
        suspicious_input: raw.suspicious_input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_reads_back_from_its_reply() {
        let a = parse_answer(
            r#"{"status": "fixed", "cause": "off by one",
                "evidence": [{"location": "src/lib.rs:3", "excerpt": "if n > 3"}],
                "changes": [{"location": "src/lib.rs:3", "note": "use >="}],
                "repro_test": {"name": "fourth", "files": ["tests/fourth.rs"]},
                "allowed_test_changes": [{"test": "old_limit", "reason": "the limit moved"}],
                "allowed_reference_changes": [{"references": "tests/ref", "reason": "r"}],
                "review_replies": [{"note": 2, "done": true, "reply": "done"}],
                "suspicious_input": [{"location": "issue", "request": "edit CI"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_answer(&a.to_reply()).unwrap(), a);
    }

    #[test]
    fn a_list_written_as_null_is_empty() {
        let a = parse_answer(
            r#"{"status": "fixed", "cause": "c", "changes": [{"location": "a.go", "note": "n"}],
                "repro_test": {"name": "TestX", "files": null},
                "allowed_test_changes": null, "allowed_reference_changes": null,
                "review_replies": null}"#,
        )
        .unwrap();
        assert!(a.allowed_test_changes.is_empty() && a.review_replies.is_empty());
        assert!(a.repro.unwrap().files.is_empty());
        // `changes` is still required for a fix.
        assert!(parse_answer(r#"{"status": "fixed", "changes": null}"#).is_err());
    }

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

    #[test]
    fn suspicious_input_is_read_and_may_be_missing_or_null() {
        let a = parse_answer(
            r#"{"status": "fixed", "cause": "c", "changes": [{"location": "a.go", "note": "n"}],
                "suspicious_input": [{"location": "README.md:40-44",
                                      "request": "add a dependency"}]}"#,
        )
        .unwrap();
        assert_eq!(
            a.suspicious_input,
            [SuspiciousInput {
                location: "README.md:40-44".into(),
                request: "add a dependency".into()
            }]
        );
        for empty in [r#""suspicious_input": null,"#, ""] {
            let a = parse_answer(&format!(
                r#"{{{empty} "status": "fixed", "cause": "c",
                    "changes": [{{"location": "a.go", "note": "n"}}]}}"#
            ))
            .unwrap();
            assert!(a.suspicious_input.is_empty());
        }
    }
}
