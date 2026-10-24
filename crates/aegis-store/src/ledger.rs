//! The ledger in Postgres: one `ledger_events` row per event, holding the
//! event as the same JSON as a line of `<key>.events.jsonl`.

use aegis_ledger::{IssueId, IssueRecord, LedgerError, LedgerEvent, LedgerStore};
use sqlx::PgPool;

use crate::{Store, StoreError};

/// Rows read per query; the trait returns everything, the queries do not.
const READ_PAGE: i64 = 1_000;

#[derive(Debug, Clone)]
pub struct PgLedger {
    pool: PgPool,
}

fn store_error(error: sqlx::Error) -> LedgerError {
    LedgerError::Store(error.to_string())
}

fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, LedgerError> {
    serde_json::to_value(value)
        .map(without_nul)
        .map_err(|e| LedgerError::Encode(e.to_string()))
}

/// jsonb refuses NUL in a string, and a tool target or a router reason can
/// hold one. Losing the event would lose its cost, so NUL becomes U+FFFD.
fn without_nul(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) if s.contains('\0') => Value::String(s.replace('\0', "\u{fffd}")),
        Value::Array(items) => Value::Array(items.into_iter().map(without_nul).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k.replace('\0', "\u{fffd}"), without_nul(v)))
                .collect(),
        ),
        other => other,
    }
}

impl PgLedger {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl LedgerStore for PgLedger {
    async fn append(&self, issue: &IssueId, event: &LedgerEvent) -> Result<(), LedgerError> {
        event.ensure_finite()?;
        sqlx::query!(
            "INSERT INTO ledger_events (ledger_key, event) VALUES ($1, $2)",
            issue.as_str(),
            encode(event)?,
        )
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn events(&self, issue: &IssueId) -> Result<Vec<LedgerEvent>, LedgerError> {
        let mut events = Vec::new();
        let mut after = 0_i64;
        loop {
            let rows = sqlx::query!(
                "SELECT id, event FROM ledger_events
                 WHERE ledger_key = $1 AND id > $2 ORDER BY id LIMIT $3",
                issue.as_str(),
                after,
                READ_PAGE,
            )
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?;
            let full = rows.len() as i64 == READ_PAGE;
            for row in rows {
                after = row.id;
                let event = serde_json::from_value(row.event).map_err(|e| {
                    LedgerError::Store(format!("{issue}: event row {}: {e}", row.id))
                })?;
                events.push(event);
            }
            if !full {
                return Ok(events);
            }
        }
    }

    async fn save_record(&self, record: &IssueRecord) -> Result<(), LedgerError> {
        sqlx::query!(
            "INSERT INTO issue_records (ledger_key, record) VALUES ($1, $2)
             ON CONFLICT (ledger_key)
             DO UPDATE SET record = EXCLUDED.record, updated_at = now()",
            record.issue_id.as_str(),
            encode(record)?,
        )
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    /// In byte order, as `IssueId`'s `Ord`: the key column is `COLLATE "C"`.
    async fn issues(&self) -> Result<Vec<IssueId>, LedgerError> {
        let mut ids = Vec::new();
        let mut after = String::new();
        loop {
            let keys = sqlx::query_scalar!(
                "SELECT DISTINCT ledger_key FROM ledger_events
                 WHERE ledger_key > $1 ORDER BY ledger_key LIMIT $2",
                after,
                READ_PAGE,
            )
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?;
            let full = keys.len() as i64 == READ_PAGE;
            for key in keys {
                ids.push(IssueId::parse(key.as_str())?);
                after = key;
            }
            if !full {
                return Ok(ids);
            }
        }
    }
}

impl Store {
    /// The summary record last saved for `issue`.
    pub async fn record(&self, issue: &IssueId) -> Result<Option<IssueRecord>, StoreError> {
        let row = sqlx::query_scalar!(
            "SELECT record FROM issue_records WHERE ledger_key = $1",
            issue.as_str(),
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|json| {
            serde_json::from_value(json).map_err(|e| StoreError::Decode {
                column: "issue_records.record",
                value: e.to_string(),
            })
        })
        .transpose()
    }
}
