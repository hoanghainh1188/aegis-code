//! A feature's own budget (#675, ADR 0019): on top of each job's cap and
//! its repo's month, the job that planned a feature may have a ceiling over
//! what it and all its sub-jobs spend.
//!
//! As with a repo's month, less than a job's cap left counts as spent: no
//! sub-job is made under the feature ([`Store::create_sub_job_by`]), and its
//! queued sub-jobs are not claimed but wait, with the reason, for a higher
//! ceiling. Sub-jobs already running end under their own cap.

use sqlx::PgConnection;

use crate::audit::{self, Target};
use crate::budget::{BudgetRules, BudgetState};
use crate::tx;
use crate::{Actor, JobId, Store, StoreError};

/// How the reason of a sub-job refused, or kept waiting, for its feature's
/// budget begins.
pub const EPIC_BUDGET_SPENT: &str = "the feature's budget is spent";

/// A feature's ceiling and what it spent.
#[derive(Debug, Clone, PartialEq)]
pub struct EpicBudget {
    /// The job that planned the feature.
    pub parent: JobId,
    /// `None`: no ceiling of its own.
    pub ceiling_usd: Option<f64>,
    /// What the planning job and its sub-jobs spent.
    pub spent_usd: f64,
}

impl EpicBudget {
    /// What is left of the ceiling, never below 0.
    pub fn left_usd(&self) -> Option<f64> {
        self.ceiling_usd.map(|cap| (cap - self.spent_usd).max(0.0))
    }

    /// As a repo's month: spent with less than a job's cap left.
    pub fn state(&self, rules: &BudgetRules) -> BudgetState {
        match self.ceiling_usd {
            None => BudgetState::Unlimited,
            Some(cap) if cap - self.spent_usd < rules.job_reserve_usd => BudgetState::Spent,
            Some(cap) if self.spent_usd >= cap * rules.warn_ratio => BudgetState::Warn,
            Some(_) => BudgetState::Ok,
        }
    }

    /// Why no more sub-job of this feature runs now.
    pub fn spent_reason(&self, rules: &BudgetRules) -> String {
        format!(
            "{EPIC_BUDGET_SPENT}: ${:.2} of ${:.2} used by job {} and its sub-jobs, and a job \
             needs ${:.2} left; its sub-jobs run once the ceiling is raised",
            self.spent_usd,
            self.ceiling_usd.unwrap_or(0.0),
            self.parent.0,
            rules.job_reserve_usd,
        )
    }
}

struct EpicRow {
    id: i64,
    epic_budget_usd: Option<f64>,
    spent: f64,
}

impl From<EpicRow> for EpicBudget {
    fn from(row: EpicRow) -> Self {
        Self {
            parent: JobId(row.id),
            ceiling_usd: row.epic_budget_usd,
            spent_usd: row.spent,
        }
    }
}

/// `BudgetSpent` when the feature planned by `parent` has spent its budget.
pub(crate) async fn check(
    conn: &mut PgConnection,
    rules: &BudgetRules,
    parent: JobId,
) -> Result<(), StoreError> {
    let budget: EpicBudget = sqlx::query_as!(
        EpicRow,
        r#"SELECT id, epic_budget_usd, epic_spent(id) AS "spent!" FROM jobs WHERE id = $1"#,
        parent.0,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("job {}", parent.0)))?
    .into();
    match budget.state(rules) {
        BudgetState::Spent => Err(StoreError::BudgetSpent(budget.spent_reason(rules))),
        _ => Ok(()),
    }
}

