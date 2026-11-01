//! What became of each webhook delivery (#331), for the repo's connection
//! page: when the forge last reached the server, and why a delivery made
//! no job. Never a delivery's content.

use chrono::{DateTime, Utc};

use crate::audit::{self, Target};
use crate::{Actor, JobId, RepoId, Store, StoreError, WebhookOutcome, WebhookSource};

/// How long a delivery's outcome is kept.
const KEPT_DAYS: i32 = 30;
const MAX_REASON_CHARS: usize = 500;
const MAX_NAME_CHARS: usize = 128;

/// A delivery's outcome to record. `delivery_id`, `event` and `repo` are
/// `None` for a delivery whose signature did not verify: nothing it says
/// is trusted.
#[derive(Debug, Clone)]
pub struct NewWebhookEvent {
    pub source: WebhookSource,
    pub delivery_id: Option<String>,
    pub event: Option<String>,
    pub repo: Option<RepoId>,
    pub outcome: WebhookOutcome,
    pub reason: Option<String>,
    /// The job it made or found (#373).
    pub job: Option<JobId>,
}

/// A recorded delivery of a repo.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookEvent {
    pub source: WebhookSource,
    pub delivery_id: Option<String>,
    pub event: Option<String>,
    pub outcome: WebhookOutcome,
    pub reason: Option<String>,
    pub job_id: Option<JobId>,
    pub received_at: DateTime<Utc>,
}

/// Deliveries of a source whose signature did not verify, lately.
#[derive(Debug, Clone, PartialEq)]
pub struct BadSignatures {
    pub source: WebhookSource,
    pub count: i64,
    pub last: DateTime<Utc>,
}

fn cut(text: Option<&str>, max: usize) -> Option<String> {
    text.map(|t| t.chars().take(max).collect())
}

impl Store {
    /// Records `event`; outcomes older than 30 days go at the same time.
    pub async fn record_webhook(&self, event: &NewWebhookEvent) -> Result<(), StoreError> {
        sqlx::query!(
            "WITH pruned AS (
                 DELETE FROM webhook_events
                 WHERE received_at < now() - make_interval(days => $7)
             )
             INSERT INTO webhook_events
                 (source, delivery_id, event, repo_id, outcome, reason, job_id)
             VALUES ($1, $2, $3, $4, $5, $6, $8)",
            event.source.as_str(),
            cut(event.delivery_id.as_deref(), MAX_NAME_CHARS),
            cut(event.event.as_deref(), MAX_NAME_CHARS),
            event.repo.map(|r| r.0),
            event.outcome.as_str(),
            cut(event.reason.as_deref(), MAX_REASON_CHARS),
            KEPT_DAYS,
            event.job.map(|j| j.0),
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::from_db)?;
        Ok(())
    }

    /// `repo`'s last `limit` deliveries, the latest first.
    pub async fn repo_webhooks(
        &self,
        repo: RepoId,
        limit: u32,
    ) -> Result<Vec<WebhookEvent>, StoreError> {
        let rows = sqlx::query!(
            "SELECT source, delivery_id, event, outcome, reason, job_id, received_at
             FROM webhook_events WHERE repo_id = $1
             ORDER BY received_at DESC, id DESC LIMIT $2",
            repo.0,
            i64::from(limit),
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(WebhookEvent {
                    source: WebhookSource::decode("webhook_events.source", &r.source)?,
                    delivery_id: r.delivery_id,
                    event: r.event,
                    outcome: WebhookOutcome::decode("webhook_events.outcome", &r.outcome)?,
                    reason: r.reason,
                    job_id: r.job_id.map(JobId),
                    received_at: r.received_at,
                })
            })
            .collect()
    }

    /// That `actor` made or updated `repo`'s webhook `hook_id` on its forge
    /// (#376), on the repo itself or its fork (`place`): only that, never
    /// the token it was made with nor its secret.
    pub async fn audit_webhook_made_by(
        &self,
        repo: RepoId,
        place: &str,
        hook_id: u64,
        created: bool,
        actor: &Actor,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let target = Target {
            repo: Some(repo),
            job: None,
        };
        let detail = serde_json::json!({ "place": place, "hook_id": hook_id, "created": created });
        let work = audit::record(&mut tx, actor, "repo.webhook", target, detail).await;
        crate::tx::commit(tx, work).await
    }

    /// Deliveries whose signature did not verify in the last `hours`, by
    /// source: they name no repo, so every repo's page shows them.
    pub async fn bad_signatures(&self, hours: i32) -> Result<Vec<BadSignatures>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT source, count(*) AS "count!", max(received_at) AS "last!"
               FROM webhook_events
               WHERE outcome = 'bad_signature' AND received_at > now() - make_interval(hours => $1)
               GROUP BY source ORDER BY source"#,
            hours,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(BadSignatures {
                    source: WebhookSource::decode("webhook_events.source", &r.source)?,
                    count: r.count,
                    last: r.last,
                })
            })
            .collect()
    }
}
