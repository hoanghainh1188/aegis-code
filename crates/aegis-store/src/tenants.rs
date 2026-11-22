//! Tenants (#525): an AMS contract. Every repo belongs to exactly one;
//! those registered before tenants existed belong to [`TenantId::DEFAULT`].
//! A tenant's people see only its repos (their roles are per repo, and a
//! tenant's admins have every right in its repos and none in another's),
//! its monthly budget caps the spend of all its repos together, at most
//! `max_concurrent_jobs` of its jobs run at once, and the claim shares
//! workers between tenants. Every change is audited.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::audit::{self, Target};
use crate::budget::{BudgetRules, BudgetState, WAITING};
use crate::tx;
use crate::users::ensure_user;
use crate::{Actor, RepoId, Store, StoreError, UserId, UserKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(pub i64);

impl TenantId {
    /// The tenant of every repo registered before tenants (#525): its
    /// paths and secret names are today's.
    pub const DEFAULT: TenantId = TenantId(1);
}

/// The locale codes a tenant may default to, as the column allows.
pub const LOCALES: [&str; 3] = ["en", "ja", "vi"];

/// How the reason of a job refused, or kept waiting, for its tenant's
/// monthly budget begins.
pub const TENANT_BUDGET_SPENT: &str = "the contract's monthly budget is spent";

#[derive(Debug, Clone, PartialEq)]
pub struct Tenant {
    pub id: TenantId,
    /// `[a-z0-9][a-z0-9-]*`, at most 63 characters: in paths and secret
    /// names.
    pub slug: String,
    pub name: String,
    /// Over all its repos; `None`: no cap.
    pub monthly_budget_usd: Option<f64>,
    /// Its jobs that may run at once, over all its repos; `None`: no cap.
    pub max_concurrent_jobs: Option<u32>,
    /// What its repos write in when a repo names no language (#589): `en`,
    /// `ja` or `vi`; `None`: the server's default.
    pub default_locale: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Tenant {
    pub fn is_default(&self) -> bool {
        self.id == TenantId::DEFAULT
    }

    /// The prefix every secret a repo of this tenant names must start with
    /// (#525): `ACME_` for `acme`; none for the default tenant.
    pub fn secret_prefix(&self) -> Option<String> {
        (!self.is_default()).then(|| format!("{}_", self.slug.replace('-', "_").to_uppercase()))
    }
}

struct TenantRow {
    id: i64,
    slug: String,
    name: String,
    monthly_budget_usd: Option<f64>,
    max_concurrent_jobs: Option<i32>,
    default_locale: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<TenantRow> for Tenant {
    fn from(r: TenantRow) -> Self {
        Self {
            id: TenantId(r.id),
            slug: r.slug,
            name: r.name,
            monthly_budget_usd: r.monthly_budget_usd,
            max_concurrent_jobs: r.max_concurrent_jobs.and_then(|n| u32::try_from(n).ok()),
            default_locale: r.default_locale,
            created_at: r.created_at,
        }
    }
}

/// A tenant's cap and what its repos spent this month.
#[derive(Debug, Clone, PartialEq)]
pub struct TenantBudget {
    pub tenant: TenantId,
    pub slug: String,
    pub monthly_usd: Option<f64>,
    pub spent_usd: f64,
}

impl TenantBudget {
    pub fn state(&self, rules: &BudgetRules) -> BudgetState {
        match self.monthly_usd {
            None => BudgetState::Unlimited,
            Some(cap) if cap - self.spent_usd < rules.job_reserve_usd => BudgetState::Spent,
            Some(cap) if self.spent_usd >= cap * rules.warn_ratio => BudgetState::Warn,
            Some(_) => BudgetState::Ok,
        }
    }

    pub fn spent_reason(&self, rules: &BudgetRules) -> String {
        format!(
            "{TENANT_BUDGET_SPENT} (`{}`): ${:.2} of ${:.2} used this month ({}) over its repos, \
             and a job needs ${:.2} left; jobs run again next month or once the cap is raised",
            self.slug,
            self.spent_usd,
            self.monthly_usd.unwrap_or(0.0),
            rules.time_zone,
            rules.job_reserve_usd,
        )
    }
}

struct BudgetRow {
    id: i64,
    slug: String,
    monthly_budget_usd: Option<f64>,
    spent: f64,
}

impl From<BudgetRow> for TenantBudget {
    fn from(r: BudgetRow) -> Self {
        Self {
            tenant: TenantId(r.id),
            slug: r.slug,
            monthly_usd: r.monthly_budget_usd,
            spent_usd: r.spent,
        }
    }
}

const MAX_NAME_CHARS: usize = 200;

fn check_slug(slug: &str) -> Result<(), StoreError> {
    let ok = !slug.is_empty()
        && slug.len() <= 63
        && slug
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid {
            field: "slug",
            message: format!(
                "`{slug}`: 1-63 of a-z, 0-9 and `-`, starting with a letter or a digit"
            ),
        })
    }
}

/// `BudgetSpent` when the tenant of `repo` has spent its month (#525).
pub(crate) async fn check(
    conn: &mut PgConnection,
    rules: &BudgetRules,
    repo: RepoId,
) -> Result<(), StoreError> {
    let budget = sqlx::query_as!(
        BudgetRow,
        r#"SELECT t.id, t.slug, t.monthly_budget_usd,
                  tenant_month_spent(t.id, $2) AS "spent!"
           FROM repos r JOIN tenants t ON t.id = r.tenant_id
           WHERE r.id = $1 AND t.monthly_budget_usd IS NOT NULL"#,
        repo.0,
        rules.time_zone,
    )
    .fetch_optional(conn)
    .await?
    .map(TenantBudget::from);
    match budget {
        Some(b) if b.state(rules) == BudgetState::Spent => {
            Err(StoreError::BudgetSpent(b.spent_reason(rules)))
        }
        _ => Ok(()),
    }
}

/// The repos of tenants whose month is spent; their queued jobs say why
/// they wait. For the claim, under its lock.
pub(crate) async fn hold_spent(
    conn: &mut PgConnection,
    rules: &BudgetRules,
) -> Result<Vec<i64>, StoreError> {
    let capped = sqlx::query_as!(
        BudgetRow,
        r#"SELECT id, slug, monthly_budget_usd, tenant_month_spent(id, $1) AS "spent!"
           FROM tenants WHERE monthly_budget_usd IS NOT NULL"#,
        rules.time_zone,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut held = Vec::new();
    for budget in capped.into_iter().map(TenantBudget::from) {
        if budget.state(rules) != BudgetState::Spent {
            continue;
        }
        let reason = format!("{WAITING}{}", budget.spent_reason(rules));
        let repos =
            sqlx::query_scalar!("SELECT id FROM repos WHERE tenant_id = $1", budget.tenant.0)
                .fetch_all(&mut *conn)
                .await?;
        sqlx::query!(
            "UPDATE jobs SET reason = $2
             WHERE repo_id = ANY($1) AND status = 'queued' AND reason IS DISTINCT FROM $2",
            &repos,
            reason,
        )
        .execute(&mut *conn)
        .await?;
        held.extend(repos);
    }
    Ok(held)
}

async fn audit_tenant(
    conn: &mut PgConnection,
    actor: &Actor,
    action: &str,
    detail: serde_json::Value,
) -> Result<(), StoreError> {
    audit::record(conn, actor, action, Target::default(), detail).await
}

impl Store {
    /// Every tenant, by id.
    pub async fn tenants(&self) -> Result<Vec<Tenant>, StoreError> {
        Ok(sqlx::query_as!(
            TenantRow,
            "SELECT id, slug, name, monthly_budget_usd, max_concurrent_jobs, default_locale, created_at
             FROM tenants ORDER BY id"
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Tenant::from)
        .collect())
    }

    pub async fn tenant(&self, id: TenantId) -> Result<Option<Tenant>, StoreError> {
        Ok(sqlx::query_as!(
            TenantRow,
            "SELECT id, slug, name, monthly_budget_usd, max_concurrent_jobs, default_locale, created_at
             FROM tenants WHERE id = $1",
            id.0
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Tenant::from))
    }

    pub async fn tenant_by_slug(&self, slug: &str) -> Result<Option<Tenant>, StoreError> {
        Ok(sqlx::query_as!(
            TenantRow,
            "SELECT id, slug, name, monthly_budget_usd, max_concurrent_jobs, default_locale, created_at
             FROM tenants WHERE slug = $1",
            slug
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Tenant::from))
    }

    /// A new tenant, audited `tenant.create`; `Conflict` when the slug is
    /// taken.
    pub async fn create_tenant_by(
        &self,
        slug: &str,
        name: &str,
        actor: &Actor,
    ) -> Result<Tenant, StoreError> {
        check_slug(slug)?;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(StoreError::Invalid {
                field: "name",
                message: format!("1-{MAX_NAME_CHARS} characters"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let tenant: Tenant = sqlx::query_as!(
                TenantRow,
                "INSERT INTO tenants (slug, name) VALUES ($1, $2)
                 RETURNING id, slug, name, monthly_budget_usd, max_concurrent_jobs, default_locale, created_at",
                slug,
                name,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?
            .into();
            let detail = serde_json::json!({ "tenant": tenant.slug, "name": tenant.name });
            audit_tenant(&mut tx, actor, "tenant.create", detail).await?;
            Ok::<_, StoreError>(tenant)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Sets `tenant`'s monthly cap and its cap of jobs at once (`None`:
    /// none), audited `tenant.limits`.
    pub async fn set_tenant_limits_by(
        &self,
        tenant: TenantId,
        monthly_usd: Option<f64>,
        max_concurrent_jobs: Option<u32>,
        actor: &Actor,
    ) -> Result<Tenant, StoreError> {
        if monthly_usd.is_some_and(|c| !c.is_finite() || c < 0.0) {
            return Err(StoreError::Invalid {
                field: "monthly_usd",
                message: "a number of dollars, 0 or more".into(),
            });
        }
        let jobs = match max_concurrent_jobs {
            Some(0) => {
                return Err(StoreError::Invalid {
                    field: "max_concurrent_jobs",
                    message: "at least 1".into(),
                });
            }
            Some(n) => Some(i32::try_from(n).map_err(|_| StoreError::Invalid {
                field: "max_concurrent_jobs",
                message: "too large".into(),
            })?),
            None => None,
        };
        let mut tx = self.pool.begin().await?;
        let work = async {
            let tenant: Tenant = sqlx::query_as!(
                TenantRow,
                "UPDATE tenants SET monthly_budget_usd = $2, max_concurrent_jobs = $3
                 WHERE id = $1
                 RETURNING id, slug, name, monthly_budget_usd, max_concurrent_jobs, default_locale, created_at",
                tenant.0,
                monthly_usd,
                jobs,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("tenant {}", tenant.0)))?
            .into();
            let detail = serde_json::json!({
                "tenant": tenant.slug,
                "monthly_usd": monthly_usd,
                "max_concurrent_jobs": max_concurrent_jobs,
            });
            audit_tenant(&mut tx, actor, "tenant.limits", detail).await?;
            Ok::<_, StoreError>(tenant)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Sets the language `tenant`'s repos write in when a repo names none
    /// (#589): `en`, `ja` or `vi`, `None` for the server's default;
    /// audited `tenant.locale`.
    pub async fn set_tenant_locale_by(
        &self,
        tenant: TenantId,
        locale: Option<&str>,
        actor: &Actor,
    ) -> Result<Tenant, StoreError> {
        if locale.is_some_and(|l| !LOCALES.contains(&l)) {
            return Err(StoreError::Invalid {
                field: "default_locale",
                message: format!("one of {}", LOCALES.join(", ")),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let tenant: Tenant = sqlx::query_as!(
                TenantRow,
                "UPDATE tenants SET default_locale = $2
                 WHERE id = $1
                 RETURNING id, slug, name, monthly_budget_usd, max_concurrent_jobs, \
                 default_locale, created_at",
                tenant.0,
                locale,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("tenant {}", tenant.0)))?
            .into();
            let detail = serde_json::json!({ "tenant": tenant.slug, "default_locale": locale });
            audit_tenant(&mut tx, actor, "tenant.locale", detail).await?;
            Ok::<_, StoreError>(tenant)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// What `tenant`'s repos spent this month, and its cap.
    pub async fn tenant_budget(&self, tenant: TenantId) -> Result<TenantBudget, StoreError> {
        Ok(sqlx::query_as!(
            BudgetRow,
            r#"SELECT id, slug, monthly_budget_usd, tenant_month_spent(id, $2) AS "spent!"
               FROM tenants WHERE id = $1"#,
            tenant.0,
            self.budget.time_zone,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("tenant {}", tenant.0)))?
        .into())
    }

    /// Makes `kind`/`name` an admin of `tenant`, or no longer one, audited
    /// `tenant.admin.grant` / `tenant.admin.revoke`.
    pub async fn set_tenant_admin_by(
        &self,
        tenant: TenantId,
        kind: UserKind,
        name: &str,
        admin: bool,
        actor: &Actor,
    ) -> Result<crate::User, StoreError> {
        let t = self
            .tenant(tenant)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("tenant {}", tenant.0)))?;
        let mut tx = self.pool.begin().await?;
        let work = async {
            let user = ensure_user(&mut tx, kind, name, admin).await?;
            if admin {
                sqlx::query!(
                    "INSERT INTO tenant_admins (tenant_id, user_id) VALUES ($1, $2)
                     ON CONFLICT DO NOTHING",
                    tenant.0,
                    user.id.0,
                )
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query!(
                    "DELETE FROM tenant_admins WHERE tenant_id = $1 AND user_id = $2",
                    tenant.0,
                    user.id.0,
                )
                .execute(&mut *tx)
                .await?;
            }
            let action = if admin {
                "tenant.admin.grant"
            } else {
                "tenant.admin.revoke"
            };
            let detail = serde_json::json!({ "tenant": t.slug, "user": user.name });
            audit_tenant(&mut tx, actor, action, detail).await?;
            Ok::<_, StoreError>(user)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// The tenants `user` administers.
    pub async fn tenants_administered(&self, user: UserId) -> Result<Vec<TenantId>, StoreError> {
        Ok(sqlx::query_scalar!(
            "SELECT tenant_id FROM tenant_admins WHERE user_id = $1 ORDER BY tenant_id",
            user.0
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(TenantId)
        .collect())
    }

    /// The repos of `tenants`.
    pub async fn repos_of_tenants(&self, tenants: &[TenantId]) -> Result<Vec<RepoId>, StoreError> {
        let ids: Vec<i64> = tenants.iter().map(|t| t.0).collect();
        Ok(sqlx::query_scalar!(
            "SELECT id FROM repos WHERE tenant_id = ANY($1) ORDER BY id",
            &ids
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(RepoId)
        .collect())
    }

    /// Moves `repo` to `tenant`, audited `repo.tenant`. Its clone and job
    /// dirs move with it on disk (`aegis-lifecycle`), so it is meant for a
    /// repo with no job running.
    pub async fn set_repo_tenant_by(
        &self,
        repo: RepoId,
        tenant: TenantId,
        actor: &Actor,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let running = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!" FROM jobs WHERE repo_id = $1 AND status = 'running'"#,
                repo.0
            )
            .fetch_one(&mut *tx)
            .await?;
            if running > 0 {
                return Err(StoreError::Conflict(format!(
                    "repo {} has {running} job(s) running; move it once they end",
                    repo.0
                )));
            }
            let moved = sqlx::query!(
                "UPDATE repos SET tenant_id = $2 WHERE id = $1",
                repo.0,
                tenant.0
            )
            .execute(&mut *tx)
            .await
            .map_err(|e| match StoreError::from_db(e) {
                StoreError::NotFound(_) => StoreError::NotFound(format!("tenant {}", tenant.0)),
                other => other,
            })?;
            if moved.rows_affected() == 0 {
                return Err(StoreError::NotFound(format!("repo {}", repo.0)));
            }
            let target = Target {
                repo: Some(repo),
                job: None,
            };
            let detail = serde_json::json!({ "tenant": tenant.0 });
            audit::record(&mut tx, actor, "repo.tenant", target, detail).await?;
            Ok::<_, StoreError>(())
        }
        .await;
        tx::commit(tx, work).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_for_paths_and_secret_names() {
        for ok in ["acme", "acme-jp", "a1"] {
            assert!(check_slug(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Acme", "-acme", "acme_jp", "a/b", &"a".repeat(64)] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_other_tenants_prefix_their_secrets() {
        let t = |id, slug: &str| Tenant {
            id: TenantId(id),
            slug: slug.into(),
            name: "n".into(),
            monthly_budget_usd: None,
            max_concurrent_jobs: None,
            default_locale: None,
            created_at: Utc::now(),
        };
        assert_eq!(t(1, "default").secret_prefix(), None);
        assert_eq!(t(2, "acme-jp").secret_prefix().as_deref(), Some("ACME_JP_"));
    }
}
