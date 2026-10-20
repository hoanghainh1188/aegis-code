//! Ledger events and the per-issue record built from them (design §8).

use std::collections::BTreeMap;
use std::fmt;

use aegis_provider::Usage;
use serde::{Deserialize, Serialize};

use crate::LedgerError;

const MAX_ISSUE_ID_LEN: usize = 128;

/// Issue identifier, also used as a file name by [`crate::JsonFileLedger`],
/// so it is limited to `[A-Za-z0-9._-]` and may not start with `.`.
///
/// Stored upper-cased: on case-insensitive filesystems (macOS) `gh-7` and
/// `GH-7` would share a file, so they are the same issue (#56). Tracker keys
/// such as Jira's are upper-case and case-insensitive anyway.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IssueId(String);

impl IssueId {
    pub fn parse(id: impl Into<String>) -> Result<Self, LedgerError> {
        let id = id.into();
        let reason = if id.is_empty() {
            Some("empty")
        } else if id.len() > MAX_ISSUE_ID_LEN {
            Some("longer than 128 bytes")
        } else if id.starts_with('.') {
            Some("starts with `.`")
        } else if !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            Some("only ASCII letters, digits, `.`, `_` and `-` are allowed")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(LedgerError::InvalidIssueId { id, reason }),
            None => Ok(Self(id.to_ascii_uppercase())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for IssueId {
    type Error = LedgerError;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        Self::parse(id)
    }
}

impl From<IssueId> for String {
    fn from(id: IssueId) -> Self {
        id.0
    }
}

impl fmt::Display for IssueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    Bug,
    CiFailure,
    Sonar,
}

/// Model tier (design §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Light,
    Core,
    Expert,
}

impl Tier {
    /// Light calls do not count towards the 20-call limit (design §5.3).
    pub fn counts_as_model_call(self) -> bool {
        match self {
            Self::Light => false,
            Self::Core | Self::Expert => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    PrOpened,
    StoppedBudget,
    StoppedSteps,
    StoppedExpertFail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Accepted,
    AcceptedWithEdits,
    Rejected,
}

/// Filled in by the reviewer after handling the PR (design §8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub verdict: ReviewVerdict,
    pub lines_edited: u32,
    pub review_minutes: f64,
}

/// Tokens by price class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenCounts {
    /// Uncached input.
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

impl TokenCounts {
    /// Cache writes the backend did not split by TTL are counted as one-hour
    /// writes, the dearer rate, so the budget is never underestimated.
    pub fn from_usage(usage: &Usage) -> Self {
        let split = usage.cache_creation;
        let attributed = split.ephemeral_5m_input_tokens + split.ephemeral_1h_input_tokens;
        let unattributed = usage.cache_creation_input_tokens.saturating_sub(attributed);
        Self {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_write_5m: split.ephemeral_5m_input_tokens,
            cache_write_1h: split.ephemeral_1h_input_tokens + unattributed,
            cache_read: usage.cache_read_input_tokens,
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_write_5m: self.cache_write_5m + other.cache_write_5m,
            cache_write_1h: self.cache_write_1h + other.cache_write_1h,
            cache_read: self.cache_read + other.cache_read,
        }
    }
}

/// One entry in the append-only ledger of an issue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LedgerEvent {
    Classified {
        kind: IssueKind,
    },
    /// `cost_usd` is priced when the call happens, so later price changes do
    /// not rewrite history.
    ModelCall {
        tier: Tier,
        model: String,
        tokens: TokenCounts,
        cost_usd: f64,
    },
    GateRun(GateRun),
    /// Secret values hidden from the model in tool output or input text
    /// (design §7).
    SecretsRedacted {
        count: u32,
    },
    Finished {
        outcome: Outcome,
        wall_time_s: f64,
    },
    Reviewed(Review),
}

