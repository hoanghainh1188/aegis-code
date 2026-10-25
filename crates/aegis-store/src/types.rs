//! Rows and the text values their CHECK constraints allow.

use aegis_ledger::{IssueId, Outcome};
use chrono::{DateTime, Utc};

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

text_enum! {
    /// `customer-jp` keeps the repo's data in Japan (Proposal §7).
    RepoMode { Poc = "poc", CustomerJp = "customer-jp" }
}

text_enum! {
    /// Where the job came from.
    JobSource {
        Cli = "cli",
        Github = "github",
        Gitlab = "gitlab",
        Jira = "jira",
        Backlog = "backlog",
        Sonar = "sonar",
        Ci = "ci",
    }
}

text_enum! {
    /// Who sent a webhook delivery, or which tracker a polled ticket is
    /// from (#26).
    WebhookSource { Github = "github", Gitlab = "gitlab", Jira = "jira", Backlog = "backlog" }
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
    }
}

pub(crate) fn decode_outcome(text: &str) -> Result<Outcome, StoreError> {
    [
        Outcome::PrOpened,
        Outcome::StoppedBudget,
        Outcome::StoppedSteps,
        Outcome::StoppedExpertFail,
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
    pub mode: RepoMode,
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
    pub mode: RepoMode,
    pub max_concurrent_jobs: u32,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewJob {
    pub repo_id: RepoId,
    /// The issue in its tracker: `owner/repo#12`, a Jira key, …
    pub issue_ref: String,
    /// The job's ledger, unique across repos.
    pub ledger_key: IssueId,
    pub source: JobSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub repo_id: RepoId,
    pub issue_ref: String,
    pub ledger_key: IssueId,
    pub source: JobSource,
    pub status: JobStatus,
    /// Set exactly when `status` is `Done`.
    pub outcome: Option<Outcome>,
    /// Runs started so far.
    pub attempts: u32,
    pub locked_by: Option<String>,
    pub locked_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Not claimed before this (backoff after an infrastructure failure).
    pub run_after: DateTime<Utc>,
    /// Infrastructure failures and expired leases so far.
    pub retries: u32,
    pub cancel_requested: bool,
    /// Why it ended: the agent's stop reason, the failure, the cancel.
    pub reason: Option<String>,
    pub pr_url: Option<String>,
    /// The job this one runs again.
    pub retry_of: Option<JobId>,
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
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
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
