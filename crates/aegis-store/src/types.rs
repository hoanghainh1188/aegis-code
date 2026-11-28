//! Rows and the text values their CHECK constraints allow.

use std::borrow::Cow;

use aegis_ledger::{IssueId, Outcome};
use time::OffsetDateTime;

use crate::StoreError;

/// A Rust enum for a text column with a CHECK list of the same values.
macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// The value spelled `text`, as the column holds it.
            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }

            #[allow(dead_code, reason = "not every enum is read back")]
            pub(crate) fn decode(column: &'static str, text: &str) -> Result<Self, StoreError> {
                match text {
                    $($text => Ok(Self::$variant),)+
                    other => Err(StoreError::Decode {
                        column,
                        value: other.to_owned(),
                    }),
                }
            }
        }
    };
}

/// Longest mode name the `repos.mode` CHECK allows.
const MAX_MODE_LEN: usize = 64;

/// A repo's mode (#591): which mode of the server's `[modes]` its jobs
/// run with (#592). Any name the `repos.mode` CHECK allows: 1–64 of `a-z`,
/// `0-9`, `_` and `-`, starting with a letter or a digit. Whether the
/// server has a configuration for it is checked where the repo is
/// registered and where its jobs run, not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModeName(Cow<'static, str>);

impl ModeName {
    /// The default mode.
    pub const POC: Self = Self(Cow::Borrowed("poc"));
    /// Keeps the repo's data in Japan (Proposal §7).
    pub const CUSTOMER_JP: Self = Self(Cow::Borrowed("customer-jp"));

