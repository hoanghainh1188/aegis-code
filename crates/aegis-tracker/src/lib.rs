//! Jira and Backlog as issue trackers (#26, Proposal §1): the tickets a
//! filter selects (JQL for Jira, project, status, category and keyword
//! for Backlog), each with its description and the text logs attached to
//! it; and, when a job ends, a comment on the ticket, the PR linked to it
//! and, if configured, the ticket moved on.
//!
//! - Jira Cloud by default (REST API v2, so descriptions are text, not
//!   ADF; search through `/search/jql`), with `user` (the account's
//!   e-mail) and an API token; Data Center or Server with
//!   `deployment = "data-center"` and a personal access token.
//! - Backlog (Nulab) API v2 with an API key, sent as `apiKey` the way the
//!   API asks; it never appears in an error.
//!
//! A ticket's text is untrusted (design §7), like a GitHub issue's.
//! Tokens are not in the configuration: `token_env` names the secret, the
//! caller reads it from its secret store.

mod backlog;
mod config;
mod error;
mod http;
mod jira;
mod ticket;

pub use config::{BacklogConfig, Deployment, JiraConfig, TrackerConfig};
pub use error::TrackerError;
pub use ticket::{Log, Ticket, checked_key};

use backlog::Backlog;
use jira::Jira;

/// The most tickets one search returns: a filter that selects more is too
/// wide, and the next poll takes the rest once these have jobs.
pub const MAX_TICKETS: usize = 500;

/// How a job ended, for its ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report<'a> {
    /// Plain text; the caller makes untrusted parts inert.
    pub comment: &'a str,
    /// The PR opened, when there is one: linked to the ticket, and the
    /// ticket moved by `pr_transition` / `pr_status_id`.
    pub pr_url: Option<&'a str>,
}

/// A tracker, connected with its token.
#[derive(Debug, Clone)]
pub enum Tracker {
    Jira(Jira),
    Backlog(Backlog),
}

impl Tracker {
    pub fn new(config: &TrackerConfig, token: &str) -> Result<Self, TrackerError> {
        config.check()?;
        Ok(match config {
            TrackerConfig::Jira(c) => Self::Jira(Jira::new(c, token)?),
            TrackerConfig::Backlog(c) => Self::Backlog(Backlog::new(c, token)?),
        })
    }

    /// The keys of the tickets the filter selects, at most
    /// [`MAX_TICKETS`]; keys that are not `PROJ-12` are left out.
    pub async fn search(&self) -> Result<Vec<String>, TrackerError> {
        match self {
            Self::Jira(j) => j.search().await,
            Self::Backlog(b) => b.search().await,
        }
    }

    /// Ticket `key` with its text logs.
    pub async fn ticket(&self, key: &str) -> Result<Ticket, TrackerError> {
        let key = checked_key(key)?;
        match self {
            Self::Jira(j) => j.ticket(key).await,
            Self::Backlog(b) => b.ticket(key).await,
        }
    }

    /// Writes how the job ended on ticket `key`.
    pub async fn report(&self, key: &str, report: &Report<'_>) -> Result<(), TrackerError> {
        let key = checked_key(key)?;
        match self {
            Self::Jira(j) => j.report(key, report).await,
            Self::Backlog(b) => b.report(key, report).await,
        }
    }
}
