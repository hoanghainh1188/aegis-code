//! What a maintainer does to jobs from the pages (#309): `POST /jobs`
//! creates one, `POST /jobs/{id}/cancel` and `…/retry` act on one. The
//! same checks as the API's ([`crate::api::new_job`], the store's rules on
//! which job may be cancelled or run again) and the same audit, done as
//! the signed-in user; the guard has checked the session and its CSRF
//! token. Each ends on the job's page with what was done.

use aegis_store::{JobId, JobStatus, RepoId, RepoRole, Store};
use axum::Extension;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;

use super::{WebError, WebResult, job_for};
use crate::api::{CreateCveJob, cve_job, new_job};
use crate::auth::Access;

/// The trackers a job may be read from, as `aegis --source` takes
/// them; the label shown for each.
pub(super) const SOURCES: [(&str, &str); 7] = [
    // The first, chosen by default (#564): the issue's number on the
    // chosen repo's own forge.
    (
        crate::repo_issue::REPO_SOURCE,
        "This repo's issues (12 or #12)",
    ),
    ("github", "GitHub issue (owner/repo#12)"),
    ("gitlab", "GitLab issue (group/project#12)"),
    ("jira", "Jira ticket (KEY-12)"),
    ("jsm", "JSM ticket (KEY-12)"),
    ("servicenow", "ServiceNow incident (INC0010001)"),
    ("backlog", "Backlog ticket (KEY-12)"),
];

/// What `?done=` on a job's page may say: a fixed word, never the request's
/// own text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    Created,
    Cancelled,
    CancelRequested,
    Retried,
    /// A review was recorded from the form (#562).
    Reviewed,
    /// A recalled memory was marked obsolete from the job's page (#658).
    MemoryObsolete,
    /// The job's diagnosis was confirmed and kept as a memory (#657).
    DiagnosisConfirmed,
    /// The job's plan was approved and queued again (#675).
    Approved,
}

impl Done {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Created => "Job created; a worker picks it up when the repo has a free slot.",
            Self::Cancelled => "Job cancelled.",
            Self::CancelRequested => {
                "Cancel requested; the job stops when its worker next checks in."
            }
            Self::Retried => "Job queued again as this new job.",
            Self::Reviewed => {
                "Review recorded for the measures. The PR itself is unchanged: approve, \
                 merge or ask for changes on its forge."
            }
            Self::MemoryObsolete => {
                "Memory marked obsolete: no job is given it again until its text changes."
            }
            Self::DiagnosisConfirmed => {
                "Diagnosis confirmed: the repo's next jobs are given it as a memory."
            }
            Self::Approved => {
                "Plan approved, to be delivered as it recommends. Its sub-jobs are not run yet: \
                 running them comes with a later release (#678)."
            }
        }
    }

    const ALL: [Self; 8] = [
        Self::Created,
        Self::Cancelled,
        Self::CancelRequested,
        Self::Retried,
        Self::Reviewed,
        Self::MemoryObsolete,
        Self::DiagnosisConfirmed,
        Self::Approved,
    ];

    fn word(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Cancelled => "cancelled",
            Self::CancelRequested => "cancel_requested",
            Self::Retried => "retried",
            Self::Reviewed => "reviewed",
            Self::MemoryObsolete => "memory_obsolete",
            Self::DiagnosisConfirmed => "diagnosis_confirmed",
            Self::Approved => "approved",
        }
    }

    /// A word the page does not know shows nothing.
    pub(super) fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.word() == word)
    }
}

fn to_job(id: JobId, done: Done) -> axum::response::Response {
    Redirect::to(&format!("/jobs/{}?done={}", id.0, done.word())).into_response()
}

#[derive(Debug, Deserialize)]
pub struct CreateForm {
    repo_id: i64,
    issue_ref: String,
    source: String,
}

pub async fn create(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Form(form): Form<CreateForm>,
) -> WebResult {
    let repo_id = RepoId(form.repo_id);
    // A repo they cannot see does not exist for them, as for jobs.
    if !access.can(repo_id, RepoRole::Viewer) {
        return Err(WebError::not_found(format!("repo {}", form.repo_id)));
    }
    if !access.can(repo_id, RepoRole::Maintainer) {
        return Err(WebError::new(
            StatusCode::FORBIDDEN,
            "you need role maintainer in this repo to create jobs",
        ));
    }
    // "This repo's issues" (#564): `12` or `#12` on the repo's forge.
    let repo = store
        .repo(repo_id)
        .await?
        .ok_or_else(|| WebError::not_found(format!("repo {}", form.repo_id)))?;
    let (source, issue_ref) = crate::repo_issue::resolve(&repo, &form.source, &form.issue_ref)
        .map_err(|why| WebError::new(StatusCode::BAD_REQUEST, why))?;
    let new = new_job(repo_id, &issue_ref, &source, None)
        .map_err(|why| WebError::new(StatusCode::BAD_REQUEST, why))?;
    let job = store.create_job_by(&new, &access.actor).await?;
    Ok(to_job(job.id, Done::Created))
}

