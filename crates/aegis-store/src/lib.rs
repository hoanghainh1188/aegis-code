//! Durable platform state in PostgreSQL (ADR 0003 §2, #22): repos, jobs,
//! their runs, the job queue, webhook deliveries, and the ledger behind
//! [`aegis_ledger::LedgerStore`]; users, sessions, roles and the audit log
//! (#29); repos' monthly budgets (#31); the records behind the dashboard's
//! KPIs (#170); the Sonar warnings each polled batch took (#27).
//!
//! Queries are checked against the schema at compile time; `.sqlx/` at the
//! workspace root lets the crate build without a database. Every list is
//! paged ([`Page`]).

mod audit;
mod budget;
mod deliveries;
mod error;
mod jobs;
mod kpi;
mod ledger;
mod page;
mod polls;
mod queue;
mod repos;
mod reviews;
mod sonar;
mod stats;
mod tokens;
mod types;
mod users;

#[cfg(feature = "testing")]
pub mod testing;

use std::str::FromStr;
use std::sync::Arc;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub use audit::{Actor, AuditEntry};
pub use budget::{BUDGET_SPENT, BudgetRules, BudgetState, RepoBudget};
pub use error::StoreError;
pub use kpi::{KpiJob, KpiRecord};
pub use ledger::PgLedger;
pub use page::{Page, PageOf};
pub use queue::{AttemptEnd, Claim, Lease, MAX_PATCH_BYTES, Patch};
pub use reviews::{DEFAULT_MAX_REVIEW_ROUNDS, pr_issue_ref};
pub use sonar::MAX_BATCH_WARNINGS;
pub use stats::QueueStats;
pub use tokens::{MAX_PERSONAL_TOKENS, MAX_TOKEN_DAYS, PersonalToken};
pub use types::{
    ActorKind, Delivered, Job, JobEnd, JobFilter, JobId, JobSource, JobStatus, NewDelivery, NewJob,
    NewRepo, Repo, RepoId, RepoMode, RepoRole, Run, RunEnd, RunId, RunStatus, User, UserId,
    UserKind, WebhookSource,
};
pub use users::{PendingLogin, RepoMember, Session, TokenHash};

/// The schema's migrations, expand/contract (`docs/runbook`): an older
/// binary must start on a schema a newer one expanded, as a rollback does
/// (#176). So migrations applied but unknown to this binary are let be;
/// one it knows whose checksum changed still stops it.
fn migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!();
    migrator.set_ignore_missing(true);
    migrator
}

const MAX_CONNECTIONS: u32 = 10;

#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
    /// How monthly budgets apply (#31); [`BudgetRules::default`] until
    /// [`Store::with_budget`].
    budget: Arc<BudgetRules>,
    /// Review rounds a PR may have (#243).
    max_review_rounds: u32,
}

impl Store {
    /// Connects and brings the schema up to date.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let options = PgConnectOptions::from_str(url)?;
        Self::connect_with(options).await
    }

    pub async fn connect_with(options: PgConnectOptions) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .connect_with(options)
            .await?;
        migrator().run(&pool).await?;
        Ok(Self {
            pool,
            budget: Arc::default(),
            max_review_rounds: reviews::DEFAULT_MAX_REVIEW_ROUNDS,
        })
    }

    /// New connections use `url` (a rotated password, #30); open ones
    /// keep theirs until the pool replaces them.
    pub fn set_url(&self, url: &str) -> Result<(), StoreError> {
        self.pool
            .set_connect_options(PgConnectOptions::from_str(url)?);
        Ok(())
    }

    /// The ledger in this database.
    pub fn ledger(&self) -> PgLedger {
        PgLedger::new(self.pool.clone())
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}
