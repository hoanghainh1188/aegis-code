//! The audit log (#29): who created, cancelled or retried a job, who
//! registered a repo or changed a role, who filled a review. Each entry is
//! written in the transaction of what it records, so neither lands
//! without the other.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::{ActorKind, Job, JobId, Page, PageOf, RepoId, Store, StoreError, User, UserId};

/// Who does a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub kind: ActorKind,
    /// The user behind a session or a token.
    pub user: Option<UserId>,
    /// An e-mail, a service name, `github`: kept as it was at the time.
    pub name: String,
}

impl Actor {
    /// `user` through a session (`User`) or an API token (`Token`).
    pub fn user(user: &User, kind: ActorKind) -> Self {
        Self {
            kind,
            user: Some(user.id),
            name: user.name.clone(),
        }
    }

    pub fn cli() -> Self {
        let name = std::env::var("USER").unwrap_or_else(|_| "cli".into());
        Self {
            kind: ActorKind::Cli,
            user: None,
            name,
        }
    }

    pub fn system() -> Self {
        Self {
            kind: ActorKind::System,
            user: None,
            name: "aegis".into(),
        }
    }

    pub fn webhook(source: &str) -> Self {
        Self {
            kind: ActorKind::Webhook,
            user: None,
            name: source.into(),
        }
    }
}

/// One audited write.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: Actor,
    pub action: String,
    pub repo_id: Option<RepoId>,
    pub job_id: Option<JobId>,
    pub detail: serde_json::Value,
}

/// What an entry is about.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Target {
    pub repo: Option<RepoId>,
    pub job: Option<JobId>,
}

impl Target {
    pub fn of_job(job: &Job) -> Self {
        Self {
            repo: Some(job.repo_id),
            job: Some(job.id),
        }
    }
}

/// Writes one entry on `conn`, the transaction of the write it records.
pub(crate) async fn record(
    conn: &mut PgConnection,
    actor: &Actor,
    action: &str,
    target: Target,
    detail: serde_json::Value,
) -> Result<(), StoreError> {
    sqlx::query!(
        "INSERT INTO audit_log (actor_kind, actor_id, actor_name, action, repo_id, job_id, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        actor.kind.as_str(),
        actor.user.map(|u| u.0),
        actor.name,
        action,
        target.repo.map(|r| r.0),
        target.job.map(|j| j.0),
        detail,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Which entries to read (#368); each `None` is any.
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub repo: Option<RepoId>,
    /// Who did it, by name (an e-mail, a service, `github`), case aside.
    pub actor: Option<String>,
    /// The action; one ending in `.` takes every action it starts
    /// (`job.` is `job.create`, `job.cancel`…).
    pub action: Option<String>,
}

impl Store {
    /// Newest first; only `repo`'s entries when given.
    pub async fn audit_log(
        &self,
        repo: Option<RepoId>,
        page: Page,
    ) -> Result<PageOf<AuditEntry>, StoreError> {
        let filter = AuditFilter {
            repo,
            ..AuditFilter::default()
        };
        self.audit_log_where(&filter, page).await
    }

    /// The entries `filter` takes, newest first.
    pub async fn audit_log_where(
        &self,
        filter: &AuditFilter,
        page: Page,
    ) -> Result<PageOf<AuditEntry>, StoreError> {
        let (exact, prefix) = match filter.action.as_deref() {
            Some(a) if a.ends_with('.') => (None, Some(a)),
            Some(a) => (Some(a), None),
            None => (None, None),
        };
        let rows = sqlx::query!(
            "SELECT id, at, actor_kind, actor_id, actor_name, action, repo_id, job_id, detail
             FROM audit_log
             WHERE ($1::bigint IS NULL OR repo_id = $1)
               AND ($2::bigint IS NULL OR id < $2)
               AND ($4::text IS NULL OR lower(actor_name) = lower($4))
               AND ($5::text IS NULL OR action = $5)
               AND ($6::text IS NULL OR starts_with(action, $6))
             ORDER BY id DESC LIMIT $3",
            filter.repo.map(|r| r.0),
            page.cursor(),
            page.fetch(),
            filter.actor.as_deref(),
            exact,
            prefix,
        )
        .fetch_all(&self.pool)
        .await?;
        let entries = rows
            .into_iter()
            .map(|row| {
                Ok(AuditEntry {
                    id: row.id,
                    at: row.at,
                    actor: Actor {
                        kind: ActorKind::decode("audit_log.actor_kind", &row.actor_kind)?,
                        user: row.actor_id.map(UserId),
                        name: row.actor_name,
                    },
                    action: row.action,
                    repo_id: row.repo_id.map(RepoId),
                    job_id: row.job_id.map(JobId),
                    detail: row.detail,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(PageOf::new(entries, page, |e| e.id))
    }

    /// Every action the log has, in order: what the page offers to filter by.
    pub async fn audit_actions(&self) -> Result<Vec<String>, StoreError> {
        Ok(
            sqlx::query_scalar!("SELECT DISTINCT action FROM audit_log ORDER BY action LIMIT 200")
                .fetch_all(&self.pool)
                .await?,
        )
    }
}
