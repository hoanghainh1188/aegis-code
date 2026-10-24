//! A repo's budget per month (#31), on top of each job's own cap ($4, ADR
//! 0005). What a repo spent this month is the cost in its jobs' ledgers
//! recorded since the month began, in the configured time zone.
//!
//! A job that starts may spend up to its own cap, so a repo with less than
//! that left of its month counts as spent: no job is created for it
//! ([`Store::create_job_by`], webhooks, retries), and its queued jobs are
//! not claimed but wait, with the reason, for the next month or a higher
//! cap. Jobs already running end under their own cap.

use std::sync::Arc;

use aegis_ledger::COST_PER_ISSUE_MAX;
use sqlx::PgConnection;

use crate::audit::{self, Target};
use crate::{Actor, RepoId, Store, StoreError};

/// How the reason of a job refused, or kept waiting, for its repo's
/// monthly budget begins.
pub const BUDGET_SPENT: &str = "the repo's monthly budget is spent";
/// How the reason of a queued job kept waiting begins; cleared once it
/// is claimed.
const WAITING: &str = "waiting: ";

/// How the store applies the monthly budgets.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetRules {
    /// Where a month begins: an IANA name, `UTC` by default.
    pub time_zone: String,
    /// What one more job may spend: its own cap (the worker's
    /// `max_cost_usd`). A repo with less left than this is spent.
    pub job_reserve_usd: f64,
    /// The share of the cap from which the budget shows as nearly spent.
    pub warn_ratio: f64,
}

impl Default for BudgetRules {
    fn default() -> Self {
        Self {
            time_zone: "UTC".into(),
            job_reserve_usd: COST_PER_ISSUE_MAX,
            warn_ratio: 0.8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetState {
    /// No monthly cap.
    Unlimited,
    Ok,
    /// At `warn_ratio` of the cap or above.
    Warn,
    /// Less than a job's cap left: no new job runs.
    Spent,
}

impl BudgetState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unlimited => "unlimited",
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Spent => "spent",
        }
    }
}

/// A repo's cap and what it spent this month.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoBudget {
    pub repo_id: RepoId,
    pub slug: String,
    /// `None`: no monthly cap.
    pub monthly_usd: Option<f64>,
    pub spent_usd: f64,
}

impl RepoBudget {
    /// What is left of the cap, never below 0.
    pub fn left_usd(&self) -> Option<f64> {
        self.monthly_usd.map(|cap| (cap - self.spent_usd).max(0.0))
    }

    pub fn state(&self, rules: &BudgetRules) -> BudgetState {
        match self.monthly_usd {
            None => BudgetState::Unlimited,
            Some(cap) if cap - self.spent_usd < rules.job_reserve_usd => BudgetState::Spent,
            Some(cap) if self.spent_usd >= cap * rules.warn_ratio => BudgetState::Warn,
            Some(_) => BudgetState::Ok,
        }
    }

    /// Why no job of this repo runs now.
    pub fn spent_reason(&self, rules: &BudgetRules) -> String {
        format!(
            "{BUDGET_SPENT}: ${:.2} of ${:.2} used this month ({}), and a job needs ${:.2} left; \
             jobs run again next month or once the cap is raised",
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

impl From<BudgetRow> for RepoBudget {
    fn from(row: BudgetRow) -> Self {
        Self {
            repo_id: RepoId(row.id),
            slug: row.slug,
            monthly_usd: row.monthly_budget_usd,
            spent_usd: row.spent,
        }
    }
}

/// `BudgetSpent` when `repo`'s budget is spent; `NotFound` when there is
/// no such repo.
pub(crate) async fn check(
    conn: &mut PgConnection,
    rules: &BudgetRules,
    repo: RepoId,
) -> Result<(), StoreError> {
    let budget: RepoBudget = sqlx::query_as!(
        BudgetRow,
        r#"SELECT id, slug, monthly_budget_usd, repo_month_spent(id, $2) AS "spent!"
           FROM repos WHERE id = $1"#,
        repo.0,
        rules.time_zone,
    )
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("repo {}", repo.0)))?
    .into();
    match budget.state(rules) {
        BudgetState::Spent => Err(StoreError::BudgetSpent(budget.spent_reason(rules))),
        _ => Ok(()),
    }
}

/// The repos whose budget is spent; their queued jobs say why they wait.
/// For the claim, under its lock.
pub(crate) async fn hold_spent(
    conn: &mut PgConnection,
    rules: &BudgetRules,
) -> Result<Vec<i64>, StoreError> {
    let capped = sqlx::query_as!(
        BudgetRow,
        r#"SELECT id, slug, monthly_budget_usd, repo_month_spent(id, $1) AS "spent!"
           FROM repos WHERE monthly_budget_usd IS NOT NULL"#,
        rules.time_zone,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut held = Vec::new();
    for budget in capped.into_iter().map(RepoBudget::from) {
        if budget.state(rules) != BudgetState::Spent {
            continue;
        }
        let reason = format!("{WAITING}{}", budget.spent_reason(rules));
        sqlx::query!(
            "UPDATE jobs SET reason = $2
             WHERE repo_id = $1 AND status = 'queued' AND reason IS DISTINCT FROM $2",
            budget.repo_id.0,
            reason,
        )
        .execute(&mut *conn)
        .await?;
        held.push(budget.repo_id.0);
    }
    Ok(held)
}

/// The start of a waiting job's reason, to clear when it is claimed.
pub(crate) fn waiting_prefix() -> String {
    format!("{WAITING}{BUDGET_SPENT}")
}

impl Store {
    /// The store, applying `rules`. `Invalid` when the database does not
    /// know the time zone or a ratio is out of range.
    pub async fn with_budget(self, rules: BudgetRules) -> Result<Self, StoreError> {
        let known = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM pg_timezone_names WHERE name = $1) AS "known!""#,
            rules.time_zone,
        )
        .fetch_one(&self.pool)
        .await?;
        if !known {
            return Err(StoreError::Invalid {
                field: "time_zone",
                message: format!("`{}` is not a time zone name", rules.time_zone),
            });
        }
        if !(rules.job_reserve_usd.is_finite() && rules.job_reserve_usd >= 0.0) {
            return Err(StoreError::Invalid {
                field: "job_reserve_usd",
                message: format!("{}: 0 or above", rules.job_reserve_usd),
            });
        }
        if !(rules.warn_ratio > 0.0 && rules.warn_ratio <= 1.0) {
            return Err(StoreError::Invalid {
                field: "warn_ratio",
                message: format!("{}: above 0, at most 1", rules.warn_ratio),
            });
        }
        Ok(Self {
            budget: Arc::new(rules),
            ..self
        })
    }

