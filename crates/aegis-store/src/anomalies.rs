//! Error spikes from a repo's APM source (#706): each becomes an `apm` job
//! that diagnoses it, written with the spike in one transaction. A
//! fingerprint diagnosed within the repo's dedupe period is not diagnosed
//! again, and a repo gets at most its cap of spikes per day.

use std::time::Duration;

use aegis_ledger::IssueId;

use crate::audit::{self, Target};
use crate::deliveries::queue_in;
use crate::jobs::JobRow;
use crate::{
    Actor, Delivered, Job, JobId, JobSource, NewJob, RepoId, Store, StoreError, budget, tx,
};

/// What a spike's fix job adds to the spike's `issue_ref`.
pub const FIX_SUFFIX: &str = "-fix";
const MAX_TICKET_REF_CHARS: usize = 300;

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

    /// The ticket the report of spike `issue_ref` of `repo` was posted to:
    /// `owner/name#12` on GitHub.
    pub async fn set_anomaly_ticket(
        &self,
        repo: RepoId,
        issue_ref: &str,
        ticket_ref: &str,
    ) -> Result<(), StoreError> {
        if ticket_ref.is_empty() || ticket_ref.chars().count() > MAX_TICKET_REF_CHARS {
            return Err(invalid("ticket_ref", MAX_TICKET_REF_CHARS));
        }
        sqlx::query!(
            "UPDATE apm_anomalies SET ticket_ref = $3 WHERE repo_id = $1 AND issue_ref = $2",
            repo.0,
            issue_ref,
            ticket_ref,
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::from_db)?;
        Ok(())
    }

    /// The ticket a spike's report was posted to, if it was.
    pub async fn anomaly_ticket(
        &self,
        repo: RepoId,
        issue_ref: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(sqlx::query_scalar!(
            "SELECT ticket_ref FROM apm_anomalies WHERE repo_id = $1 AND issue_ref = $2",
            repo.0,
            issue_ref,
        )
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    /// The job that diagnosed the spike whose report was posted to
    /// `ticket_ref` of `repo`.
    pub async fn anomaly_job_by_ticket(
        &self,
        repo: RepoId,
        ticket_ref: &str,
    ) -> Result<Option<JobId>, StoreError> {
        Ok(sqlx::query_scalar!(
            "SELECT j.id FROM apm_anomalies a
             JOIN jobs j ON j.repo_id = a.repo_id AND j.issue_ref = a.issue_ref
                        AND j.source = 'apm'
             WHERE a.repo_id = $1 AND a.ticket_ref = $2
             ORDER BY j.id DESC LIMIT 1",
            repo.0,
            ticket_ref,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(JobId))
    }

    /// The report of the job that diagnosed spike `issue_ref` of `repo`,
    /// the newest if it ran more than once.
    pub async fn anomaly_report(
        &self,
        repo: RepoId,
        issue_ref: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(sqlx::query_scalar!(
            "SELECT d.report FROM jobs j JOIN job_diagnoses d ON d.job_id = j.id
             WHERE j.repo_id = $1 AND j.issue_ref = $2 AND j.source = 'apm'
             ORDER BY j.id DESC LIMIT 1",
            repo.0,
            issue_ref,
        )
        .fetch_optional(&self.pool)
        .await?)
    }

    /// A fix job for the spike `job` diagnosed, asked for by `actor` (#706):
    /// an `apm` job whose `issue_ref` is the spike's with [`FIX_SUFFIX`].
    /// Refused unless `job` diagnosed a spike, or while a fix of it is live
    /// or opened a PR; the repo's budget is checked as for any job.
    pub async fn fix_anomaly_by(&self, job: JobId, actor: &Actor) -> Result<Job, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let spike = sqlx::query!(
                "SELECT repo_id, issue_ref, ledger_key, source, status, outcome
                 FROM jobs WHERE id = $1 FOR UPDATE",
                job.0,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
            let diagnosed = spike.source == JobSource::Apm.as_str()
                && !spike.issue_ref.ends_with(FIX_SUFFIX)
                && (spike.status.as_str(), spike.outcome.as_deref()) == ("done", Some("diagnosed"));
            if !diagnosed {
                return Err(StoreError::Conflict(format!(
                    "job {} is not an error spike's diagnosis",
                    job.0
                )));
            }
            let issue_ref = format!("{}{FIX_SUFFIX}", spike.issue_ref);
            let earlier = sqlx::query!(
                "SELECT status, outcome FROM jobs WHERE repo_id = $1 AND issue_ref = $2",
                spike.repo_id,
                issue_ref,
            )
            .fetch_all(&mut *tx)
            .await?;
            if earlier.iter().any(|j| {
                matches!(j.status.as_str(), "queued" | "running")
                    || j.outcome.as_deref() == Some("pr_opened")
            }) {
                return Err(StoreError::Conflict(format!(
                    "the spike of job {} already has a fix running or a PR",
                    job.0
                )));
            }
            budget::check(&mut tx, &self.budget, RepoId(spike.repo_id)).await?;
            let key = match earlier.len() {
                0 => format!("{}{FIX_SUFFIX}", spike.ledger_key),
                n => format!("{}{FIX_SUFFIX}{}", spike.ledger_key, n + 1),
            };
            let key = IssueId::parse(key)?;
            let created: Job = sqlx::query_as!(
                JobRow,
                "INSERT INTO jobs (repo_id, issue_ref, ledger_key, source)
                 VALUES ($1, $2, $3, 'apm')
                 RETURNING id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                           status, outcome, attempts, locked_by, locked_until, created_at,
                           updated_at, run_after, retries, cancel_requested, reason, pr_url,
                           retry_of, review_of, review_round, shadow, publish_of",
                spike.repo_id,
                issue_ref,
                key.as_str(),
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?
            .try_into()?;
            let detail = serde_json::json!({ "diagnosis_job": job.0 });
            audit::record(
                &mut tx,
                actor,
                "job.fix_spike",
                Target::of_job(&created),
                detail,
            )
            .await?;
            Ok::<_, StoreError>(created)
        }
        .await;
        tx::commit(tx, work).await
    }
}
