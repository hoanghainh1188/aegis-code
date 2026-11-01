//! The ledger in Postgres: one `ledger_events` row per event, holding the
//! event as the same JSON as a line of `<key>.events.jsonl`.

use aegis_ledger::{IssueId, IssueRecord, LedgerError, LedgerEvent, LedgerStore, Review};
use sqlx::PgPool;

use crate::audit::{self, Target};
use crate::tx;
use crate::{Actor, Job, RunId, Store, StoreError};

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

    /// [`LedgerStore::append`], with the event tied to the job's run that
    /// wrote it: what an interrupted attempt cost stays measurable.
    pub async fn append_in_run(
        &self,
        issue: &IssueId,
        event: &LedgerEvent,
        run: Option<RunId>,
    ) -> Result<(), LedgerError> {
        event.ensure_finite()?;
        sqlx::query!(
            "INSERT INTO ledger_events (ledger_key, event, run_id) VALUES ($1, $2, $3)",
            issue.as_str(),
            encode(event)?,
            run.map(|r| r.0),
        )
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}

impl LedgerStore for PgLedger {
    async fn append(&self, issue: &IssueId, event: &LedgerEvent) -> Result<(), LedgerError> {
        self.append_in_run(issue, event, None).await
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

    /// The reviewer's verdict on `job`'s PR, written as
    /// [`aegis_ledger::IssueLedger::reviewed`] writes it (the event, then
    /// the record it leads to) and audited, all in one transaction.
    /// `Conflict` when the job already has a review and `replace` is false.
    pub async fn record_review(
        &self,
        job: &Job,
        review: Review,
        replace: bool,
        actor: &Actor,
    ) -> Result<IssueRecord, StoreError> {
        let event = LedgerEvent::Reviewed(review);
        event.ensure_finite()?;
        let key = &job.ledger_key;
        let mut tx = self.pool.begin().await?;
        let work = async {
            // Two reviews of one job at once: the second waits, then sees the first.
            sqlx::query!("SELECT id FROM jobs WHERE id = $1 FOR UPDATE", job.id.0)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("job {}", job.id.0)))?;
            let rows = sqlx::query_scalar!(
                "SELECT event FROM ledger_events WHERE ledger_key = $1 ORDER BY id",
                key.as_str(),
            )
            .fetch_all(&mut *tx)
            .await?;
            let events = rows
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<Vec<LedgerEvent>, _>>()
                .map_err(|e| StoreError::Decode {
                    column: "ledger_events.event",
                    value: e.to_string(),
                })?;
            let before = IssueRecord::from_events(key.clone(), &events);
            if before.review.is_some() && !replace {
                return Err(StoreError::Conflict(format!(
                    "job {} is already reviewed",
                    job.id.0
                )));
            }
            let record = before.apply(&event);
            sqlx::query!(
                "INSERT INTO ledger_events (ledger_key, event) VALUES ($1, $2)",
                key.as_str(),
                encode(&event)?,
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                "INSERT INTO issue_records (ledger_key, record) VALUES ($1, $2)
                 ON CONFLICT (ledger_key)
                 DO UPDATE SET record = EXCLUDED.record, updated_at = now()",
                key.as_str(),
                encode(&record)?,
            )
            .execute(&mut *tx)
            .await?;
            let detail = serde_json::to_value(&event).map_err(|e| StoreError::Decode {
                column: "audit_log.detail",
                value: e.to_string(),
            })?;
            audit::record(&mut tx, actor, "review.submit", Target::of_job(job), detail).await?;
            Ok::<_, StoreError>(record)
        }
        .await;
        tx::commit(tx, work).await
    }
}
