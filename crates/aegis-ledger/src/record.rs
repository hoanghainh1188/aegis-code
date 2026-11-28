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
    /// New behavior asked for (#675, ADR 0019): planned as an RFC and a
    /// DAG of sub-jobs a person approves before any code is written. Set
    /// by the feature lane, never by Light's classification.
    Feature,
    /// An error spike an APM source showed before any ticket (#706):
    /// diagnosed, never fixed, until a person asks for a fix.
    Anomaly,
    /// The repo's deprecation warnings, on its schedule (#705): fixed as a
    /// batch, the way Sonar warnings are.
    Deprecation,
}

/// What a ticket needs, as Light reads it (#497, ADR 0012): a code fix,
/// or something an operator does. Recorded only for now: every job still
/// works on the code, and the record shows how often the reading was right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageCategory {
    CodeBug,
    OpsOrConfig,
    DataIssue,
    NeedsInfo,
    /// New behavior rather than a defect (#675, ADR 0019): for the feature
    /// lane. Recorded only until that lane takes it.
    FeatureRequest,
}

impl TriageCategory {
    pub const ALL: [Self; 5] = [
        Self::CodeBug,
        Self::OpsOrConfig,
        Self::DataIssue,
        Self::NeedsInfo,
        Self::FeatureRequest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodeBug => "code_bug",
            Self::OpsOrConfig => "ops_or_config",
            Self::DataIssue => "data_issue",
            Self::NeedsInfo => "needs_info",
            Self::FeatureRequest => "feature_request",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == text)
    }
}

/// Light's triage of a ticket (#497).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Triage {
    pub category: TriageCategory,
    /// From 0 to 1.
    pub confidence: f64,
    /// One sentence, from Light: shown as text, never followed.
    pub reason: String,
}

/// Natural language of an intake ticket (#511).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketLanguage {
    En,
    Ja,
    Vi,
    Other,
}

impl TicketLanguage {
    pub const ALL: [Self; 4] = [Self::En, Self::Ja, Self::Vi, Self::Other];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::Vi => "vi",
            Self::Other => "other",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|lang| lang.as_str() == text)
    }
}

