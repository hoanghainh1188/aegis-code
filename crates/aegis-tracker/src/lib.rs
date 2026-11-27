//! Issue trackers, ITSM systems, and APM connectors (#26, #452, #453,
//! Proposal §1):
//!
//! - Jira Cloud / Data Center, Jira Service Management (`jsm`), ServiceNow
//!   (`servicenow`), and Backlog (Nulab) poll matching tickets with optional SLA
//!   priority (`P1`–`P3`) and deadlines, read ticket descriptions and log
//!   attachments, and write completion status / PR links back.
//! - Sentry, Datadog and CloudWatch Logs APM enrichment (`apm`) read stack traces and
//!   breadcrumbs for issues lacking logs (#452, #704).
//!
//! A ticket's text is untrusted (design §7), like a GitHub issue's.
//! Tokens are not in the configuration: `token_env` names the secret, the
//! caller reads it from its secret store.

mod apm;
mod backlog;
mod config;
mod error;
mod http;
mod jira;
mod jsm;
mod servicenow;
mod ticket;

pub use apm::{
    Anomaly, ApmClient, ApmConfig, CloudWatchConfig, DatadogConfig, ErrorCount, Release,
    SentryConfig, SpikeRules, Telemetry, TelemetryQuery, anomalies,
};
use backlog::Backlog;
pub use config::{
    BacklogConfig, Deployment, JiraConfig, JsmConfig, ServiceNowConfig, TrackerConfig,
};
pub use error::TrackerError;
use jira::Jira;
use jsm::Jsm;
use servicenow::ServiceNow;
pub use ticket::{Log, Priority, SearchTicket, Ticket, checked_key, checked_servicenow_key};

/// The most tickets one search returns: a filter that selects more is too
/// wide, and the next poll takes the rest once these have jobs.
pub const MAX_TICKETS: usize = 500;

/// How a job ended, for its ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report<'a> {
    /// Plain text; the caller makes untrusted parts inert.
    pub comment: &'a str,
    /// The PR opened, when there is one: linked to the ticket, and the
    /// ticket moved by `pr_transition` / `pr_status_id` / `pr_state`.
    pub pr_url: Option<&'a str>,
}

/// A tracker or ITSM system, connected with its token.
#[derive(Debug, Clone)]
pub enum Tracker {
    Jira(Jira),
    Backlog(Backlog),
    Jsm(Jsm),
    Servicenow(ServiceNow),
}

impl Tracker {
    pub fn new(config: &TrackerConfig, token: &str) -> Result<Self, TrackerError> {
        config.check()?;
        Ok(match config {
            TrackerConfig::Jira(c) => Self::Jira(Jira::new(c, token)?),
            TrackerConfig::Backlog(c) => Self::Backlog(Backlog::new(c, token)?),
            TrackerConfig::Jsm(c) => Self::Jsm(Jsm::new(c, token)?),
            TrackerConfig::Servicenow(c) => Self::Servicenow(ServiceNow::new(c, token)?),
        })
    }

    /// The keys of the tickets the filter selects, at most
    /// [`MAX_TICKETS`]; keys that do not match the tracker's key syntax are left out.
    pub async fn search(&self) -> Result<Vec<String>, TrackerError> {
        Ok(self
            .search_tickets()
            .await?
            .into_iter()
            .map(|t| t.key)
            .collect())
    }

    /// Search matching tickets along with any ITSM priority and SLA deadline (#453).
    pub async fn search_tickets(&self) -> Result<Vec<SearchTicket>, TrackerError> {
        match self {
            Self::Jira(j) => Ok(j
                .search()
                .await?
                .into_iter()
                .map(|key| SearchTicket {
                    key,
                    priority: None,
                    sla_deadline: None,
                })
                .collect()),
            Self::Backlog(b) => Ok(b
                .search()
                .await?
                .into_iter()
                .map(|key| SearchTicket {
                    key,
                    priority: None,
                    sla_deadline: None,
                })
                .collect()),
            Self::Jsm(j) => j.search_tickets().await,
            Self::Servicenow(s) => s.search_tickets().await,
        }
    }

    /// Ticket `key` with its text logs and ITSM SLA metadata (if any).
    pub async fn ticket(&self, key: &str) -> Result<Ticket, TrackerError> {
        match self {
            Self::Jira(j) => {
                let key = checked_key(key)?;
                j.ticket(key).await
            }
            Self::Backlog(b) => {
                let key = checked_key(key)?;
                b.ticket(key).await
            }
            Self::Jsm(j) => {
                let key = checked_key(key)?;
                j.ticket(key).await
            }
            Self::Servicenow(s) => {
                let key = checked_servicenow_key(key)?;
                s.ticket(key).await
            }
        }
    }

    /// Writes how the job ended on ticket `key`.
    pub async fn report(&self, key: &str, report: &Report<'_>) -> Result<(), TrackerError> {
        match self {
            Self::Jira(j) => {
                let key = checked_key(key)?;
                j.report(key, report).await
            }
            Self::Backlog(b) => {
                let key = checked_key(key)?;
                b.report(key, report).await
            }
            Self::Jsm(j) => {
                let key = checked_key(key)?;
                j.report(key, report).await
            }
            Self::Servicenow(s) => {
                let key = checked_servicenow_key(key)?;
                s.report(key, report).await
            }
        }
    }
}
