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
use crate::api::new_job;
use crate::auth::Access;

/// The trackers a job may be read from, as `aegis --source` takes
/// them; the label shown for each.
pub(super) const SOURCES: [(&str, &str); 4] = [
    ("github", "GitHub issue (owner/repo#12)"),
    ("gitlab", "GitLab issue (group/project#12)"),
    ("jira", "Jira ticket (KEY-12)"),
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
        }
    }

    const ALL: [Self; 4] = [
        Self::Created,
        Self::Cancelled,
        Self::CancelRequested,
        Self::Retried,
    ];

    fn word(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Cancelled => "cancelled",
            Self::CancelRequested => "cancel_requested",
            Self::Retried => "retried",
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
    let new = new_job(repo_id, &form.issue_ref, &form.source, None)
        .map_err(|why| WebError::new(StatusCode::BAD_REQUEST, why))?;
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

pub async fn retry(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    job_for(&store, &access, id, RepoRole::Maintainer).await?;
    let job = store.retry_job_by(JobId(id), &access.actor).await?;
    Ok(to_job(job.id, Done::Retried))
}