impl fmt::Display for TicketLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Strip fenced code blocks and URLs before checking natural-language scripts
/// so code identifiers or URLs do not skew classification.
fn strip_code_and_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for word in line.split_whitespace() {
            if word.starts_with("http://") || word.starts_with("https://") {
                continue;
            }
            out.push_str(word);
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{FF65}'..='\u{FF9F}')
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// Characters distinct to Vietnamese orthography (excludes shared Western
/// European diacritics like `á`/`é`/`ê`/`ó` to prevent false positives on names).
fn is_vietnamese_marker(c: char) -> bool {
    matches!(
        c,
        'ă' | 'Ă'
            | 'đ'
            | 'Đ'
            | 'ơ'
            | 'Ơ'
            | 'ư'
            | 'Ư'
            | 'ả'
            | 'Ả'
            | 'ã'
            | 'Ã'
            | 'ạ'
            | 'Ạ'
            | 'ằ'
            | 'Ằ'
            | 'ẳ'
            | 'Ẳ'
            | 'ẵ'
            | 'Ẵ'
            | 'ắ'
            | 'Ắ'
            | 'ặ'
            | 'Ặ'
            | 'ầ'
            | 'Ầ'
            | 'ẩ'
            | 'Ẩ'
            | 'ẫ'
            | 'Ẫ'
            | 'ấ'
            | 'Ấ'
            | 'ậ'
            | 'Ậ'
            | 'ẻ'
            | 'Ẻ'
            | 'ẽ'
            | 'Ẽ'
            | 'ẹ'
            | 'Ẹ'
            | 'ề'
            | 'Ề'
            | 'ể'
            | 'Ể'
            | 'ễ'
            | 'Ễ'
            | 'ế'
            | 'Ế'
            | 'ệ'
            | 'Ệ'
            | 'ỉ'
            | 'Ỉ'
            | 'ĩ'
            | 'Ĩ'
            | 'ị'
            | 'Ị'
            | 'ỏ'
            | 'Ỏ'
            | 'õ'
            | 'Õ'
            | 'ọ'
            | 'Ọ'
            | 'ồ'
            | 'Ồ'
            | 'ổ'
            | 'Ổ'
            | 'ỗ'
            | 'Ỗ'
            | 'ố'
            | 'Ố'
            | 'ộ'
            | 'Ộ'
            | 'ờ'
            | 'Ờ'
            | 'ở'
            | 'Ở'
            | 'ỡ'
            | 'Ỡ'
            | 'ớ'
            | 'Ớ'
            | 'ợ'
            | 'Ợ'
            | 'ủ'
            | 'Ủ'
            | 'ũ'
            | 'Ũ'
            | 'ụ'
            | 'Ụ'
            | 'ừ'
            | 'Ừ'
            | 'ử'
            | 'Ử'
            | 'ữ'
            | 'Ữ'
            | 'ứ'
            | 'Ứ'
            | 'ự'
            | 'Ự'
            | 'ỷ'
            | 'Ỷ'
            | 'ỹ'
            | 'Ỹ'
            | 'ỵ'
            | 'Ỵ'
    )
}

/// Deterministic, zero-cost detector of the ticket's prose language (#511).
pub fn detect_ticket_language(title: &str, body: &str) -> TicketLanguage {
    let combined = format!("{title}\n{body}");
    let stripped = strip_code_and_urls(&combined);
    let sample = if stripped.trim().is_empty() {
        combined.as_str()
    } else {
        stripped.as_str()
    };

    let mut kana = 0usize;
    let mut cjk = 0usize;
    let mut vi_markers = 0usize;
    let mut non_ascii_alpha = 0usize;
    let mut total_alpha = 0usize;

    for ch in sample.chars() {
        if !ch.is_alphabetic() {
            continue;
        }
        total_alpha += 1;
        if is_kana(ch) {
            kana += 1;
        } else if is_cjk(ch) {
            cjk += 1;
        } else if is_vietnamese_marker(ch) {
            vi_markers += 1;
        }
        if !ch.is_ascii() {
            non_ascii_alpha += 1;
        }
    }

    if kana >= 2 || (kana >= 1 && cjk >= 2) {
        TicketLanguage::Ja
    } else if vi_markers >= 2 {
        TicketLanguage::Vi
    } else if non_ascii_alpha >= 5 && total_alpha > 0 && non_ascii_alpha * 5 > total_alpha {
        TicketLanguage::Other
    } else {
        TicketLanguage::En
    }
}

/// Categorical dimensions recorded per run for bias analysis (#511).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub programming_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_language: Option<TicketLanguage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

impl TicketMeta {
    pub fn is_empty(&self) -> bool {
        self.programming_language.is_none()
            && self.ticket_source.is_none()
            && self.ticket_language.is_none()
            && self.repo.is_none()
    }

    /// Overlay any present fields in `other` onto `self`.
    pub fn merged_with(&self, other: &Self) -> Self {
        Self {
            programming_language: other
                .programming_language
                .clone()
                .or_else(|| self.programming_language.clone()),
            ticket_source: other
                .ticket_source
                .clone()
                .or_else(|| self.ticket_source.clone()),
            ticket_language: other.ticket_language.or(self.ticket_language),
            repo: other.repo.clone().or_else(|| self.repo.clone()),
        }
    }
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
    /// Triaged as no code change (#451): a read-only diagnosis was written
    /// back to the ticket, and no worktree, patch or PR was made.
    Diagnosed,
    /// A plan waits for a person's sign-off (#675, ADR 0019): the job holds
    /// no lease and has not failed; a write-authorized approval queues it
    /// again.
    AwaitingApproval,
    /// A feature's sub-job delivered as one integration PR (#678): its
    /// gate passed, and again once merged into the feature's integration
    /// branch; it opens no PR of its own, the feature's comes at the end.
    Integrated,
    /// A shadow job (#699) whose gate passed: it would have opened a PR,
    /// but its repo is in shadow mode; its patch is kept for its page and
    /// for a person to judge.
    Shadowed,
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
    /// The PR broke a test that passed before and the reviewer caught it
    /// (KPI "PR that breaks old tests reaching a person", design §8).
    #[serde(default)]
    pub broke_tests: bool,
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
    ///
    /// Providers that know the request's [`aegis_provider::CachePlan`] should
    /// call [`aegis_provider::Usage::attribute_writes_from_plan`] first so
    /// Bedrock (or any backend) that omits the split still prices 5-minute
    /// writes at the 5-minute rate (#55).
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

/// What the agent's final answer said (#316): the PR's explanation, or why
/// it stopped. Redacted and capped before it is written
/// ([`crate::IssueLedger::answered`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerSummary {
    /// `status: fixed`; false when the agent stopped.
    pub fixed: bool,
    #[serde(default)]
    pub cause: String,
    #[serde(default)]
    pub changes: Vec<AnswerChange>,
    /// The reproduction test's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro_test: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// A review round's answer to each note (#245).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_replies: Vec<AnswerReply>,
    /// Embedded instructions the agent reports it saw and did not follow
    /// (#443).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suspicious_input: Vec<AnswerSuspicious>,
}

/// One change the answer lists: `file:line` and what it does there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerChange {
    pub location: String,
    pub note: String,
}

