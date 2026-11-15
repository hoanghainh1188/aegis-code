//! The AMS report (#526, ADR 0011 §5): per tenant and SLA level, over the
//! jobs created this month (the budget's month, in its time zone):
//!
//! - **MTTR**: from a job's creation (the ticket coming in) to the end of
//!   its last run, for the jobs that ended with a result (`done`: a PR, a
//!   diagnosis, or a stop);
//! - **SLA on time**: of the done jobs with an `sla_deadline`, those that
//!   ended by it; and the open ones already past it;
//! - **triage or PR**: done jobs resolved by a diagnosis (#451), by a PR,
//!   or neither;
//! - **cost per contract**: what each tenant's repos spent this month,
//!   against its cap.

use crate::{RepoId, Store, StoreError, TenantBudget, TenantId};

/// One tenant and SLA level.
#[derive(Debug, Clone, PartialEq)]
pub struct AmsRow {
    pub tenant: String,
    /// `P1`, `P2`, `P3`, or `none`.
    pub priority: String,
    /// Jobs that ended with a result.
    pub done: i64,
    /// Mean time from creation to the end of the last run, in seconds.
    pub mttr_s: Option<f64>,
    /// Done jobs with a deadline, and those that ended by it.
    pub with_deadline: i64,
    pub on_time: i64,
    /// Queued or running jobs already past their deadline.
    pub overdue_open: i64,
    pub diagnosed: i64,
    pub prs: i64,
}

impl AmsRow {
    /// Done jobs with a deadline that ended by it, from 0 to 1.
    pub fn on_time_ratio(&self) -> Option<f64> {
        (self.with_deadline > 0).then(|| self.on_time as f64 / self.with_deadline as f64)
    }

    /// Done jobs resolved at triage, from 0 to 1.
    pub fn triage_ratio(&self) -> Option<f64> {
        (self.done > 0).then(|| self.diagnosed as f64 / self.done as f64)
    }

    /// Done jobs resolved by a PR, from 0 to 1.
    pub fn pr_ratio(&self) -> Option<f64> {
        (self.done > 0).then(|| self.prs as f64 / self.done as f64)
    }
}

/// The report: its rows, and each tenant's spend against its cap.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AmsReport {
    pub rows: Vec<AmsRow>,
    pub contracts: Vec<TenantBudget>,
}

impl AmsReport {
    /// The rows of `tenant` summed over its levels, MTTR weighted by jobs.
    pub fn tenant_total(&self, tenant: &str) -> AmsRow {
        let rows: Vec<&AmsRow> = self.rows.iter().filter(|r| r.tenant == tenant).collect();
        let done: i64 = rows.iter().map(|r| r.done).sum();
        let weighted: f64 = rows
            .iter()
            .filter_map(|r| r.mttr_s.map(|m| m * r.done as f64))
            .sum();
        AmsRow {
            tenant: tenant.to_owned(),
            priority: "all".into(),
            done,
            mttr_s: (done > 0).then(|| weighted / done as f64),
            with_deadline: rows.iter().map(|r| r.with_deadline).sum(),
            on_time: rows.iter().map(|r| r.on_time).sum(),
            overdue_open: rows.iter().map(|r| r.overdue_open).sum(),
            diagnosed: rows.iter().map(|r| r.diagnosed).sum(),
            prs: rows.iter().map(|r| r.prs).sum(),
        }
    }
}

