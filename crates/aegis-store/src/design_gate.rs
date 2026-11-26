//! The feature lane's design gate (#677, ADR 0019 §3): a job whose plan
//! waits for a person ends `awaiting_approval`. Someone who may write to
//! the repo either approves it, choosing how the feature is delivered, or
//! asks for a revision with feedback, up to the repo's cap, and the job
//! plans again. Who may do either is the caller's to check.
//!
//! An approval is final. The plan's sub-jobs are made with it (#678), and
//! the job that planned them waits, approved, with its ticket, while they
//! run.

use std::collections::{BTreeMap, BTreeSet};

use aegis_ledger::IssueId;
use time::OffsetDateTime;

use crate::audit::{self, Target};
use crate::budget;
use crate::jobs::JobRow;
use crate::tx;
use crate::{Actor, Job, JobId, JobSource, NewJob, RepoId, SlaPriority, Store, StoreError};

/// The longest feedback kept with a revision, as the table allows.
pub const MAX_FEEDBACK_CHARS: usize = 4_000;

/// How an approved feature reaches the main branch (#678).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// One integration branch, one final PR.
    IntegrationPr,
    /// One PR per sub-job, each on the one before.
    StackedPrs,
}

impl DeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IntegrationPr => "integration_pr",
            Self::StackedPrs => "stacked_prs",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "integration_pr" => Some(Self::IntegrationPr),
            "stacked_prs" => Some(Self::StackedPrs),
            _ => None,
        }
    }
}

/// The approval of a job's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// The actor's name as it was then: an e-mail, a forge login.
    pub by: String,
    pub at: OffsetDateTime,
    /// `None`: as the plan recommends.
    pub delivery_mode: Option<DeliveryMode>,
}

/// A revision of a job's plan someone asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRequest {
    /// 1 for the first revision asked for: the plan's revision 2.
    pub revision: i32,
    pub feedback: String,
    pub by: String,
    pub at: OffsetDateTime,
}

/// One of an approved plan's sub-jobs to make (#678): its id in the plan,
/// and the earlier ones it waits for, by theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedJob {
    pub key: String,
    pub depends_on: Vec<String>,
}

/// The ledger key of the sub-job `key` of the feature job `parent` planned.
pub fn sub_job_ledger_key(parent: JobId, key: &str) -> Result<IssueId, StoreError> {
    Ok(IssueId::parse(format!("F{}-{key}", parent.0))?)
}

struct Held {
    repo_id: i64,
    issue_ref: String,
    source: String,
    priority: Option<String>,
    sla_deadline: Option<OffsetDateTime>,
    status: String,
    outcome: Option<String>,
    approved: bool,
}

/// `job`, locked, when its plan waits for approval and is not approved.
async fn waiting(conn: &mut sqlx::PgConnection, job: JobId) -> Result<Held, StoreError> {
    let held = sqlx::query_as!(
        Held,
        r#"SELECT j.repo_id, j.issue_ref, j.source, j.priority, j.sla_deadline, j.status, j.outcome,
                  EXISTS (SELECT 1 FROM job_approvals a WHERE a.job_id = j.id) AS "approved!"
           FROM jobs j WHERE j.id = $1 FOR UPDATE OF j"#,
        job.0,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("job {}", job.0)))?;
    if (held.status.as_str(), held.outcome.as_deref()) != ("done", Some("awaiting_approval")) {
        return Err(StoreError::Conflict(format!(
            "job {} does not wait for approval",
            job.0
        )));
    }
    if held.approved {
        return Err(StoreError::Conflict(format!(
            "the plan of job {} is approved already",
            job.0
        )));
    }
    Ok(held)
}