/// A line of untrusted text the job read that reads like an instruction to
/// an AI agent (#444): where it is, the rule that matched it, and the line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFlag {
    /// `issue`, `CI log`, `review` or `file`.
    pub source: String,
    /// `path:line`, `line 12` of the issue, `note 2` of a review.
    pub location: String,
    pub rule: String,
    /// The line, trimmed, redacted and capped.
    pub excerpt: String,
}

/// One text the agent reports as trying to instruct it (#443).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerSuspicious {
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub request: String,
}

/// A review round's answer to note `note`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerReply {
    pub note: u32,
    pub done: bool,
    #[serde(default)]
    pub reply: String,
}

/// One entry in the append-only ledger of an issue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LedgerEvent {
    Classified {
        kind: IssueKind,
    },
    /// Light's triage of the ticket (#497), when it gave one.
    Triaged {
        triage: Triage,
    },
    /// The diagnosis lane took the ticket (#451): its `category`, and how
    /// many of the report's items were kept (each with a checked source)
    /// and dropped (none could be checked).
    Diagnosed {
        category: TriageCategory,
        kept: u32,
        dropped: u32,
    },
    /// The deprecation warnings a deprecation batch took (#705), by key:
    /// the repo's next scheduled batch takes only others.
    DeprecationsTaken {
        keys: Vec<String>,
    },
    /// `cost_usd` is priced when the call happens, so later price changes do
    /// not rewrite history.
    ModelCall {
        tier: Tier,
        model: String,
        tokens: TokenCounts,
        cost_usd: f64,
    },
    /// The tools the model called in the model call just before (#129).
    ToolCalls {
        tier: Tier,
        calls: Vec<ToolCall>,
    },
    /// The router reminded the agent to write and run its reproduction
    /// test after `calls` Core/Expert calls without a test run (#144).
    TestReminded {
        tier: Tier,
        calls: u32,
    },
    /// Before the first gate run, the platform asked the code graph for
    /// the code linked to the patch outside its files (#203): `files` are
    /// the files the agent was shown, none when the graph linked nothing,
    /// and `error` why the query failed.
    ImpactChecked {
        files: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The router moved the job from Core to Expert after `calls`
    /// Core/Expert calls; `reason` is the router's (design §5.4).
    Escalated {
        reason: String,
        calls: u32,
    },
    GateRun(GateRun),
    /// Secret values hidden from the model in tool output or input text
    /// (design §7).
    SecretsRedacted {
        count: u32,
    },
    /// The places a fix's answer cites, checked against the job's tree
    /// (#508): how many, how many did not hold, and whether the job was
    /// held back for it (out of tries, no PR).
    SourcesChecked {
        cited: u32,
        wrong: u32,
        held: bool,
    },
    /// Secrets kept from leaving the platform (#509): `place` is `patch`
    /// when the gate's `secrets` step failed on them, or the text they
    /// were masked in before it was sent (`commit message`, `PR`,
    /// `review reply`). Never the values.
    SecretsBlocked {
        place: String,
        count: u32,
    },
    /// The fix's own tests run on the agent's patch (eval runs, #98).
    HiddenTests {
        passed: bool,
        duration_s: f64,
    },
    Finished {
        outcome: Outcome,
        wall_time_s: f64,
    },
    Reviewed(Review),
    /// The issue the job works on, as its tracker gave it (#316): its
    /// title, redacted and capped, and where it is. Never its body.
    IssueRead {
        title: String,
        url: String,
    },
    /// The agent's final answer (#316), each time it gives one.
    Answered(AnswerSummary),
    /// Untrusted text the job read that reads like an instruction to an AI
    /// agent, found by the platform (#444).
    InputFlagged(InputFlag),
    /// Intake dimensions recorded for bias tracking (#511).
    TicketMeta(TicketMeta),
    /// What a shadow job (#699) keeps to open its PR later without the
    /// agent: its final answer, redacted, and the commit its patch is on.
    ShadowKept(ShadowKept),
}

