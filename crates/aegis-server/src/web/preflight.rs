//! The pre-flight check of a repo from its page (#599): the checks of
//! `aegis-local check-repo` ([`aegis_lifecycle::run_preflight`]) on the
//! server's clone, in the background, a few at a time, each item shown as
//! it comes (htmx polling). A repo added from the pages waits, paused, for
//! a check that passes; then its admin turns it on, with no `add-repo.sh`.

use std::sync::Arc;

use aegis_lifecycle::{CheckItem, CheckStatus, PreflightOptions, PreflightReport, run_preflight};
use aegis_store::{
    Actor, CloneState, PreflightItem, PreflightState, Repo, RepoId, RepoPreflight, Store,
};
use askama::Template;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use tokio::sync::Semaphore;

use super::admin::repo_admin_only;
use super::repo_admin::{RepoAdmin, RepoAdminExt, editing, repo_of};
use super::{Me, WebError, WebResult, render};
use crate::auth::Access;

/// Checks running at once on this server: each runs the repo's gate.
pub const MAX_CONCURRENT_PREFLIGHTS: usize = 2;
/// Why a repo added from the pages is paused until its check passes.
pub const AWAITING_PREFLIGHT: &str = "Added from the pages: waiting for its pre-flight check";

/// The checks' slots, shared by the server's pages.
pub(super) fn slots() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(MAX_CONCURRENT_PREFLIGHTS))
}

fn status_of(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "pass",
        CheckStatus::Fail => "fail",
        CheckStatus::Warn => "warn",
        CheckStatus::Skip => "skip",
    }
}

fn stored(item: &CheckItem) -> PreflightItem {
    PreflightItem {
        category: item.category.to_owned(),
        name: item.name.clone(),
        status: status_of(item.status).to_owned(),
        detail: item.detail.clone(),
        is_blocking: item.is_blocking,
    }
}

/// How a finished report ends.
fn outcome(report: &PreflightReport) -> PreflightState {
    if report.has_blocking_failures() {
        PreflightState::Failed
    } else if report
        .items
        .iter()
        .any(|i| matches!(i.status, CheckStatus::Warn | CheckStatus::Fail))
    {
        PreflightState::Warned
    } else {
        PreflightState::Passed
    }
}

/// One failing item, for a check that could not run.
fn cannot_run(name: &str, detail: String) -> PreflightItem {
    PreflightItem {
        category: "Server".into(),
        name: name.into(),
        status: "fail".into(),
        detail,
        is_blocking: true,
    }
}

/// Runs `repo`'s check, its items stored as they come; ends it however
/// it goes, so a failure never leaves it `running`.
async fn run(store: Store, admin: RepoAdmin, repo: Repo, slots: Arc<Semaphore>) {
    let ended = check(&store, &admin, &repo, &slots).await;
    if let Err(e) = store.finish_preflight(repo.id, ended).await {
        tracing::error!(repo = %repo.slug, error = %e, "recording the pre-flight check");
    }
}

async fn check(store: &Store, admin: &RepoAdmin, repo: &Repo, slots: &Semaphore) -> PreflightState {
    let fail = |name: &str, detail: String| {
        let item = cannot_run(name, detail);
        async move {
            if let Err(e) = store.add_preflight_item(repo.id, &item).await {
                tracing::error!(repo = %repo.slug, error = %e, "recording a pre-flight item");
            }
            PreflightState::Failed
        }
    };
    let Ok(_slot) = slots.acquire().await else {
        return fail("Check slot", "the server is stopping".into()).await;
    };
    match store.repo_clone(repo.id).await {
        Ok(Some(c)) if c.state != CloneState::Done => {
            return fail(
                "Clone",
                "the repo's clone is not ready: wait for it, or fix it and save the repo again"
                    .into(),
            )
            .await;
        }
        Ok(_) => {}
        Err(e) => return fail("Clone", e.to_string()).await,
    }
    let cfg = match admin.modes().repo_run(store, repo.id).await {
        Ok(run) => run.config,
        Err(e) => return fail("Configuration", e.to_string()).await,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CheckItem>();
    let writer = {
        let (store, id, slug) = (store.clone(), repo.id, repo.slug.clone());
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                if let Err(e) = store.add_preflight_item(id, &stored(&item)).await {
                    tracing::error!(repo = %slug, error = %e, "recording a pre-flight item");
                }
            }
        })
    };
    let mut report = PreflightReport::with_progress(tx);
    let options = PreflightOptions {
        fetch: true,
        ..PreflightOptions::default()
    };
    run_preflight(&cfg, &options, &mut report).await;
    let ended = outcome(&report);
    drop(report);
    let _ = writer.await;
    ended
}

/// Starts `repo`'s check by `actor` in the background; `false` when one
/// is running already.
pub(super) async fn start(
    store: &Store,
    admin: &RepoAdmin,
    repo: &Repo,
    actor: &Actor,
) -> Result<bool, WebError> {
    if !store.start_preflight_by(repo.id, actor).await? {
        return Ok(false);
    }
    let (store, admin, repo) = (store.clone(), admin.clone(), repo.clone());
    let slots = Arc::clone(&admin.preflights);
    tokio::spawn(run(store, admin, repo, slots));
    Ok(true)
}

