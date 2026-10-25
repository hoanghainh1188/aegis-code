//! The records the dashboard's KPIs come from (#170): each ledger's saved
//! summary record with its job and repo, in one query, instead of folding
//! every event of every ledger on each page load.

use aegis_ledger::{IssueId, IssueRecord, LedgerStore};
use chrono::{DateTime, Utc};

use crate::{RepoId, Store, StoreError};

/// The job a ledger belongs to, with what the dashboard groups by.
#[derive(Debug, Clone, PartialEq)]
pub struct KpiJob {
    pub repo_id: RepoId,
    pub repo_slug: String,
    /// As the repo was registered; the dashboard lowercases it.
    pub language: String,
    pub created_at: DateTime<Utc>,
}

/// One ledger's record and its job; `None` for CLI and eval runs.
#[derive(Debug, Clone, PartialEq)]
pub struct KpiRecord {
    pub record: IssueRecord,
    pub job: Option<KpiJob>,
}

impl Store {
    /// The record of every ledger with events, as folding its events gives
    /// it, of the jobs of the repos `within` only when given (runs outside
    /// jobs are then left out too).
    ///
    /// The saved record is the folded one except when its save failed after
    /// `finished` or `reviewed` was appended: those ledgers, the ones with
    /// such an event newer than their record, are folded here. A ledger
    /// without a record and without `finished` has not ended: its record is
    /// empty, which no KPI counts, but its job's groups still show.
    pub async fn kpi_records(
        &self,
        within: Option<&[RepoId]>,
    ) -> Result<Vec<KpiRecord>, StoreError> {
        let within: Option<Vec<i64>> = within.map(|repos| repos.iter().map(|r| r.0).collect());
        let rows = sqlx::query!(
            r#"WITH stale AS (
                 SELECT DISTINCT e.ledger_key FROM ledger_events e
                 LEFT JOIN issue_records r ON r.ledger_key = e.ledger_key
                 WHERE e.kind IN ('finished', 'reviewed')
                   AND (r.ledger_key IS NULL OR e.recorded_at > r.updated_at)
               ), keys AS (
                 SELECT ledger_key FROM issue_records
                 UNION SELECT ledger_key FROM stale
                 UNION SELECT ledger_key FROM jobs
               )
               SELECT k.ledger_key AS "ledger_key!",
                      CASE WHEN s.ledger_key IS NULL THEN r.record END AS "record?",
                      s.ledger_key IS NOT NULL AS "stale!",
                      j.repo_id AS "repo_id?", p.slug AS "slug?", p.language AS "language?",
                      j.created_at AS "created_at?"
               FROM keys k
               LEFT JOIN stale s ON s.ledger_key = k.ledger_key
               LEFT JOIN issue_records r ON r.ledger_key = k.ledger_key
               LEFT JOIN jobs j ON j.ledger_key = k.ledger_key
               LEFT JOIN repos p ON p.id = j.repo_id
               WHERE ($1::bigint[] IS NULL OR j.repo_id = ANY($1))
                 AND EXISTS (SELECT 1 FROM ledger_events e WHERE e.ledger_key = k.ledger_key)
               ORDER BY k.ledger_key"#,
            within.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;
        let ledger = self.ledger();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let issue = IssueId::parse(row.ledger_key)?;
            let record = match (row.stale, row.record) {
                (true, _) => {
                    let events = ledger.events(&issue).await?;
                    IssueRecord::from_events(issue, &events)
                }
                (false, Some(json)) => {
                    serde_json::from_value(json).map_err(|e| StoreError::Decode {
                        column: "issue_records.record",
                        value: e.to_string(),
                    })?
                }
                (false, None) => IssueRecord::new(issue),
            };
            let job = match (row.repo_id, row.slug, row.language, row.created_at) {
                (Some(repo_id), Some(repo_slug), Some(language), Some(created_at)) => {
                    Some(KpiJob {
                        repo_id: RepoId(repo_id),
                        repo_slug,
                        language,
                        created_at,
                    })
                }
                _ => None,
            };
            out.push(KpiRecord { record, job });
        }
        Ok(out)
    }
}
