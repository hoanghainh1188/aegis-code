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
pub async fn database() -> Result<Store, String> {
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
/// Only reads `from`: it may be the live ledger of real runs.
pub async fn import(store: &Store, from: &Path) -> Result<String, String> {
    if !from.is_dir() {
        return Err(format!("no ledger directory {}", from.display()));
    }
    let report = copy_ledger(&JsonFileLedger::new(from), &store.ledger())
        .await
        .map_err(|e| e.to_string())?;
    Ok(summary("imported", &report))
}

/// `aegis ledger export`: the database's ledger as JSON files under `to`.
pub async fn export(store: &Store, to: &Path) -> Result<String, String> {
    let report = copy_ledger(&store.ledger(), &JsonFileLedger::new(to))
        .await
        .map_err(|e| e.to_string())?;
    Ok(summary("exported", &report))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    use aegis_ledger::Outcome;

    use super::*;

    /// Every file's bytes and modification time, and the directory's.
    fn snapshot(dir: &Path) -> (SystemTime, BTreeMap<String, (Vec<u8>, SystemTime)>) {
        let files = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let modified = e.metadata().unwrap().modified().unwrap();
                let bytes = std::fs::read(e.path()).unwrap();
                (e.file_name().into_string().unwrap(), (bytes, modified))
            })
            .collect();
        (std::fs::metadata(dir).unwrap().modified().unwrap(), files)
    }

    #[tokio::test]
    async fn import_never_writes_to_the_ledger_it_reads() {
        let Some(db) = aegis_store::testing::test_db().await else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("aegis-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source = JsonFileLedger::new(&dir);
        let issue = IssueId::parse("scrapy-7604-r1").unwrap();
        let finished = LedgerEvent::Finished {
            outcome: Outcome::PrOpened,
            wall_time_s: 3.0,
        };
        source.append(&issue, &finished).await.unwrap();
        // A run still going: events, no record. The import must not add one.
        let running = IssueId::parse("typst-1-r1").unwrap();
        source
            .append(&running, &LedgerEvent::SecretsRedacted { count: 0 })
            .await
            .unwrap();
        source
            .save_record(&IssueRecord::from_events(issue.clone(), [&finished]))
            .await
            .unwrap();
        let before = snapshot(&dir);

        let first = import(&db.store, &dir).await.unwrap();
        let again = import(&db.store, &dir).await.unwrap();
        assert_eq!(first, "imported 2 issues, 0 already there");
        assert_eq!(again, "imported 0 issues, 2 already there");
        assert_eq!(snapshot(&dir), before, "the source is untouched");
        assert_eq!(
            db.store.ledger().events(&issue).await.unwrap(),
            vec![finished]
        );
        std::fs::remove_dir_all(&dir).unwrap();
        db.drop().await;
    }
}