pub async fn start_check(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
) -> WebResult {
    let repo = repo_of(&store, id).await?;
    repo_admin_only(&access, repo.id)?;
    let admin = editing(admin)?;
    let done = if start(&store, &admin, &repo, &access.actor).await? {
        "preflight"
    } else {
        "preflight-running"
    };
    Ok(Redirect::to(&format!("/admin/repos/{id}?done={done}")).into_response())
}

/// Turns on a repo paused for its check, once its last check passed
/// (warnings allowed) and nothing runs.
pub async fn activate(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    let repo = repo_of(&store, id).await?;
    repo_admin_only(&access, repo.id)?;
    let ready = store
        .repo_preflight(repo.id)
        .await?
        .is_some_and(|p| p.state.ready());
    if !ready {
        return Err(WebError::new(
            StatusCode::CONFLICT,
            "the repo's last pre-flight check did not pass: run it again once its \
             blocking items are fixed",
        ));
    }
    store.resume_repo_by(repo.id, &access.actor).await?;
    Ok(Redirect::to(&format!("/admin/repos/{id}?done=activated")).into_response())
}

/// The check's part of a repo's page; while it runs it asks for itself
/// again.
#[derive(Template)]
#[template(path = "admin_repo_preflight.html")]
pub struct PreflightPart {
    /// For the forms' CSRF token.
    pub me: Me,
    pub repo_id: i64,
    pub check: Option<CheckView>,
    /// The repo is paused waiting for this check, which passed.
    pub can_activate: bool,
    /// The server can run one: it loaded the modes.
    pub can_run: bool,
    /// One is running: no button to start another.
    pub running: bool,
}

pub struct CheckView {
    pub state: &'static str,
    pub live: bool,
    pub items: Vec<ItemView>,
    pub started_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// One item as the page shows it: its status one of the four the page
/// translates.
pub struct ItemView {
    pub category: String,
    pub name: String,
    pub status: &'static str,
    pub detail: String,
    pub is_blocking: bool,
}

impl From<PreflightItem> for ItemView {
    fn from(i: PreflightItem) -> Self {
        Self {
            status: match i.status.as_str() {
                "pass" => "pass",
                "warn" => "warn",
                "skip" => "skip",
                _ => "fail",
            },
            category: i.category,
            name: i.name,
            detail: i.detail,
            is_blocking: i.is_blocking,
        }
    }
}

impl From<RepoPreflight> for CheckView {
    fn from(p: RepoPreflight) -> Self {
        Self {
            state: p.state.as_str(),
            live: p.state == PreflightState::Running,
            items: p.items.into_iter().map(ItemView::from).collect(),
            started_by: p.started_by,
            started_at: crate::when::second(p.started_at),
            finished_at: p.finished_at.map(crate::when::second),
        }
    }
}

pub async fn part_of(
    store: &Store,
    me: &Me,
    repo: RepoId,
    can_run: bool,
) -> Result<PreflightPart, WebError> {
    let check = store.repo_preflight(repo).await?;
    let waiting = store
        .repo_pause(repo)
        .await?
        .is_some_and(|p| p.reason.as_deref() == Some(AWAITING_PREFLIGHT));
    Ok(PreflightPart {
        me: me.clone(),
        repo_id: repo.0,
        can_activate: waiting && check.as_ref().is_some_and(|c| c.state.ready()),
        running: check
            .as_ref()
            .is_some_and(|c| c.state == PreflightState::Running),
        check: check.map(CheckView::from),
        can_run,
    })
}

pub async fn part(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
    Path(id): Path<i64>,
) -> WebResult {
    let repo = repo_of(&store, id).await?;
    repo_admin_only(&access, repo.id)?;
    render(&part_of(&store, &me, repo.id, admin.is_some()).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(status: CheckStatus, is_blocking: bool) -> CheckItem {
        CheckItem {
            category: "Git",
            name: "x".into(),
            status,
            detail: String::new(),
            is_blocking,
        }
    }

    #[test]
    fn a_report_ends_failed_warned_or_passed() {
        let mut report = PreflightReport::default();
        report.add(item(CheckStatus::Pass, true));
        report.add(item(CheckStatus::Skip, false));
        assert_eq!(outcome(&report), PreflightState::Passed);
        report.add(item(CheckStatus::Fail, false));
        assert_eq!(
            outcome(&report),
            PreflightState::Warned,
            "a failure that blocks nothing"
        );
        report.add(item(CheckStatus::Warn, false));
        assert_eq!(outcome(&report), PreflightState::Warned);
        report.add(item(CheckStatus::Fail, true));
        assert_eq!(outcome(&report), PreflightState::Failed);
        assert_eq!(stored(&report.items[4]).status, "fail");
    }
}
