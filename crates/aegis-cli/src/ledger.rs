//! The ledger `run.ledger` picks (#22), and `aegis ledger import|export`
//! to move a JSON ledger into PostgreSQL and back. The database URL comes
//! from `AEGIS_DATABASE_URL`, never from the configuration file.
//!
//! An export writes the JSON layout `scripts/eval/tool_usage.py` reads.

use std::path::Path;

use aegis_ledger::{
    CopyReport, IssueId, IssueRecord, JsonFileLedger, LedgerError, LedgerEvent, LedgerStore,
    copy_ledger,
};
use aegis_lifecycle::{LedgerBackend, RunConfig};
use aegis_store::{PgLedger, Store};

pub const DATABASE_URL_ENV: &str = "AEGIS_DATABASE_URL";

/// One of the two stores, picked at run time without `dyn` (ADR 0003 §7).
#[derive(Debug, Clone)]
pub enum Ledger {
    Json(JsonFileLedger),
    Postgres(PgLedger),
}

impl LedgerStore for Ledger {
    async fn append(&self, issue: &IssueId, event: &LedgerEvent) -> Result<(), LedgerError> {
        match self {
            Self::Json(store) => store.append(issue, event).await,
            Self::Postgres(store) => store.append(issue, event).await,
        }
    }

    async fn events(&self, issue: &IssueId) -> Result<Vec<LedgerEvent>, LedgerError> {
        match self {
            Self::Json(store) => store.events(issue).await,
            Self::Postgres(store) => store.events(issue).await,
        }
    }

    async fn save_record(&self, record: &IssueRecord) -> Result<(), LedgerError> {
        match self {
            Self::Json(store) => store.save_record(record).await,
            Self::Postgres(store) => store.save_record(record).await,
        }
    }

    async fn issues(&self) -> Result<Vec<IssueId>, LedgerError> {
        match self {
            Self::Json(store) => store.issues().await,
            Self::Postgres(store) => store.issues().await,
        }
    }
}

/// The JSON ledger of the configuration, whichever backend it picks.
pub fn json_dir(config: &RunConfig) -> std::path::PathBuf {
    config.run.state_dir.join("ledger")
}

/// Connects (and migrates) the database named by `AEGIS_DATABASE_URL`.
async fn database() -> Result<Store, String> {
    let url = std::env::var(DATABASE_URL_ENV)
        .map_err(|_| format!("set {DATABASE_URL_ENV} to the PostgreSQL URL of the ledger"))?;
    Store::connect(&url)
        .await
        .map_err(|e| format!("{DATABASE_URL_ENV}: {e}"))
}

/// The configured ledger of real runs.
pub async fn open(config: &RunConfig) -> Result<Ledger, String> {
    match config.run.ledger {
        LedgerBackend::Json => Ok(Ledger::Json(JsonFileLedger::new(json_dir(config)))),
        LedgerBackend::Postgres => Ok(Ledger::Postgres(database().await?.ledger())),
    }
}

fn summary(verb: &str, report: &CopyReport) -> String {
    format!(
        "{verb} {} issues, {} already there",
        report.copied.len(),
        report.unchanged.len()
    )
}

/// `aegis ledger import`: the JSON ledger at `from` into the database.
pub async fn import(from: &Path) -> Result<String, String> {
    if !from.is_dir() {
        return Err(format!("no ledger directory {}", from.display()));
    }
    let store = database().await?;
    let report = copy_ledger(&JsonFileLedger::new(from), &store.ledger())
        .await
        .map_err(|e| e.to_string())?;
    Ok(summary("imported", &report))
}

/// `aegis ledger export`: the database's ledger as JSON files under `to`.
pub async fn export(to: &Path) -> Result<String, String> {
    let store = database().await?;
    let report = copy_ledger(&store.ledger(), &JsonFileLedger::new(to))
        .await
        .map_err(|e| e.to_string())?;
    Ok(summary("exported", &report))
}