#[derive(Debug, Deserialize)]
pub struct CreateCveForm {
    repo_id: i64,
    package: String,
    version: String,
    advisory: String,
}

/// A CVE job from the form on the jobs page (#502): maintainers only.
pub async fn create_cve(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Form(form): Form<CreateCveForm>,
) -> WebResult {
    let repo_id = RepoId(form.repo_id);
    if !access.can(repo_id, RepoRole::Viewer) {
        return Err(WebError::not_found(format!("repo {}", form.repo_id)));
    }
    if !access.can(repo_id, RepoRole::Maintainer) {
        return Err(WebError::new(
            StatusCode::FORBIDDEN,
            "you need role maintainer in this repo to create jobs",
        ));
    }
    let target = CreateCveJob {
        package: form.package,
        version: form.version,
        advisory: form.advisory,
    };
    let new =
        cve_job(repo_id, &target).map_err(|why| WebError::new(StatusCode::BAD_REQUEST, why))?;
    let job = store.create_job_by(&new, &access.actor).await?;
    Ok(to_job(job.id, Done::Created))
}

pub async fn cancel(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    job_for(&store, &access, id, RepoRole::Maintainer).await?;
    let job = store.request_cancel_by(JobId(id), &access.actor).await?;
    // A running job stops when its worker next renews the lease.
    let done = match job.status {
        JobStatus::Cancelled => Done::Cancelled,
        _ => Done::CancelRequested,
    };
    Ok(to_job(job.id, done))
}

/// Whether `access` may mark `memory` obsolete: a maintainer of its repo,
/// and for one shared with the tenant its admin (#596, #658).
pub(super) fn may_curate(access: &Access, memory: &aegis_store::RepoMemory) -> bool {
    match memory.scope {
        aegis_store::MemoryScope::Repo => access.can(memory.repo_id, RepoRole::Maintainer),
        aegis_store::MemoryScope::Tenant => access.administers(memory.repo_id),
    }
}

/// Marks a memory job `id` was given obsolete (#658): recall leaves it
/// out until its text changes. Only a memory of this job's, by who may
/// curate it.
pub async fn mark_memory_obsolete(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path((id, memory_id)): Path<(i64, i64)>,
) -> WebResult {
    let job = job_for(&store, &access, id, RepoRole::Viewer).await?;
    let given = store
        .recalled_memories(job.id)
        .await?
        .into_iter()
        .find(|r| r.memory.id.0 == memory_id)
        .ok_or_else(|| WebError::not_found(format!("memory {memory_id} of job {id}")))?;
    if !may_curate(&access, &given.memory) {
        return Err(WebError::new(
            StatusCode::FORBIDDEN,
            "marking this memory obsolete takes a maintainer of its repo, or the tenant's \
             admin for one shared with the tenant",
        ));
    }
    store
        .mark_memory_obsolete_by(
            given.memory.repo_id,
            given.memory.id,
            &access.actor,
            Some(job.id),
        )
        .await?;
    Ok(to_job(job.id, Done::MemoryObsolete))
}

/// Confirms the diagnosis job `id` wrote back to its ticket (#657): what
/// it found is kept as a `confirmed_diagnosis` memory of its repo, for
/// the next tickets like it. Takes the reviewer role, as a PR's review.
pub async fn confirm_diagnosis(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Extension(admin): super::repo_admin::RepoAdminExt,
    Path(id): Path<i64>,
) -> WebResult {
    let job = job_for(&store, &access, id, RepoRole::Reviewer).await?;
    let sync = admin.as_ref().map(super::RepoAdmin::memory_sync);
    match crate::memory_learn::confirmed_diagnosis_with(&store, &job, sync.as_ref(), &access.actor)
        .await?
    {
        Some(_) => Ok(to_job(job.id, Done::DiagnosisConfirmed)),
        None => Err(WebError::new(
            StatusCode::CONFLICT,
            format!("job {id} wrote no diagnosis back to its ticket: there is nothing to confirm"),
        )),
    }
}

/// Approves the plan job `id` waits with (#675, #677), as it recommends
/// delivering it; the job keeps waiting, approved, for its sub-jobs (#678).
/// As retry, for the repo's maintainers, since it will spend the repo's
/// budget.
pub async fn approve(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    job_for(&store, &access, id, RepoRole::Maintainer).await?;
    let job = store.approve_job_by(JobId(id), None, &access.actor).await?;
    Ok(to_job(job.id, Done::Approved))
}

pub async fn retry(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    job_for(&store, &access, id, RepoRole::Maintainer).await?;
    let job = store.retry_job_by(JobId(id), &access.actor).await?;
    Ok(to_job(job.id, Done::Retried))
}