/// Queues `planned` under `parent`, which `held` is: the ids of the jobs
/// made, in order.
async fn make_sub_jobs(
    conn: &mut sqlx::PgConnection,
    store: &Store,
    parent: JobId,
    held: &Held,
    planned: &[PlannedJob],
    actor: &Actor,
) -> Result<Vec<i64>, StoreError> {
    let source = JobSource::decode("jobs.source", &held.source)?;
    let priority = held
        .priority
        .as_deref()
        .map(|p| SlaPriority::decode("jobs.priority", p))
        .transpose()?;
    let mut made: BTreeMap<&str, JobId> = BTreeMap::new();
    let mut ids = Vec::with_capacity(planned.len());
    for sub in planned {
        let mut deps = BTreeSet::new();
        for key in &sub.depends_on {
            let dep = made.get(key.as_str()).ok_or_else(|| StoreError::Invalid {
                field: "sub_jobs",
                message: format!("`{}` depends on `{key}`, not a sub-job before it", sub.key),
            })?;
            deps.insert(*dep);
        }
        if made.contains_key(sub.key.as_str()) {
            return Err(StoreError::Invalid {
                field: "sub_jobs",
                message: format!("`{}` is planned twice", sub.key),
            });
        }
        let new = NewJob {
            repo_id: RepoId(held.repo_id),
            issue_ref: held.issue_ref.clone(),
            ledger_key: sub_job_ledger_key(parent, &sub.key)?,
            source,
            priority,
            sla_deadline: held.sla_deadline,
        };
        let created =
            crate::dag::insert_sub_job(conn, store, parent, &deps, &new, Some(&sub.key), actor)
                .await?;
        made.insert(&sub.key, created.id);
        ids.push(created.id.0);
    }
    Ok(ids)
}

impl Store {
    /// Approves the plan `job` waits with, as `actor`, to be delivered as
    /// `mode` (`None`: as the plan recommends), and makes none of its
    /// sub-jobs: [`Store::approve_feature_by`] with none.
    pub async fn approve_job_by(
        &self,
        job: JobId,
        mode: Option<DeliveryMode>,
        actor: &Actor,
    ) -> Result<Job, StoreError> {
        self.approve_feature_by(job, mode, &[], actor).await
    }