/// [`LedgerEvent::ShadowKept`]: what the PR of a shadow job is opened
/// from, with its patch the store keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowKept {
    /// The agent's final answer, as the JSON object it gives (redacted).
    pub answer: String,
    /// The commit the patch applies to, which its gate ran against.
    pub base: String,
}

impl LedgerEvent {
    /// JSON has no NaN or infinity: serde_json would write `null` and the
    /// event could not be read back, so stores refuse such events.
    pub fn ensure_finite(&self) -> Result<(), LedgerError> {
        let fields: &[(&'static str, f64)] = match self {
            Self::Classified { .. }
            | Self::SecretsRedacted { .. }
            | Self::SecretsBlocked { .. }
            | Self::SourcesChecked { .. }
            | Self::Diagnosed { .. }
            | Self::DeprecationsTaken { .. }
            | Self::ToolCalls { .. }
            | Self::TestReminded { .. }
            | Self::ImpactChecked { .. }
            | Self::Escalated { .. }
            | Self::IssueRead { .. }
            | Self::Answered(_)
            | Self::InputFlagged(_)
            | Self::TicketMeta(_)
            | Self::ShadowKept(_) => &[],
            Self::HiddenTests { duration_s, .. } => &[("duration_s", *duration_s)],
            Self::ModelCall { cost_usd, .. } => &[("cost_usd", *cost_usd)],
            Self::GateRun(run) => &[("duration_s", run.duration_s)],
            Self::Finished { wall_time_s, .. } => &[("wall_time_s", *wall_time_s)],
            Self::Reviewed(review) => &[("review_minutes", review.review_minutes)],
            Self::Triaged { triage } => &[("triage_confidence", triage.confidence)],
        };
        match fields.iter().find(|(_, value)| !value.is_finite()) {
            Some(&(field, value)) => Err(LedgerError::NonFinite { field, value }),
            None => Ok(()),
        }
    }
}

/// One tool call, by name and target: what the agent looked at or changed,
/// never the content it wrote or read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub is_error: bool,
    /// `file:line`, `file:from-to`, a symbol, a search text or a file
    /// pattern, cut short by whoever records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateRun {
    pub passed: bool,
    /// First failing step (`fmt`, `clippy`, `test`, …); `None` when passed.
    pub failed_step: Option<String>,
    pub duration_s: f64,
    /// Cumulative sidecar startup, reset, and teardown time in seconds when sidecars ran (#454).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_s: Option<f64>,
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
    /// Secrets kept from leaving the platform (#509): found in the patch
    /// by the gate, or masked in a text before it was sent.
    #[serde(default)]
    pub secrets_blocked: u32,
    /// The places the last checked answer cited (#508).
    #[serde(default)]
    pub sources_cited: u32,
    /// Cited places that did not hold, over every answer checked.
    #[serde(default)]
    pub sources_wrong: u32,
    /// The job stopped because its answer had no verifiable sources.
    #[serde(default)]
    pub held_for_sources: bool,
    /// In eval runs: whether the fix's own tests pass on the agent's patch.
    /// `None` when they did not run (no eval, or no gated patch).
    #[serde(default)]
    pub hidden_tests_passed: Option<bool>,
    /// The issue's title and URL, once the run read the issue (#316).
    #[serde(default)]
    pub issue_title: Option<String>,
    #[serde(default)]
    pub issue_url: Option<String>,
    /// The agent's last final answer.
    #[serde(default)]
    pub answer: Option<AnswerSummary>,
    /// A shadow job's answer and base (#699), to open its PR from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_kept: Option<ShadowKept>,
    /// Embedded instructions the platform found in what the job read
    /// (#444).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_flags: Vec<InputFlag>,
    /// What Light read the ticket as needing (#497); recorded only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<Triage>,
    /// Intake metadata dimensions used for bias analysis (#511).
    #[serde(default, skip_serializing_if = "TicketMeta::is_empty")]
    pub ticket_meta: TicketMeta,
}

