//! Error spikes from a repo's APM source (#706): each becomes an `apm` job
//! that diagnoses it, written with the spike in one transaction. A
//! fingerprint diagnosed within the repo's dedupe period is not diagnosed
//! again, and a repo gets at most its cap of spikes per day.

use std::time::Duration;

use crate::deliveries::queue_in;
use crate::tx;
use crate::{Actor, Delivered, NewJob, RepoId, Store, StoreError};

const MAX_FINGERPRINT_CHARS: usize = 300;
const MAX_TITLE_CHARS: usize = 300;
const MAX_BODY_CHARS: usize = 8_000;

/// One spike, as the job that diagnoses it reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAnomaly {
    /// `sentry:<issue>`, `datadog:<error kind>`: the same spike again.
    pub fingerprint: String,
    pub title: String,
    /// The issue body the job is given (Markdown, its APM text fenced).
    pub body: String,
}

/// How often a repo's spikes are diagnosed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnomalyLimits {
    /// A fingerprint diagnosed within this is not diagnosed again.
    pub dedupe: Duration,
    /// Spikes diagnosed per repo in any 24 hours.
    pub per_day: u32,
}

/// Why a spike was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnomalySkipped {
    /// Its fingerprint was diagnosed within the dedupe period.
    Seen,
    /// The repo had its spikes of the day.
    Capped,
    /// The job's `issue_ref` already has a live job.
    Live,
}

fn invalid(field: &'static str, max: usize) -> StoreError {
    StoreError::Invalid {
        field,
        message: format!("1 to {max} characters"),
    }
}

fn check(anomaly: &NewAnomaly) -> Result<(), StoreError> {
    let fits = |text: &str, max: usize| !text.trim().is_empty() && text.chars().count() <= max;
    if !fits(&anomaly.fingerprint, MAX_FINGERPRINT_CHARS) {
        return Err(invalid("fingerprint", MAX_FINGERPRINT_CHARS));
    }
    if !fits(&anomaly.title, MAX_TITLE_CHARS) {
        return Err(invalid("title", MAX_TITLE_CHARS));
    }
    if !fits(&anomaly.body, MAX_BODY_CHARS) {
        return Err(invalid("body", MAX_BODY_CHARS));
    }
    Ok(())
}

impl Store {
    /// Queues `job` (its `issue_ref` names the spike) to diagnose
    /// `anomaly`, as [`Store::deliver_job`] does, unless its fingerprint was
    /// diagnosed within `limits.dedupe` or the repo had `limits.per_day`
    /// spikes in the last 24 hours; then nothing is written. Spikes of one
    /// repo are queued one at a time.
    pub async fn deliver_anomaly(
        &self,
        job: &NewJob,
        anomaly: &NewAnomaly,
        limits: AnomalyLimits,
    ) -> Result<Result<Delivered, AnomalySkipped>, StoreError> {
        check(anomaly)?;
        let dedupe_s = i64::try_from(limits.dedupe.as_secs()).unwrap_or(i64::MAX);
        let mut tx = self.pool.begin().await?;
        let work = async {
            // One repo's spikes in turn, so two polls keep to the cap.
            sqlx::query!(
                "SELECT pg_advisory_xact_lock(hashtextextended('apm_anomalies', $1))",
                job.repo_id.0,
            )
            .execute(&mut *tx)
            .await?;
            let seen = sqlx::query_scalar!(
                r#"SELECT EXISTS (
                     SELECT 1 FROM apm_anomalies
                     WHERE repo_id = $1 AND fingerprint = $2
                       AND found_at > now() - make_interval(secs => $3::bigint)
                   ) AS "seen!""#,
                job.repo_id.0,
                anomaly.fingerprint,
                dedupe_s,
            )
            .fetch_one(&mut *tx)
            .await?;
            if seen {
                return Ok(tx::Done::Discard(Err(AnomalySkipped::Seen)));
            }
            let today = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!" FROM apm_anomalies
                   WHERE repo_id = $1 AND found_at > now() - interval '24 hours'"#,
                job.repo_id.0,
            )
            .fetch_one(&mut *tx)
            .await?;
            if today >= i64::from(limits.per_day) {
                return Ok(tx::Done::Discard(Err(AnomalySkipped::Capped)));
            }
            sqlx::query!(
                "INSERT INTO apm_anomalies (repo_id, issue_ref, fingerprint, title, body)
                 VALUES ($1, $2, $3, $4, $5)",
                job.repo_id.0,
                job.issue_ref,
                anomaly.fingerprint,
                anomaly.title,
                anomaly.body,
            )
            .execute(&mut *tx)
            .await
            .map_err(|e| match StoreError::from_db(e) {
                StoreError::NotFound(_) => StoreError::NotFound(format!("repo {}", job.repo_id.0)),
                other => other,
            })?;
            let detail = serde_json::json!({ "fingerprint": anomaly.fingerprint });
            let (_, delivered) =
                queue_in(&mut tx, &self.budget, &Actor::webhook("apm"), detail, job).await?;
            if matches!(delivered, Delivered::AlreadyLive(_)) {
                return Ok(tx::Done::Discard(Err(AnomalySkipped::Live)));
            }
            Ok::<_, StoreError>(tx::Done::Commit(Ok(delivered)))
        }
        .await;
        tx::end(tx, work).await
    }

    /// The spike `issue_ref` of `repo` names: its title and body.
    pub async fn anomaly(
        &self,
        repo: RepoId,
        issue_ref: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(sqlx::query!(
            "SELECT title, body FROM apm_anomalies WHERE repo_id = $1 AND issue_ref = $2",
            repo.0,
            issue_ref,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|row| (row.title, row.body)))
    }
}
