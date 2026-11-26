//! The feature lane's design gate as the server reads it (#677): the
//! repo's cap on revisions, the delivery modes a person may choose on
//! approval, and the approval itself, which makes the plan's sub-jobs
//! (#678).

use aegis_lifecycle::{FeatureDeliveryMode, FeatureLane, FeaturePlan};
use aegis_store::{Actor, DeliveryMode, Job, JobId, PlannedJob, Store, StoreError};

/// How many revisions of a plan the repo allows (`[feature_lane]
/// max_revisions`), from its settings; the lane's default when unset or
/// unreadable.
pub fn max_revisions(config: &serde_json::Value) -> u32 {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        feature_lane: FeatureLane,
    }
    serde_json::from_value::<Partial>(config.clone())
        .map(|p| p.feature_lane.max_revisions)
        .unwrap_or_else(|_| FeatureLane::default().max_revisions)
}

/// A delivery mode as a person writes it, `integration_pr` or
/// `stacked_prs`; why not, otherwise.
pub fn parse_mode(mode: &str) -> Result<DeliveryMode, String> {
    DeliveryMode::parse(mode)
        .ok_or_else(|| format!("mode `{mode}` is not `integration_pr` or `stacked_prs`"))
}

/// How the feature of `plan` is delivered when approved as `mode`:
/// `None` takes the plan's recommendation.
pub fn delivery(plan: &FeaturePlan, mode: Option<DeliveryMode>) -> FeatureDeliveryMode {
    match mode {
        None => plan.delivery,
        Some(DeliveryMode::IntegrationPr) => FeatureDeliveryMode::IntegrationPr,
        Some(DeliveryMode::StackedPrs) => FeatureDeliveryMode::StackedPrs,
    }
}

/// The sub-jobs an approval of `plan` as `mode` makes, one per sub-job of
/// the plan. `stacked_prs`: in its order, each on the one before, so each
/// PR goes on top of the last and holds every part it depends on.
/// `integration_pr`: as the plan's DAG says, so the parts that do not
/// depend on each other run side by side, each merged into the
/// integration branch once it passes.
pub fn sub_jobs_of(plan: &FeaturePlan, mode: Option<DeliveryMode>) -> Vec<PlannedJob> {
    match delivery(plan, mode) {
        FeatureDeliveryMode::IntegrationPr => plan
            .sub_jobs
            .iter()
            .map(|sub| PlannedJob {
                key: sub.id.clone(),
                depends_on: sub.depends_on.clone(),
            })
            .collect(),
        FeatureDeliveryMode::StackedPrs => plan
            .sub_jobs
            .iter()
            .scan(None::<String>, |before, sub| {
                let depends_on = before.iter().cloned().collect();
                *before = Some(sub.id.clone());
                Some(PlannedJob {
                    key: sub.id.clone(),
                    depends_on,
                })
            })
            .collect(),
    }
}

/// Approves the plan `job` waits with, as `actor`, to be delivered as
/// `mode`, and queues its sub-jobs ([`sub_jobs_of`]) with the approval. A
/// plan the store did not keep, or cannot read, makes none: it is approved
/// all the same, and logged.
pub async fn approve(
    store: &Store,
    job: JobId,
    mode: Option<DeliveryMode>,
    actor: &Actor,
) -> Result<Job, StoreError> {
    let planned = match store.feature_plan(job).await? {
        None => {
            tracing::warn!(
                job = job.0,
                "approved with no plan kept: no sub-jobs to run"
            );
            vec![]
        }
        Some(kept) => match serde_json::from_value::<FeaturePlan>(kept.plan) {
            Ok(plan) => sub_jobs_of(&plan, mode),
            Err(e) => {
                tracing::error!(job = job.0, error = %e, "the kept plan cannot be read");
                vec![]
            }
        },
    };
    store.approve_feature_by(job, mode, &planned, actor).await
}

/// Why a sub-job's task could not be read.
#[derive(Debug)]
pub enum TaskError {
    /// The store, out of reach: another attempt may do.
    Store(StoreError),
    /// What it needs is missing: no attempt changes it.
    Refused(String),
}

