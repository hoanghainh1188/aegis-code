//! Durable platform state in PostgreSQL (ADR 0003 §2, #22): repos, jobs,
//! their runs, the job queue, webhook deliveries, and the ledger behind
//! [`aegis_ledger::LedgerStore`].
//!
//! Queries are checked against the schema at compile time; `.sqlx/` at the
//! workspace root lets the crate build without a database. Every list is
//! paged ([`Page`]).

mod deliveries;
mod error;
mod jobs;
mod ledger;
mod page;
mod queue;
mod repos;
mod types;

#[cfg(feature = "testing")]
pub mod testing;

use std::str::FromStr;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub use error::StoreError;
pub use ledger::PgLedger;
pub use page::{Page, PageOf};
pub use queue::{AttemptEnd, Claim, Lease};
pub use types::{
    Delivered, Job, JobEnd, JobFilter, JobId, JobSource, JobStatus, NewDelivery, NewJob, NewRepo,
    Repo, RepoId, RepoMode, Run, RunEnd, RunId, RunStatus, WebhookSource,
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

const MAX_CONNECTIONS: u32 = 10;

#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
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
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
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