    /// `text` as a mode name, if the `repos.mode` CHECK allows it.
    pub fn parse(text: &str) -> Result<Self, StoreError> {
        if is_mode_name(text) {
            Ok(Self(Cow::Owned(text.to_owned())))
        } else {
            Err(StoreError::Invalid {
                field: "mode",
                message: format!(
                    "`{text}`: 1-{MAX_MODE_LEN} of a-z, 0-9, `_` and `-`, starting with a letter or a digit"
                ),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name with each `_` as `-`: names with the same key are one
    /// mode, so `customer_jp` in an older server.toml is `customer-jp`
    /// (#592).
    pub fn key(&self) -> String {
        self.0.replace('_', "-")
    }

    pub(crate) fn decode(column: &'static str, text: &str) -> Result<Self, StoreError> {
        Self::parse(text).map_err(|_| StoreError::Decode {
            column,
            value: text.to_owned(),
        })
    }
}

impl std::fmt::Display for ModeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `repos.mode` CHECK: `^[a-z0-9][a-z0-9_-]{0,63}$`.
fn is_mode_name(text: &str) -> bool {
    let lower_or_digit = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    text.len() <= MAX_MODE_LEN
        && text.bytes().next().is_some_and(lower_or_digit)
        && text
            .bytes()
            .all(|b| lower_or_digit(b) || matches!(b, b'_' | b'-'))
}

text_enum! {
    /// Where the job came from. `cve`: a dependency upgrade for an
    /// advisory (#502), its target in `issue_ref` as
    /// `cve:<advisory> <package> <version>`. `apm`: an error spike an APM
    /// source showed before any ticket (#706), diagnosed, never fixed.
    /// `deprecation`: the repo's deprecation warnings, fixed as a batch on
    /// its schedule (#705).
    JobSource {
        Cli = "cli",
        Github = "github",
        Gitlab = "gitlab",
        Jira = "jira",
        Backlog = "backlog",
        Jsm = "jsm",
        Servicenow = "servicenow",
        Sonar = "sonar",
        Ci = "ci",
        Review = "review",
        Cve = "cve",
        Apm = "apm",
        Deprecation = "deprecation",
    }
}

text_enum! {
    /// Who sent a webhook delivery, or which tracker a polled ticket is
    /// from (#26, #453), or Bitbucket Cloud (#634) or Data Center (#635), or Azure DevOps (#636).
    WebhookSource {
        Github = "github",
        Gitlab = "gitlab",
        Jira = "jira",
        Backlog = "backlog",
        Jsm = "jsm",
        Servicenow = "servicenow",
        Bitbucket = "bitbucket",
        BitbucketDc = "bitbucket-dc",
        AzureDevops = "azure-devops",
    }
}

text_enum! {
    /// Severity / SLA priority mapped from ITSM tickets (#453).
    SlaPriority {
        P1 = "P1",
        P2 = "P2",
        P3 = "P3",
    }
}

text_enum! {
    /// What became of a webhook delivery (#331). `recorded`: no job, but a
    /// merged agent PR's memory was recorded (#456). `installation`: the
    /// GitHub App's installation changed (#540).
    WebhookOutcome {
        Queued = "queued",
        AlreadyLive = "already_live",
        Refused = "refused",
        Duplicate = "duplicate",
        Ignored = "ignored",
        Ping = "ping",
        Malformed = "malformed",
        BadSignature = "bad_signature",
        Recorded = "recorded",
        Installation = "installation",
    }
}

text_enum! {
    /// `failed` is an infrastructure failure after its retries; what the
    /// agent achieved is `done` with an [`Outcome`].
    JobStatus {
        Queued = "queued",
        Running = "running",
        Done = "done",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

text_enum! {
    RunStatus {
        Running = "running",
        Finished = "finished",
        Interrupted = "interrupted",
        Failed = "failed",
    }
}

pub(crate) fn outcome_str(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::PrOpened => "pr_opened",
        Outcome::StoppedBudget => "stopped_budget",
        Outcome::StoppedSteps => "stopped_steps",
        Outcome::StoppedExpertFail => "stopped_expert_fail",
        Outcome::Diagnosed => "diagnosed",
        Outcome::AwaitingApproval => "awaiting_approval",
        Outcome::Integrated => "integrated",
        Outcome::Shadowed => "shadowed",
    }
}

pub(crate) fn decode_outcome(text: &str) -> Result<Outcome, StoreError> {
    [
        Outcome::PrOpened,
        Outcome::StoppedBudget,
        Outcome::StoppedSteps,
        Outcome::StoppedExpertFail,
        Outcome::Diagnosed,
        Outcome::AwaitingApproval,
        Outcome::Integrated,
        Outcome::Shadowed,
    ]
    .into_iter()
    .find(|o| outcome_str(*o) == text)
    .ok_or_else(|| StoreError::Decode {
        column: "jobs.outcome",
        value: text.to_owned(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(pub i64);

#[derive(Debug, Clone, PartialEq)]
pub struct NewRepo {
    /// `[A-Za-z0-9][A-Za-z0-9._-]*`, at most 128 bytes.
    pub slug: String,
    pub url: String,
    pub default_branch: String,
    pub language: String,
    pub mode: ModeName,
    /// Jobs of this repo that may run at the same time, at least 1.
    pub max_concurrent_jobs: u32,
    /// The rest of the repo's settings, as `aegis-lifecycle` reads them.
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Repo {
    pub id: RepoId,
    pub slug: String,
    pub url: String,
    pub default_branch: String,
    pub language: String,
    pub mode: ModeName,
    pub max_concurrent_jobs: u32,
    pub config: serde_json::Value,
    pub created_at: OffsetDateTime,
    /// The contract it belongs to (#525).
    pub tenant_id: crate::TenantId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewJob {
    pub repo_id: RepoId,
    /// The issue in its tracker: `owner/repo#12`, a Jira key, …
    pub issue_ref: String,
    /// The job's ledger, unique across repos.
    pub ledger_key: IssueId,
    pub source: JobSource,
    /// SLA priority (`P1`–`P3`) mapped from an ITSM ticket (#453), if any.
    pub priority: Option<SlaPriority>,
    /// SLA due timestamp from an ITSM ticket (#453), if any.
    pub sla_deadline: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub repo_id: RepoId,
    pub issue_ref: String,
    pub ledger_key: IssueId,
    pub source: JobSource,
    /// SLA priority (`P1`–`P3`) mapped from an ITSM ticket (#453), if any.
    pub priority: Option<SlaPriority>,
    /// SLA due timestamp from an ITSM ticket (#453), if any.
    pub sla_deadline: Option<OffsetDateTime>,
    pub status: JobStatus,
    /// Set exactly when `status` is `Done`.
    pub outcome: Option<Outcome>,
    /// Runs started so far.
    pub attempts: u32,
    pub locked_by: Option<String>,
    pub locked_until: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Not claimed before this (backoff after an infrastructure failure).
    pub run_after: OffsetDateTime,
    /// Infrastructure failures and expired leases so far.
    pub retries: u32,
    pub cancel_requested: bool,
    /// Why it ended: the agent's stop reason, the failure, the cancel.
    pub reason: Option<String>,
    pub pr_url: Option<String>,
    /// The job this one runs again.
    pub retry_of: Option<JobId>,
    /// A review round (#243): the job that opened the PR it works on.
    pub review_of: Option<JobId>,
    /// The PR's review round this is, from 1; 0 when not one.
    pub review_round: u32,
    /// Queued while its repo was in shadow mode (#699): it pushes no
    /// branch, opens no PR and writes nothing on the ticket.
    pub shadow: bool,
    /// The shadow job whose PR this one opens from its patch (#699).
    pub publish_of: Option<JobId>,
}

/// A webhook delivery that asks for a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDelivery {
    pub source: WebhookSource,
    /// `X-GitHub-Delivery`, `X-Gitlab-Event-UUID`: 1 to 128 bytes.
    pub delivery_id: String,
    pub event: String,
}

/// What became of a delivery's job.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivered {
    Queued(Box<Job>),
    /// The delivery was received before: nothing done.
    Duplicate,
    /// The issue already has a live job; no second one.
    AlreadyLive(JobId),
    /// The repo's monthly budget is spent (#31): the job is recorded
    /// cancelled, with the reason, so the issue can be told.
    Refused(Box<Job>),
}

/// How a job ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEnd {
    Done(Outcome),
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobFilter {
    pub repo_id: Option<RepoId>,
    pub status: Option<JobStatus>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: RunId,
    pub job_id: JobId,
    /// 1 for the first run of the job.
    pub attempt: u32,
    pub status: RunStatus,
    pub error: Option<String>,
    pub started_at: OffsetDateTime,
    pub ended_at: Option<OffsetDateTime>,
}

/// How a run ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    Finished,
    /// Cut short by a restart or shutdown (ADR 0003 §3).
    Interrupted,
    /// An infrastructure error, retried with a new run.
    Failed(String),
}

impl RunEnd {
    pub(crate) fn status(&self) -> RunStatus {
        match self {
            Self::Finished => RunStatus::Finished,
            Self::Interrupted => RunStatus::Interrupted,
            Self::Failed(_) => RunStatus::Failed,
        }
    }

    pub(crate) fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Finished | Self::Interrupted => None,
        }
    }
}

/// A count column (`attempt`, `attempts`), never negative by its CHECK.
pub(crate) fn count(column: &'static str, value: i32) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| StoreError::Decode {
        column,
        value: value.to_string(),
    })
}

text_enum! {
    /// How an actor got in: a dashboard session, an API token, the command
    /// line on the server, a signed webhook, the platform itself.
    ActorKind {
        User = "user",
        Token = "token",
        Cli = "cli",
        Webhook = "webhook",
        System = "system",
    }
}

text_enum! {
    /// A person signs in through the SSO; a service uses API tokens.
    UserKind { Person = "person", Service = "service" }
}

text_enum! {
    /// What a user may do in one repo; each role includes those below it.
    RepoRole {
        Viewer = "viewer",
        Reviewer = "reviewer",
        Maintainer = "maintainer",
    }
}

impl RepoRole {
    fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Reviewer => 1,
            Self::Maintainer => 2,
        }
    }

    /// Whether this role allows what `needed` allows.
    pub fn includes(self, needed: Self) -> bool {
        self.rank() >= needed.rank()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub kind: UserKind,
    /// A person's e-mail or a service's name, lower case.
    pub name: String,
    pub is_admin: bool,
}

#[cfg(test)]
mod mode_name_tests {
    use super::*;

    #[test]
    fn mode_names_follow_the_repos_mode_check() {
        let longest = "a".repeat(MAX_MODE_LEN);
        for ok in [
            "poc",
            "customer-jp",
            "customer-eu",
            "local_airgap",
            "9",
            &longest,
        ] {
            assert_eq!(ModeName::parse(ok).unwrap().as_str(), ok);
        }
        let too_long = "a".repeat(MAX_MODE_LEN + 1);
        for bad in [
            "",
            "-poc",
            "_poc",
            "Customer-JP",
            "customer.eu",
            "café",
            " poc",
            &too_long,
        ] {
            assert!(
                matches!(
                    ModeName::parse(bad),
                    Err(StoreError::Invalid { field: "mode", .. })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn parsed_names_equal_the_constants() {
        assert_eq!(ModeName::parse("poc").unwrap(), ModeName::POC);
        assert_eq!(
            ModeName::parse("customer-jp").unwrap(),
            ModeName::CUSTOMER_JP
        );
        assert_eq!(ModeName::CUSTOMER_JP.to_string(), "customer-jp");
        let decoded = ModeName::decode("repos.mode", "Bad!");
        assert!(matches!(
            decoded,
            Err(StoreError::Decode {
                column: "repos.mode",
                ..
            })
        ));
    }
}
