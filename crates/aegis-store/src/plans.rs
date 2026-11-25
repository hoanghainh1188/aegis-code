//! A feature's plans (#677, ADR 0019): the Architect phase of a job
//! writes one, a person approves it or asks for another revision. The
//! store keeps each revision as the lifecycle wrote it, as JSON: the plan's
//! shape is the lifecycle's.

use time::OffsetDateTime;

use crate::{JobId, Store, StoreError};

/// The largest plan kept, in bytes of JSON, as the table allows.
pub const MAX_PLAN_BYTES: usize = 200_000;

/// One revision of a job's plan.
#[derive(Debug, Clone, PartialEq)]
pub struct FeaturePlan {
    pub job: JobId,
    /// 1 for the first plan, then one more for each revision.
    pub revision: i32,
    pub plan: serde_json::Value,
    pub created_at: OffsetDateTime,
}

impl Store {
    /// Keeps `plan` as the next revision of `job`'s plan. `Invalid` when it
    /// is larger than [`MAX_PLAN_BYTES`].
    pub async fn save_feature_plan(
        &self,
        job: JobId,
        plan: &serde_json::Value,
    ) -> Result<FeaturePlan, StoreError> {
        if plan.to_string().len() > MAX_PLAN_BYTES {
            return Err(StoreError::Invalid {
                field: "plan",
                message: format!("at most {MAX_PLAN_BYTES} bytes of JSON"),
            });
        }
        let row = sqlx::query!(
            r#"INSERT INTO feature_plans (job_id, revision, plan)
               SELECT $1, coalesce(max(revision), 0) + 1, $2
               FROM feature_plans WHERE job_id = $1
               RETURNING revision, plan, created_at"#,
            job.0,
            plan,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_db)?;
        Ok(FeaturePlan {
            job,
            revision: row.revision,
            plan: row.plan,
            created_at: row.created_at,
        })
    }

    /// The last revision of `job`'s plan, if it has one.
    pub async fn feature_plan(&self, job: JobId) -> Result<Option<FeaturePlan>, StoreError> {
        Ok(sqlx::query!(
            "SELECT revision, plan, created_at FROM feature_plans
             WHERE job_id = $1 ORDER BY revision DESC LIMIT 1",
            job.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|r| FeaturePlan {
            job,
            revision: r.revision,
            plan: r.plan,
            created_at: r.created_at,
        }))
    }

    /// Every revision of `job`'s plan, the first first.
    pub async fn feature_plans(&self, job: JobId) -> Result<Vec<FeaturePlan>, StoreError> {
        Ok(sqlx::query!(
            "SELECT revision, plan, created_at FROM feature_plans
             WHERE job_id = $1 ORDER BY revision",
            job.0,
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| FeaturePlan {
            job,
            revision: r.revision,
            plan: r.plan,
            created_at: r.created_at,
        })
        .collect())
    }
}