impl LedgerEvent {
    /// JSON has no NaN or infinity: serde_json would write `null` and the
    /// event could not be read back, so stores refuse such events.
    pub(crate) fn ensure_finite(&self) -> Result<(), LedgerError> {
        let fields: &[(&'static str, f64)] = match self {
            Self::Classified { .. } | Self::SecretsRedacted { .. } => &[],
            Self::ModelCall { cost_usd, .. } => &[("cost_usd", *cost_usd)],
            Self::GateRun(run) => &[("duration_s", run.duration_s)],
            Self::Finished { wall_time_s, .. } => &[("wall_time_s", *wall_time_s)],
            Self::Reviewed(review) => &[("review_minutes", review.review_minutes)],
        };
        match fields.iter().find(|(_, value)| !value.is_finite()) {
            Some(&(field, value)) => Err(LedgerError::NonFinite { field, value }),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateRun {
    pub passed: bool,
    /// First failing step (`fmt`, `clippy`, `test`, …); `None` when passed.
    pub failed_step: Option<String>,
    pub duration_s: f64,
}

/// Per-issue record (design §8). `outcome` is `None` while the issue is
/// still running or was interrupted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueRecord {
    pub issue_id: IssueId,
    pub kind: Option<IssueKind>,
    pub outcome: Option<Outcome>,
    /// Tiers in order of first use.
    pub tiers_used: Vec<Tier>,
    /// Core + Expert calls (Light excluded, design §5.3).
    pub model_calls: u32,
    /// Tokens per model ID.
    pub tokens: BTreeMap<String, TokenCounts>,
    /// Total, including Light calls and the regional surcharge.
    pub cost_usd: f64,
    pub gate_runs: Vec<GateRun>,
    pub wall_time_s: Option<f64>,
    pub review: Option<Review>,
    /// Secret values hidden from the model during the run.
    #[serde(default)]
    pub secrets_redacted: u32,
}

impl IssueRecord {
    pub fn new(issue_id: IssueId) -> Self {
        Self {
            issue_id,
            kind: None,
            outcome: None,
            tiers_used: Vec::new(),
            model_calls: 0,
            tokens: BTreeMap::new(),
            cost_usd: 0.0,
            gate_runs: Vec::new(),
            wall_time_s: None,
            review: None,
            secrets_redacted: 0,
        }
    }

    pub fn from_events<'a>(
        issue_id: IssueId,
        events: impl IntoIterator<Item = &'a LedgerEvent>,
    ) -> Self {
        events.into_iter().fold(Self::new(issue_id), Self::apply)
    }

    pub fn apply(mut self, event: &LedgerEvent) -> Self {
        match event {
            LedgerEvent::Classified { kind } => self.kind = Some(*kind),
            LedgerEvent::ModelCall {
                tier,
                model,
                tokens,
                cost_usd,
            } => {
                if !self.tiers_used.contains(tier) {
                    self.tiers_used.push(*tier);
                }
                if tier.counts_as_model_call() {
                    self.model_calls += 1;
                }
                let total = self.tokens.get(model).copied().unwrap_or_default();
                self.tokens.insert(model.clone(), total.add(*tokens));
                self.cost_usd += cost_usd;
            }
            LedgerEvent::GateRun(run) => self.gate_runs.push(run.clone()),
            LedgerEvent::SecretsRedacted { count } => {
                self.secrets_redacted = self.secrets_redacted.saturating_add(*count);
            }
            LedgerEvent::Finished {
                outcome,
                wall_time_s,
            } => {
                self.outcome = Some(*outcome);
                self.wall_time_s = Some(*wall_time_s);
            }
            LedgerEvent::Reviewed(review) => self.review = Some(*review),
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use aegis_provider::CacheCreation;
    use serde_json::json;

    use super::*;

    fn id(s: &str) -> IssueId {
        IssueId::parse(s).unwrap()
    }

    fn call(tier: Tier, model: &str, input: u64, cost_usd: f64) -> LedgerEvent {
        LedgerEvent::ModelCall {
            tier,
            model: model.into(),
            tokens: TokenCounts {
                input,
                ..TokenCounts::default()
            },
            cost_usd,
        }
    }

    #[test]
    fn issue_id_accepts_tracker_ids() {
        for ok in ["GH-123", "PROJ-7", "SONAR_2026.10", "42"] {
            assert_eq!(id(ok).as_str(), ok);
        }
    }

    #[test]
    fn issue_id_ignores_case() {
        assert_eq!(id("gh-7"), id("GH-7"));
        assert_eq!(id("sonar_2026.10").as_str(), "SONAR_2026.10");
        assert_eq!(
            serde_json::from_value::<IssueId>(json!("proj-1")).unwrap(),
            id("PROJ-1")
        );
    }

    #[test]
    fn issue_id_rejects_paths_and_odd_input() {
        let long = "a".repeat(129);
        for bad in ["", "../etc", "a/b", ".hidden", "a b", "é", long.as_str()] {
            assert!(IssueId::parse(bad).is_err(), "{bad:?} accepted");
        }
        assert!(serde_json::from_value::<IssueId>(json!("../x")).is_err());
    }

    #[test]
    fn token_counts_split_writes_by_ttl() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_input_tokens: 700,
            cache_read_input_tokens: 30,
            cache_creation: CacheCreation {
                ephemeral_5m_input_tokens: 100,
                ephemeral_1h_input_tokens: 500,
            },
        };
        assert_eq!(
            TokenCounts::from_usage(&usage),
            TokenCounts {
                input: 10,
                output: 20,
                cache_write_5m: 100,
                // 500 reported as 1h + 100 not attributed to any TTL.
                cache_write_1h: 600,
                cache_read: 30,
            }
        );
    }

    #[test]
    fn token_counts_without_split_count_writes_as_one_hour() {
        let usage = Usage {
            cache_creation_input_tokens: 900,
            ..Usage::default()
        };
        let counts = TokenCounts::from_usage(&usage);
        assert_eq!((counts.cache_write_5m, counts.cache_write_1h), (0, 900));
    }

    #[test]
    fn record_folds_events() {
        let events = [
            LedgerEvent::Classified {
                kind: IssueKind::Bug,
            },
            call(Tier::Light, "claude-haiku-5-5", 100, 0.001),
            call(Tier::Core, "claude-sonnet-5-5", 1_000, 0.5),
            LedgerEvent::GateRun(GateRun {
                passed: false,
                failed_step: Some("test".into()),
                duration_s: 90.0,
            }),
            call(Tier::Core, "claude-sonnet-5-5", 2_000, 0.25),
            call(Tier::Expert, "claude-opus-5-5", 500, 1.0),
            LedgerEvent::GateRun(GateRun {
                passed: true,
                failed_step: None,
                duration_s: 80.0,
            }),
            LedgerEvent::Finished {
                outcome: Outcome::PrOpened,
                wall_time_s: 600.0,
            },
        ];
        let record = IssueRecord::from_events(id("GH-1"), &events);

        assert_eq!(record.kind, Some(IssueKind::Bug));
        assert_eq!(record.outcome, Some(Outcome::PrOpened));
        assert_eq!(
            record.tiers_used,
            vec![Tier::Light, Tier::Core, Tier::Expert]
        );
        assert_eq!(record.model_calls, 3, "Light is not counted");
        assert_eq!(record.tokens["claude-sonnet-5-5"].input, 3_000);
        assert_eq!(record.tokens.len(), 3);
        assert!((record.cost_usd - 1.751).abs() < 1e-12);
        assert_eq!(record.gate_runs.len(), 2);
        assert_eq!(record.gate_runs[0].failed_step.as_deref(), Some("test"));
        assert_eq!(record.wall_time_s, Some(600.0));
        assert_eq!(record.review, None);
    }

    #[test]
    fn record_serializes_design_fields() {
        let record = IssueRecord::from_events(
            id("GH-2"),
            &[
                LedgerEvent::Classified {
                    kind: IssueKind::CiFailure,
                },
                LedgerEvent::Finished {
                    outcome: Outcome::StoppedBudget,
                    wall_time_s: 12.5,
                },
                LedgerEvent::Reviewed(Review {
                    verdict: ReviewVerdict::AcceptedWithEdits,
                    lines_edited: 4,
                    review_minutes: 7.0,
                }),
            ],
        );
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["issue_id"], "GH-2");
        assert_eq!(value["kind"], "ci_failure");
        assert_eq!(value["outcome"], "stopped_budget");
        assert_eq!(value["review"]["verdict"], "accepted_with_edits");
        for field in [
            "tiers_used",
            "model_calls",
            "tokens",
            "cost_usd",
            "gate_runs",
            "wall_time_s",
        ] {
            assert!(value.get(field).is_some(), "missing {field}");
        }
    }

    #[test]
    fn events_round_trip_as_tagged_json() {
        let event = LedgerEvent::GateRun(GateRun {
            passed: false,
            failed_step: Some("clippy".into()),
            duration_s: 3.0,
        });
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event"], "gate_run");
        assert_eq!(value["failed_step"], "clippy");
        assert_eq!(serde_json::from_value::<LedgerEvent>(value).unwrap(), event);
    }
}