impl From<StoreError> for TaskError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// What `job` runs when it is a sub-job of an approved feature (#678): the
/// plan as approved (its last revision), its part of it, and for
/// `stacked_prs` the PR of the sub-job it goes on, whose branch it starts
/// from. `None` for any other job, a sub-job made by hand included.
pub async fn sub_job_task(
    store: &Store,
    job: &Job,
) -> Result<Option<aegis_lifecycle::FeatureTask>, TaskError> {
    let links = store.job_links(job.id).await?;
    let Some(parent) = links.parent else {
        return Ok(None);
    };
    let Some(sub_job) = store.plan_key(job.id).await? else {
        return Ok(None);
    };
    let refuse = |why: String| TaskError::Refused(format!("sub-job {}: {why}", job.id.0));
    let Some(kept) = store.feature_plan(parent).await? else {
        return Err(refuse(format!("job {} keeps no plan", parent.0)));
    };
    let Some(approval) = store.approval(parent).await? else {
        return Err(refuse(format!(
            "the plan of job {} is not approved",
            parent.0
        )));
    };
    let plan: FeaturePlan = serde_json::from_value(kept.plan)
        .map_err(|e| refuse(format!("the kept plan cannot be read: {e}")))?;
    let delivery = delivery(&plan, approval.delivery_mode);
    // One integration PR: the feature's branch, not the one before.
    let integration_branch = match delivery {
        FeatureDeliveryMode::StackedPrs => None,
        FeatureDeliveryMode::IntegrationPr => {
            let root = store
                .job(parent)
                .await?
                .ok_or_else(|| refuse(format!("job {} is gone", parent.0)))?;
            Some(integration_branch_of(&root))
        }
    };
    // Stacked: on the one it depends on, the sub-job before it.
    let stacked_on = match links.depends_on.last() {
        _ if integration_branch.is_some() => None,
        None => None,
        Some(&below) => {
            let below = store
                .job(below)
                .await?
                .ok_or_else(|| refuse(format!("job {} it goes on is gone", below.0)))?;
            Some(aegis_lifecycle::StackedOn {
                branch: format!("agent/{}", below.ledger_key.as_str().to_ascii_lowercase()),
                pr_url: below.pr_url,
            })
        }
    };
    Ok(Some(aegis_lifecycle::FeatureTask {
        plan,
        sub_job,
        delivery,
        stacked_on,
        integration_branch,
    }))
}

/// The integration branch of the feature `root` planned: its own agent
/// branch, so the feature's PR is an agent PR like any other.
pub fn integration_branch_of(root: &Job) -> String {
    format!("agent/{}", root.ledger_key.as_str().to_ascii_lowercase())
}

/// What `job` runs when it planned a feature delivered as one integration
/// PR, and every part of it is merged (#678): the plan as approved and its
/// integration branch, for the one PR. `None` for any other job.
pub async fn feature_delivery(
    store: &Store,
    job: &Job,
) -> Result<Option<aegis_lifecycle::FeatureDelivery>, TaskError> {
    let Some(approval) = store.approval(job.id).await? else {
        return Ok(None);
    };
    if !store.feature_integrated(job.id).await? {
        return Ok(None);
    }
    let refuse = |why: String| TaskError::Refused(format!("job {}: {why}", job.id.0));
    let Some(kept) = store.feature_plan(job.id).await? else {
        return Err(refuse("it keeps no plan".into()));
    };
    let plan: FeaturePlan = serde_json::from_value(kept.plan)
        .map_err(|e| refuse(format!("the kept plan cannot be read: {e}")))?;
    if delivery(&plan, approval.delivery_mode) != FeatureDeliveryMode::IntegrationPr {
        return Ok(None);
    }
    Ok(Some(aegis_lifecycle::FeatureDelivery {
        plan,
        branch: integration_branch_of(job),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_comes_from_the_repo_s_settings() {
        let cap = |config: serde_json::Value| max_revisions(&config);
        assert_eq!(cap(serde_json::json!({})), 3);
        assert_eq!(
            cap(serde_json::json!({ "feature_lane": { "enabled": true, "max_revisions": 1 } })),
            1
        );
        assert_eq!(
            cap(serde_json::json!({ "feature_lane": { "bogus": 1 } })),
            3
        );
        assert_eq!(parse_mode("stacked_prs"), Ok(DeliveryMode::StackedPrs));
        assert!(parse_mode("auto").is_err());
    }

    fn plan(ids: &[&str], delivery: FeatureDeliveryMode) -> FeaturePlan {
        FeaturePlan {
            summary: "s".into(),
            criteria: vec![],
            contracts: vec![],
            dependencies: vec![],
            sub_jobs: ids
                .iter()
                .map(|id| aegis_lifecycle::PlannedSubJob {
                    id: (*id).into(),
                    title: "t".into(),
                    scope: vec![],
                    depends_on: vec![],
                })
                .collect(),
            delivery,
        }
    }

    #[test]
    fn stacked_sub_jobs_each_go_on_the_one_before() {
        let stacked = plan(&["model", "api", "ui"], FeatureDeliveryMode::StackedPrs);
        let made = sub_jobs_of(&stacked, None);
        let pairs: Vec<(&str, Vec<&str>)> = made
            .iter()
            .map(|p| {
                (
                    p.key.as_str(),
                    p.depends_on.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("model", vec![]),
                ("api", vec!["model"]),
                ("ui", vec!["api"])
            ]
        );
        // The person's choice wins over the plan's: as one integration PR,
        // the parts follow the plan's own DAG.
        let mut dag = stacked.clone();
        dag.sub_jobs[2].depends_on = vec!["model".into()];
        let integrated = sub_jobs_of(&dag, Some(DeliveryMode::IntegrationPr));
        let pairs: Vec<(&str, Vec<&str>)> = integrated
            .iter()
            .map(|p| {
                (
                    p.key.as_str(),
                    p.depends_on.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![("model", vec![]), ("api", vec![]), ("ui", vec!["model"])]
        );
        let one = plan(&["all"], FeatureDeliveryMode::IntegrationPr);
        assert_eq!(sub_jobs_of(&one, None).len(), 1);
        assert_eq!(sub_jobs_of(&one, Some(DeliveryMode::StackedPrs)).len(), 1);
        assert_eq!(
            delivery(&one, Some(DeliveryMode::StackedPrs)),
            FeatureDeliveryMode::StackedPrs
        );
    }
}