impl IssueRecord {
    /// This record with a review round's (#245) work added: its cost,
    /// calls, tokens and hidden secrets are the PR's. What the PR is
    /// (outcome, review, gate runs) stays this record's.
    pub fn with_round(&self, round: &IssueRecord) -> Self {
        let mut tokens = self.tokens.clone();
        for (model, counts) in &round.tokens {
            let sum = tokens.get(model).copied().unwrap_or_default().add(*counts);
            tokens.insert(model.clone(), sum);
        }
        Self {
            model_calls: self.model_calls + round.model_calls,
            cost_usd: self.cost_usd + round.cost_usd,
            secrets_redacted: self.secrets_redacted + round.secrets_redacted,
            secrets_blocked: self.secrets_blocked + round.secrets_blocked,
            tokens,
            ..self.clone()
        }
    }

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
            secrets_blocked: 0,
            sources_cited: 0,
            sources_wrong: 0,
            held_for_sources: false,
            hidden_tests_passed: None,
            issue_title: None,
            issue_url: None,
            answer: None,
            shadow_kept: None,
            input_flags: Vec::new(),
            triage: None,
            ticket_meta: TicketMeta::default(),
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
            LedgerEvent::Triaged { triage } => self.triage = Some(triage.clone()),
            LedgerEvent::Diagnosed { .. } | LedgerEvent::DeprecationsTaken { .. } => {}
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
            LedgerEvent::ToolCalls { .. }
            | LedgerEvent::TestReminded { .. }
            | LedgerEvent::ImpactChecked { .. }
            | LedgerEvent::Escalated { .. } => {}
            LedgerEvent::HiddenTests { passed, .. } => self.hidden_tests_passed = Some(*passed),
            LedgerEvent::SecretsRedacted { count } => {
                self.secrets_redacted = self.secrets_redacted.saturating_add(*count);
            }
            LedgerEvent::SourcesChecked { cited, wrong, held } => {
                self.sources_cited = *cited;
                self.sources_wrong = self.sources_wrong.saturating_add(*wrong);
                self.held_for_sources |= *held;
            }
            LedgerEvent::SecretsBlocked { count, .. } => {
                self.secrets_blocked = self.secrets_blocked.saturating_add(*count);
            }
            LedgerEvent::Finished {
                outcome,
                wall_time_s,
            } => {
                // A plan waiting for approval may be revised (#677): the
                // run goes on, and its time adds up.
                let before = match self.outcome {
                    Some(Outcome::AwaitingApproval) => self.wall_time_s.unwrap_or(0.0),
                    _ => 0.0,
                };
                self.outcome = Some(*outcome);
                self.wall_time_s = Some(before + *wall_time_s);
            }
            LedgerEvent::Reviewed(review) => self.review = Some(*review),
            LedgerEvent::IssueRead { title, url } => {
                self.issue_title = Some(title.clone());
                self.issue_url = Some(url.clone());
            }
            LedgerEvent::Answered(answer) => self.answer = Some(answer.clone()),
            LedgerEvent::InputFlagged(flag) => self.input_flags.push(flag.clone()),
            LedgerEvent::TicketMeta(meta) => {
                self.ticket_meta = self.ticket_meta.merged_with(meta);
            }
            LedgerEvent::ShadowKept(kept) => self.shadow_kept = Some(kept.clone()),
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
    fn token_counts_follow_a_provider_filled_five_minute_split() {
        // Providers call Usage::attribute_writes_from_plan when the backend
        // omits the TTL split (#55); the ledger then prices at 5m, not 1h.
        let mut usage = Usage {
            cache_creation_input_tokens: 900,
            ..Usage::default()
        };
        usage.attribute_writes_from_plan(aegis_provider::CachePlan {
            static_ttl: aegis_provider::CacheTtl::FiveMinutes,
            issue_message: None,
        });
        let counts = TokenCounts::from_usage(&usage);
        assert_eq!((counts.cache_write_5m, counts.cache_write_1h), (900, 0));
    }