    /// Approves the plan `job` waits with, as `actor`, to be delivered as
    /// `mode` (`None`: as the plan recommends), and queues `planned`, its
    /// sub-jobs, under it in the same transaction (#678): each on the
    /// feature's ticket, with a ledger of its own
    /// ([`sub_job_ledger_key`]), claimed once those it depends on opened
    /// their PR. The job keeps waiting, approved, with its ticket.
    /// `Conflict` when it does not wait for approval or is approved
    /// already; `Invalid` when a sub-job's id is repeated or it depends on
    /// one not before it; `BudgetSpent` when the repo's monthly budget or
    /// the feature's is spent.
    pub async fn approve_feature_by(
        &self,
        job: JobId,
        mode: Option<DeliveryMode>,
        planned: &[PlannedJob],
        actor: &Actor,
    ) -> Result<Job, StoreError> {
        if planned.len() > crate::MAX_SUB_JOBS {
            return Err(StoreError::Invalid {
                field: "sub_jobs",
                message: format!("at most {}", crate::MAX_SUB_JOBS),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let held = waiting(&mut tx, job).await?;
            budget::check(&mut tx, &self.budget, RepoId(held.repo_id)).await?;
            sqlx::query!(
                "INSERT INTO job_approvals (job_id, approved_by, delivery_mode) VALUES ($1, $2, $3)",
                job.0,
                actor.name,
                mode.map(DeliveryMode::as_str),
            )
            .execute(&mut *tx)
            .await?;
            let made = make_sub_jobs(&mut tx, self, job, &held, planned, actor).await?;
            let approved = self.job_in(&mut tx, job).await?;
            let detail = serde_json::json!({
                "delivery_mode": mode.map(DeliveryMode::as_str),
                "sub_jobs": made,
            });
            audit::record(
                &mut tx,
                actor,
                "job.approve",
                Target::of_job(&approved),
                detail,
            )
            .await?;
            Ok::<_, StoreError>(approved)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// The approval of `job`'s plan, if it has one.
    pub async fn approval(&self, job: JobId) -> Result<Option<Approval>, StoreError> {
        sqlx::query!(
            "SELECT approved_by, approved_at, delivery_mode FROM job_approvals WHERE job_id = $1",
            job.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|r| {
            let delivery_mode = match r.delivery_mode.as_deref() {
                None => None,
                Some(s) => Some(DeliveryMode::parse(s).ok_or_else(|| StoreError::Decode {
                    column: "job_approvals.delivery_mode",
                    value: s.to_owned(),
                })?),
            };
            Ok(Approval {
                by: r.approved_by,
                at: r.approved_at,
                delivery_mode,
            })
        })
        .transpose()
    }

    /// Asks for a revision of the plan `job` waits with, as `actor`, with
    /// `feedback`: the job is queued again to plan anew. At most
    /// `max_revisions` per job. `Invalid` for empty or overlong feedback;
    /// `Conflict` when the job does not wait for approval, is approved, was
    /// revised `max_revisions` times, or its ticket has another live job;
    /// `BudgetSpent` when the repo's monthly budget is spent.
    pub async fn revise_job_by(
        &self,
        job: JobId,
        feedback: &str,
        max_revisions: u32,
        actor: &Actor,
    ) -> Result<(Job, RevisionRequest), StoreError> {
        let feedback = feedback.trim();
        if feedback.is_empty() || feedback.chars().count() > MAX_FEEDBACK_CHARS {
            return Err(StoreError::Invalid {
                field: "feedback",
                message: format!("1 to {MAX_FEEDBACK_CHARS} characters"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let held = waiting(&mut tx, job).await?;
            let asked = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!" FROM job_revisions WHERE job_id = $1"#,
                job.0,
            )
            .fetch_one(&mut *tx)
            .await?;
            if asked >= i64::from(max_revisions) {
                return Err(StoreError::Conflict(format!(
                    "the plan of job {} was revised {asked} times, the repo's cap",
                    job.0
                )));
            }
            budget::check(&mut tx, &self.budget, RepoId(held.repo_id)).await?;
            let queued: Job = sqlx::query_as!(
                JobRow,
                "UPDATE jobs SET status = 'queued', outcome = NULL, reason = NULL,
                                 cancel_requested = false, run_after = now(), updated_at = now()
                 WHERE id = $1
                 RETURNING id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                           status, outcome, attempts, locked_by, locked_until, created_at,
                           updated_at, run_after, retries, cancel_requested, reason, pr_url,
                           retry_of, review_of, review_round",
                job.0,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| match StoreError::from_db(e) {
                StoreError::Conflict(_) => StoreError::Conflict(format!(
                    "the ticket of job {} has another live job",
                    job.0
                )),
                other => other,
            })?
            .try_into()?;
            let row = sqlx::query!(
                "INSERT INTO job_revisions (job_id, revision, feedback, requested_by)
                 VALUES ($1, $2, $3, $4)
                 RETURNING revision, feedback, requested_by, requested_at",
                job.0,
                i32::try_from(asked + 1).unwrap_or(i32::MAX),
                feedback,
                actor.name,
            )
            .fetch_one(&mut *tx)
            .await?;
            let request = RevisionRequest {
                revision: row.revision,
                feedback: row.feedback,
                by: row.requested_by,
                at: row.requested_at,
            };
            let detail = serde_json::json!({ "revision": request.revision });
            audit::record(
                &mut tx,
                actor,
                "job.revise",
                Target::of_job(&queued),
                detail,
            )
            .await?;
            Ok::<_, StoreError>((queued, request))
        }
        .await;
        tx::commit(tx, work).await
    }

    /// The last revision asked for of `job`'s plan, if any.
    pub async fn revision_request(
        &self,
        job: JobId,
    ) -> Result<Option<RevisionRequest>, StoreError> {
        Ok(sqlx::query!(
            "SELECT revision, feedback, requested_by, requested_at FROM job_revisions
             WHERE job_id = $1 ORDER BY revision DESC LIMIT 1",
            job.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|r| RevisionRequest {
            revision: r.revision,
            feedback: r.feedback,
            by: r.requested_by,
            at: r.requested_at,
        }))
    }

    /// Every revision asked for of `job`'s plan, the first first.
    pub async fn revision_requests(&self, job: JobId) -> Result<Vec<RevisionRequest>, StoreError> {
        Ok(sqlx::query!(
            "SELECT revision, feedback, requested_by, requested_at FROM job_revisions
             WHERE job_id = $1 ORDER BY revision",
            job.0,
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| RevisionRequest {
            revision: r.revision,
            feedback: r.feedback,
            by: r.requested_by,
            at: r.requested_at,
        })
        .collect())
    }

    async fn job_in(&self, conn: &mut sqlx::PgConnection, job: JobId) -> Result<Job, StoreError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, repo_id, issue_ref, ledger_key, source, priority, sla_deadline,
                    status, outcome, attempts, locked_by, locked_until, created_at, updated_at,
                    run_after, retries, cancel_requested, reason, pr_url, retry_of, review_of,
                    review_round
             FROM jobs WHERE id = $1",
            job.0,
        )
        .fetch_one(&mut *conn)
        .await?
        .try_into()
    }
}
