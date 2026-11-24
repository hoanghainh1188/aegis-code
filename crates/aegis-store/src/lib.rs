//! Durable platform state in PostgreSQL (ADR 0003 §2, #22): repos, jobs,
//! their runs, the job queue, webhook deliveries, and the ledger behind
//! [`aegis_ledger::LedgerStore`]; users, sessions, roles and the audit log
//! (#29); repos' monthly budgets (#31); the records behind the dashboard's
//! KPIs (#170); the Sonar warnings each polled batch took (#27).
//!
//! Queries are checked against the schema at compile time; `.sqlx/` at the
//! workspace root lets the crate build without a database. Every list is
//! paged ([`Page`]).

mod ams;
mod audit;
mod budget;
mod clones;
mod deliveries;
mod error;
mod installations;
mod instance_pause;
mod jobs;
mod kpi;
mod ledger;
mod memory;
mod page;
mod pause;
mod polls;
mod preflights;
mod queue;
mod repos;
mod reviews;
mod sonar;
mod stats;
mod tenants;
mod tokens;
mod tx;
mod types;
mod user_list;
mod users;
mod webhook_log;

#[cfg(feature = "testing")]
pub mod testing;

use std::str::FromStr;
use std::sync::Arc;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub use ams::{AmsReport, AmsRow};
pub use audit::{Actor, AuditEntry, AuditFilter};
pub use budget::{BUDGET_SPENT, BudgetRules, BudgetState, RepoBudget};
pub use clones::{CloneState, MAX_CLONE_LOG_BYTES, RepoClone};
pub use error::StoreError;
pub use installations::{
    AppOnRepo, Installation, InstallationSeen, RepoSelection, binding_refusal,
};
pub use instance_pause::{INSTANCE_PAUSED, InstancePause, InstancePaused, PauseLevel};
pub use kpi::{KpiJob, KpiRecord};
pub use ledger::PgLedger;
pub use memory::{
    EmbeddingSpace, MemoryId, MemoryKind, MemoryScope, MemoryToEmbed, NearQuery, NewRepoMemory,
    RecallScores, RepoMemory, fuse,
};
pub use page::{Page, PageOf};
pub use pause::{Paused, REPO_PAUSED, RepoPause};
pub use preflights::{
    MAX_PREFLIGHT_ITEMS, PREFLIGHT_STALE_AFTER, PreflightItem, PreflightState, RepoPreflight,
};
pub use queue::{AttemptEnd, Claim, Lease, MAX_DIAGNOSIS_CHARS, MAX_PATCH_BYTES, Patch};
pub use reviews::{DEFAULT_MAX_REVIEW_ROUNDS, pr_issue_ref};
pub use sonar::MAX_BATCH_WARNINGS;
pub use stats::{PRIORITY_LEVELS, PriorityStats, QueueStats};
pub use tenants::{LOCALES as TENANT_LOCALES, TENANT_BUDGET_SPENT, Tenant, TenantBudget, TenantId};
pub use tokens::{MAX_PERSONAL_TOKENS, MAX_TOKEN_DAYS, PersonalToken, ServiceToken};
pub use types::{
    ActorKind, Delivered, Job, JobEnd, JobFilter, JobId, JobSource, JobStatus, ModeName,
    NewDelivery, NewJob, NewRepo, Repo, RepoId, RepoRole, Run, RunEnd, RunId, RunStatus,
    SlaPriority, User, UserId, UserKind, WebhookOutcome, WebhookSource,
};
pub use user_list::UserSummary;
pub use users::{LiveSession, PendingLogin, RepoMember, Session, TokenHash};
pub use webhook_log::{BadSignatures, NewWebhookEvent, WebhookEvent};

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
    /// How long a queued job waits before it is claimed as if one SLA level
    /// higher (#524), so none waits for ever behind newer `P1`s.
    sla_aging: std::time::Duration,
    /// Which embedding dimensions pgvector searches (#655).
    vectors: Arc<memory::VectorIndexes>,
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
            sla_aging: queue::DEFAULT_SLA_AGING,
            vectors: Arc::default(),
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