    #[test]
    fn a_review_rounds_work_is_added_to_its_prs_record() {
        let pr = IssueRecord::from_events(
            id("GH-1"),
            &[
                call(Tier::Core, "core", 1_000, 0.5),
                LedgerEvent::Finished {
                    outcome: Outcome::PrOpened,
                    wall_time_s: 60.0,
                },
            ],
        );
        let round = IssueRecord::from_events(
            id("GH-1-V1"),
            &[
                call(Tier::Core, "core", 300, 0.1),
                call(Tier::Expert, "expert", 200, 0.4),
                LedgerEvent::Finished {
                    outcome: Outcome::StoppedSteps,
                    wall_time_s: 30.0,
                },
            ],
        );
        let both = pr.with_round(&round);
        assert_eq!(both.issue_id, id("GH-1"));
        assert_eq!(
            both.outcome,
            Some(Outcome::PrOpened),
            "the PR's, not the round's"
        );
        assert_eq!(both.model_calls, 3);
        assert!((both.cost_usd - 1.0).abs() < 1e-12);
        assert_eq!(both.tokens["core"].input, 1_300);
        assert_eq!(both.tokens["expert"].input, 200);
        assert_eq!(both.wall_time_s, Some(60.0));
    }

    #[test]
    fn a_triage_is_kept_and_read_back_as_written() {
        let triage = Triage {
            category: TriageCategory::DataIssue,
            confidence: 0.7,
            reason: "A row violates the foreign key.".into(),
        };
        let event = LedgerEvent::Triaged {
            triage: triage.clone(),
        };
        let line = serde_json::to_string(&event).unwrap();
        assert!(
            line.contains(r#""event":"triaged""#) && line.contains(r#""category":"data_issue""#),
            "{line}"
        );
        assert_eq!(serde_json::from_str::<LedgerEvent>(&line).unwrap(), event);
        let record = IssueRecord::from_events(id("GH-3"), &[event]);
        assert_eq!(record.triage, Some(triage));
        // Records written before #497 have none.
        assert_eq!(IssueRecord::new(id("GH-4")).triage, None);
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
                sidecar_s: None,
            }),
            call(Tier::Core, "claude-sonnet-5-5", 2_000, 0.25),
            call(Tier::Expert, "claude-opus-5-5", 500, 1.0),
            LedgerEvent::GateRun(GateRun {
                passed: true,
                failed_step: None,
                duration_s: 80.0,
                sidecar_s: None,
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
                    broke_tests: false,
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
            sidecar_s: None,
        });
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event"], "gate_run");
        assert_eq!(value["failed_step"], "clippy");
        assert_eq!(serde_json::from_value::<LedgerEvent>(value).unwrap(), event);
    }

    #[test]
    fn ticket_language_detection_handles_en_ja_vi_and_mixed_code() {
        assert_eq!(
            detect_ticket_language("Null pointer on empty cart", "Calling add() panics."),
            TicketLanguage::En
        );
        assert_eq!(
            detect_ticket_language(
                "ログイン画面でタイムアウトが発生する",
                "本番環境のみで再現します。"
            ),
            TicketLanguage::Ja
        );
        assert_eq!(
            detect_ticket_language(
                "Lỗi tính tổng giỏ hàng khi mã giảm giá rỗng",
                "Khách hàng báo bị sai số tiền."
            ),
            TicketLanguage::Vi
        );
        // Single Western European accent or code snippets must stay English.
        assert_eq!(
            detect_ticket_language(
                "Author Sofía reported panic in parser",
                "```rust\nlet café = 1;\n```\nExpected Ok(())"
            ),
            TicketLanguage::En
        );
        // Substantial Cyrillic prose classifies as Other.
        assert_eq!(
            detect_ticket_language("Ошибка при обработке", "Сервер возвращает ошибку 500"),
            TicketLanguage::Other
        );
    }

    #[test]
    fn ticket_meta_folds_and_preserves_backwards_compatibility() {
        let mut r = IssueRecord::from_events(
            id("JIRA-1"),
            &[LedgerEvent::TicketMeta(TicketMeta {
                programming_language: Some("rust".into()),
                ticket_source: Some("jira".into()),
                ticket_language: Some(TicketLanguage::Ja),
                repo: Some("acme/app".into()),
            })],
        );
        // Partial subsequent event updates only specified fields.
        r = r.apply(&LedgerEvent::TicketMeta(TicketMeta {
            ticket_source: Some("jsm".into()),
            ..TicketMeta::default()
        }));
        assert_eq!(r.ticket_meta.programming_language.as_deref(), Some("rust"));
        assert_eq!(r.ticket_meta.ticket_source.as_deref(), Some("jsm"));
        assert_eq!(r.ticket_meta.ticket_language, Some(TicketLanguage::Ja));
        assert_eq!(r.ticket_meta.repo.as_deref(), Some("acme/app"));

        // Legacy IssueRecord JSON without `ticket_meta` still deserializes cleanly.
        let legacy = json!({
            "issue_id": "LEGACY-1",
            "kind": null,
            "outcome": null,
            "tiers_used": [],
            "model_calls": 0,
            "tokens": {},
            "cost_usd": 0.0,
            "gate_runs": [],
            "wall_time_s": null,
            "review": null
        });
        let decoded: IssueRecord = serde_json::from_value(legacy).unwrap();
        assert!(decoded.ticket_meta.is_empty());
    }

    #[test]
    fn a_plan_revised_after_waiting_adds_up_its_time() {
        let finished = |outcome, wall_time_s| LedgerEvent::Finished {
            outcome,
            wall_time_s,
        };
        let record = IssueRecord::from_events(
            id("GH-9"),
            &[
                finished(Outcome::AwaitingApproval, 2.0),
                finished(Outcome::AwaitingApproval, 3.0),
            ],
        );
        assert_eq!(
            (record.outcome, record.wall_time_s),
            (Some(Outcome::AwaitingApproval), Some(5.0))
        );
        // An ended run that is finished again keeps only the last time.
        let ended = IssueRecord::from_events(
            id("GH-9"),
            &[
                finished(Outcome::StoppedSteps, 2.0),
                finished(Outcome::PrOpened, 3.0),
            ],
        );
        assert_eq!(ended.wall_time_s, Some(3.0));
    }
}