/// The features whose budget is spent, by the id of the job that planned
/// them; their queued sub-jobs say why they wait. For the claim, under its
/// lock: only features with a sub-job queued are looked at.
pub(crate) async fn hold_spent(
    conn: &mut PgConnection,
    rules: &BudgetRules,
) -> Result<Vec<i64>, StoreError> {
    let capped = sqlx::query_as!(
        EpicRow,
        r#"SELECT p.id, p.epic_budget_usd, epic_spent(p.id) AS "spent!"
           FROM jobs p
           WHERE p.epic_budget_usd IS NOT NULL
             AND EXISTS (SELECT 1 FROM jobs s
                         WHERE s.parent_job_id = p.id AND s.status = 'queued')"#,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut held = Vec::new();
    for budget in capped.into_iter().map(EpicBudget::from) {
        if budget.state(rules) != BudgetState::Spent {
            continue;
        }
        let reason = format!(
            "{}{}",
            crate::budget::waiting_prefix(),
            budget.spent_reason(rules)
        );
        sqlx::query!(
            "UPDATE jobs SET reason = $2
             WHERE parent_job_id = $1 AND status = 'queued' AND reason IS DISTINCT FROM $2",
            budget.parent.0,
            reason,
        )
        .execute(&mut *conn)
        .await?;
        held.push(budget.parent.0);
    }
    Ok(held)
}

impl Store {
    /// The budget of the feature planned by `parent`.
    pub async fn epic_budget(&self, parent: JobId) -> Result<EpicBudget, StoreError> {
        Ok(sqlx::query_as!(
            EpicRow,
            r#"SELECT id, epic_budget_usd, epic_spent(id) AS "spent!" FROM jobs WHERE id = $1"#,
            parent.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("job {}", parent.0)))?
        .into())
    }

    /// Sets the ceiling of the feature planned by `parent` (`None`: none of
    /// its own), audited `job.epic_budget` with the ceiling before and
    /// after. `Invalid` for a sub-job, which plans no feature.
    pub async fn set_epic_budget_by(
        &self,
        parent: JobId,
        ceiling_usd: Option<f64>,
        actor: &Actor,
    ) -> Result<EpicBudget, StoreError> {
        if let Some(cap) = ceiling_usd
            && !(cap.is_finite() && cap >= 0.0)
        {
            return Err(StoreError::Invalid {
                field: "ceiling_usd",
                message: format!("{cap}: 0 or above"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let held = sqlx::query!(
                "SELECT repo_id, parent_job_id, epic_budget_usd FROM jobs WHERE id = $1 FOR UPDATE",
                parent.0,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("job {}", parent.0)))?;
            if held.parent_job_id.is_some() {
                return Err(StoreError::Invalid {
                    field: "job",
                    message: "a sub-job has no budget of its own: set its feature's".into(),
                });
            }
            let budget: EpicBudget = sqlx::query_as!(
                EpicRow,
                r#"UPDATE jobs SET epic_budget_usd = $2, updated_at = now() WHERE id = $1
                   RETURNING id, epic_budget_usd, epic_spent(id) AS "spent!""#,
                parent.0,
                ceiling_usd,
            )
            .fetch_one(&mut *tx)
            .await?
            .into();
            let detail =
                serde_json::json!({ "ceiling_usd": ceiling_usd, "was": held.epic_budget_usd });
            let target = Target {
                repo: Some(crate::RepoId(held.repo_id)),
                job: Some(parent),
            };
            audit::record(&mut tx, actor, "job.epic_budget", target, detail).await?;
            Ok::<_, StoreError>(budget)
        }
        .await;
        tx::commit(tx, work).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(ceiling_usd: Option<f64>, spent_usd: f64) -> EpicBudget {
        EpicBudget {
            parent: JobId(7),
            ceiling_usd,
            spent_usd,
        }
    }

    #[test]
    fn less_than_a_job_left_of_the_feature_counts_as_spent() {
        let rules = BudgetRules::default();
        assert_eq!(budget(None, 1e6).state(&rules), BudgetState::Unlimited);
        assert_eq!(budget(Some(40.0), 4.0).state(&rules), BudgetState::Ok);
        assert_eq!(budget(Some(40.0), 32.0).state(&rules), BudgetState::Warn);
        assert_eq!(budget(Some(40.0), 36.5).state(&rules), BudgetState::Spent);
        assert_eq!(budget(Some(40.0), 50.0).left_usd(), Some(0.0));
        let reason = budget(Some(40.0), 37.0).spent_reason(&rules);
        assert!(reason.starts_with(EPIC_BUDGET_SPENT), "{reason}");
        assert!(
            reason.contains("$37.00 of $40.00 used by job 7"),
            "{reason}"
        );
    }
}