    pub fn budget_rules(&self) -> &BudgetRules {
        &self.budget
    }

    pub async fn repo_budget(&self, repo: RepoId) -> Result<Option<RepoBudget>, StoreError> {
        Ok(sqlx::query_as!(
            BudgetRow,
            r#"SELECT id, slug, monthly_budget_usd, repo_month_spent(id, $2) AS "spent!"
               FROM repos WHERE id = $1"#,
            repo.0,
            self.budget.time_zone,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(RepoBudget::from))
    }

    /// Every repo's budget, or those `within` only, by id: for the
    /// dashboard and the metrics (#32).
    pub async fn repo_budgets(
        &self,
        within: Option<&[RepoId]>,
    ) -> Result<Vec<RepoBudget>, StoreError> {
        let within: Option<Vec<i64>> = within.map(|repos| repos.iter().map(|r| r.0).collect());
        let rows = sqlx::query_as!(
            BudgetRow,
            r#"SELECT id, slug, monthly_budget_usd, repo_month_spent(id, $1) AS "spent!"
               FROM repos WHERE $2::bigint[] IS NULL OR id = ANY($2)
               ORDER BY id"#,
            self.budget.time_zone,
            within.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(RepoBudget::from).collect())
    }

    /// Sets `repo`'s monthly cap (`None`: no cap), audited as done by
    /// `actor` with the cap before and after.
    pub async fn set_monthly_budget_by(
        &self,
        repo: RepoId,
        monthly_usd: Option<f64>,
        actor: &Actor,
    ) -> Result<RepoBudget, StoreError> {
        if let Some(cap) = monthly_usd
            && !(cap.is_finite() && cap >= 0.0)
        {
            return Err(StoreError::Invalid {
                field: "monthly_usd",
                message: format!("{cap}: 0 or above"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let was = sqlx::query_scalar!(
            "SELECT monthly_budget_usd FROM repos WHERE id = $1 FOR UPDATE",
            repo.0,
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("repo {}", repo.0)))?;
        let budget: RepoBudget = sqlx::query_as!(
            BudgetRow,
            r#"UPDATE repos SET monthly_budget_usd = $2 WHERE id = $1
               RETURNING id, slug, monthly_budget_usd, repo_month_spent(id, $3) AS "spent!""#,
            repo.0,
            monthly_usd,
            self.budget.time_zone,
        )
        .fetch_one(&mut *tx)
        .await?
        .into();
        let detail = serde_json::json!({ "monthly_usd": monthly_usd, "was": was });
        let target = Target {
            repo: Some(repo),
            job: None,
        };
        audit::record(&mut tx, actor, "repo.budget", target, detail).await?;
        tx.commit().await?;
        Ok(budget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(monthly_usd: Option<f64>, spent_usd: f64) -> RepoBudget {
        RepoBudget {
            repo_id: RepoId(1),
            slug: "typst".into(),
            monthly_usd,
            spent_usd,
        }
    }

    #[test]
    fn less_than_a_job_left_counts_as_spent() {
        let rules = BudgetRules::default();
        assert_eq!(budget(None, 1e6).state(&rules), BudgetState::Unlimited);
        assert_eq!(budget(Some(100.0), 10.0).state(&rules), BudgetState::Ok);
        assert_eq!(budget(Some(100.0), 80.0).state(&rules), BudgetState::Warn);
        assert_eq!(budget(Some(100.0), 96.0).state(&rules), BudgetState::Warn);
        assert_eq!(budget(Some(100.0), 96.01).state(&rules), BudgetState::Spent);
        assert_eq!(budget(Some(3.0), 0.0).state(&rules), BudgetState::Spent);
        assert_eq!(budget(Some(100.0), 120.0).left_usd(), Some(0.0));
        let reason = budget(Some(100.0), 97.5).spent_reason(&rules);
        assert!(reason.starts_with(BUDGET_SPENT), "{reason}");
        assert!(reason.contains("$97.50 of $100.00"), "{reason}");
    }
}