impl Store {
    /// The AMS report of this month, of the repos `within` only when given.
    pub async fn ams_report(&self, within: Option<&[RepoId]>) -> Result<AmsReport, StoreError> {
        let within: Option<Vec<i64>> = within.map(|r| r.iter().map(|id| id.0).collect());
        let rows = sqlx::query!(
            r#"WITH month AS (
                 SELECT date_trunc('month', now() AT TIME ZONE $1) AT TIME ZONE $1 AS start
               ),
               ended AS (
                 SELECT job_id, max(ended_at) AS at FROM runs GROUP BY job_id
               ),
               scope AS (
                 SELECT j.*, t.slug AS tenant, e.at AS ended_at
                 FROM jobs j
                 JOIN repos r ON r.id = j.repo_id
                 JOIN tenants t ON t.id = r.tenant_id
                 LEFT JOIN ended e ON e.job_id = j.id
                 WHERE j.created_at >= (SELECT start FROM month)
                   AND ($2::bigint[] IS NULL OR j.repo_id = ANY($2))
               )
               SELECT tenant AS "tenant!",
                      coalesce(priority, 'none') AS "priority!",
                      count(*) FILTER (WHERE status = 'done') AS "done!",
                      avg(extract(epoch FROM ended_at - created_at))
                        FILTER (WHERE status = 'done')::float8 AS "mttr_s",
                      count(*) FILTER (WHERE status = 'done' AND sla_deadline IS NOT NULL)
                        AS "with_deadline!",
                      count(*) FILTER (WHERE status = 'done' AND ended_at <= sla_deadline)
                        AS "on_time!",
                      count(*) FILTER (WHERE status IN ('queued', 'running')
                                         AND sla_deadline < now()) AS "overdue_open!",
                      count(*) FILTER (WHERE outcome = 'diagnosed') AS "diagnosed!",
                      count(*) FILTER (WHERE outcome = 'pr_opened') AS "prs!"
               FROM scope
               GROUP BY 1, 2
               ORDER BY 1, 2"#,
            self.budget.time_zone,
            within.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;
        let rows: Vec<AmsRow> = rows
            .into_iter()
            .map(|r| AmsRow {
                tenant: r.tenant,
                priority: r.priority,
                done: r.done,
                mttr_s: r.mttr_s,
                with_deadline: r.with_deadline,
                on_time: r.on_time,
                overdue_open: r.overdue_open,
                diagnosed: r.diagnosed,
                prs: r.prs,
            })
            .collect();
        // The contracts of the repos in view: all of them for the platform.
        let tenants: Vec<TenantId> = match &within {
            None => self.tenants().await?.into_iter().map(|t| t.id).collect(),
            Some(repos) => sqlx::query_scalar!(
                "SELECT DISTINCT tenant_id FROM repos WHERE id = ANY($1) ORDER BY tenant_id",
                repos
            )
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(TenantId)
            .collect(),
        };
        let mut contracts = Vec::with_capacity(tenants.len());
        for t in tenants {
            contracts.push(self.tenant_budget(t).await?);
        }
        Ok(AmsReport { rows, contracts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(priority: &str, done: i64, mttr: Option<f64>) -> AmsRow {
        AmsRow {
            tenant: "acme".into(),
            priority: priority.into(),
            done,
            mttr_s: mttr,
            with_deadline: done,
            on_time: done / 2,
            overdue_open: 1,
            diagnosed: 1,
            prs: done - 1,
        }
    }

    #[test]
    fn ratios_and_a_tenants_total_weight_by_jobs() {
        let p1 = row("P1", 2, Some(600.0));
        assert_eq!(p1.on_time_ratio(), Some(0.5));
        assert_eq!(p1.triage_ratio(), Some(0.5));
        assert_eq!(p1.pr_ratio(), Some(0.5));
        let none = AmsRow {
            done: 0,
            with_deadline: 0,
            mttr_s: None,
            ..row("none", 0, None)
        };
        assert_eq!(none.on_time_ratio(), None);
        let report = AmsReport {
            rows: vec![p1, row("P3", 6, Some(3000.0))],
            contracts: vec![],
        };
        let total = report.tenant_total("acme");
        assert_eq!(total.done, 8);
        // (2 × 600 + 6 × 3000) / 8
        assert_eq!(total.mttr_s, Some(2400.0));
        assert_eq!(
            (total.with_deadline, total.on_time, total.overdue_open),
            (8, 4, 2)
        );
    }
}
