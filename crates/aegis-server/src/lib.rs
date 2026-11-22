//! The platform as a service (ADR 0003 §1, #23): one binary whose role
//! (`api`, `worker` or `all`) picks what runs. The API takes jobs and
//! answers connectors and the dashboard; workers take jobs from the queue
//! in PostgreSQL and run each issue's lifecycle in its own worktree and
//! gate containers.

pub mod api;

/// The page's text for `en`, in the request's language (#590); what
/// templates call as `crate::t("…")`.
pub fn t(en: &'static str) -> &'static str {
    web::i18n::t(en)
}

pub mod auth;
pub mod cache_sweeper;
pub mod chatops;
pub mod config;
pub mod github_app;
pub mod lifecycle;
pub mod locale;
pub mod logs;
pub mod memory;
pub mod metrics;
pub mod pause_all;
pub mod repo_cli;
pub(crate) mod repo_issue;
pub mod runner;
pub mod secrets;
pub mod sonar;
pub mod tenant_cli;
pub mod tracker;
#[cfg(unix)]
pub mod umask;
pub mod web;
pub mod webhook;
pub mod worker;

/// What a process runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Api,
    Worker,
    All,
}

impl Role {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "api" => Some(Self::Api),
            "worker" => Some(Self::Worker),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub fn serves_api(self) -> bool {
        matches!(self, Self::Api | Self::All)
    }

    pub fn runs_jobs(self) -> bool {
        matches!(self, Self::Worker | Self::All)
    }
}
