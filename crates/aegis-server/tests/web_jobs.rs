//! #309: a maintainer creates, cancels and retries jobs from the pages, as
//! the API lets them: the same checks, the same audit, behind the session
//! and its CSRF token. Who sees which button follows the role.

mod common;

use aegis_ledger::IssueId;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    ActorKind, JobEnd, JobId, JobSource, JobStatus, NewJob, NewRepo, Page, RepoId, RepoMode,
    RepoRole, Store,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

struct Answer {
    status: StatusCode,
    location: String,
    body: String,
}

async fn send(app: &Router, request: Request<Body>) -> Answer {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|l| l.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        location,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn page(app: &Router, who: &Browser, uri: &str) -> Answer {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

/// A form `who` sends, with their session's CSRF token unless `csrf` is
/// false.
async fn post(app: &Router, who: &Browser, uri: &str, fields: &str, csrf: bool) -> Answer {
    let body = match csrf {
        true => format!("{fields}&_csrf={}", who.csrf),
        false => fields.to_owned(),
    };
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    send(app, request).await
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn queued(s: &Store, repo: RepoId, key: &str) -> JobId {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("x/y#{key}"),
        ledger_key: IssueId::parse(key).unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap()
    .id
}

async fn ended(s: &Store, repo: RepoId, key: &str, end: JobEnd) -> JobId {
    let id = queued(s, repo, key).await;
    s.end_job(id, end).await.unwrap();
    id
}

/// The last audit entry: `(kind, name, action, job)`.
async fn last_audit(s: &Store) -> (ActorKind, String, String, Option<i64>) {
    let e = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    (e.actor.kind, e.actor.name, e.action, e.job_id.map(|j| j.0))
}

/// `/jobs/<id>?done=<what>` → the job id.
fn redirected_job(location: &str, done: &str) -> JobId {
    let rest = location.strip_prefix("/jobs/").expect(location);
    let (id, query) = rest.split_once('?').expect(location);
    assert_eq!(query, format!("done={done}"), "{location}");
    JobId(id.parse().unwrap())
}

struct World {
    app: Router,
    a: RepoId,
    b: RepoId,
    maint: Browser,
    rev: Browser,
}

/// Repos A and B; a maintainer of A and a reviewer of A.
async fn world(s: &Store) -> World {
    let (a, b) = (repo(s, "repo-a").await, repo(s, "repo-b").await);
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    common::grant(s, "rev@x.test", a, RepoRole::Reviewer).await;
    World {
        app: router(s.clone(), common::auth(s)),
        a,
        b,
        maint: common::sign_in(s, "maint@x.test").await,
        rev: common::sign_in(s, "rev@x.test").await,
    }
}

#[tokio::test]
async fn a_maintainer_creates_a_job_in_their_repo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let w = world(s).await;

    // The form offers the repos they maintain, and only those.
    let jobs = page(&w.app, &w.maint, "/jobs").await;
    assert!(
        jobs.body.contains("action=\"/jobs\" method=\"post\""),
        "{}",
        jobs.body
    );
    let form = &jobs.body[jobs.body.find("method=\"post\"").unwrap()..];
    assert!(form.contains(&format!("value=\"{}\"", w.a.0)), "{form}");
    assert!(!form.contains("repo-b"), "{form}");

    let fields = format!("repo_id={}&issue_ref=x%2Fy%2312&source=github", w.a.0);
    let sent = post(&w.app, &w.maint, "/jobs", &fields, true).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    let id = redirected_job(&sent.location, "created");
    let job = s.job(id).await.unwrap().unwrap();
    assert_eq!(
        (job.repo_id, job.issue_ref.as_str(), job.source, job.status),
        (w.a, "x/y#12", JobSource::Github, JobStatus::Queued)
    );
    assert_eq!(
        last_audit(s).await,
        (
            ActorKind::User,
            "maint@x.test".into(),
            "job.create".into(),
            Some(id.0)
        )
    );
    let shown = page(&w.app, &w.maint, &sent.location).await;
    assert!(shown.body.contains("role=\"status\""), "{}", shown.body);
    db.drop().await;
}

#[tokio::test]
async fn creating_follows_the_api_s_checks() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let w = world(s).await;
    let create = |repo: RepoId, issue: &str, source: &str| {
        format!("repo_id={}&issue_ref={issue}&source={source}", repo.0)
    };

    // A reviewer is not offered the form and may not send it.
    let jobs = page(&w.app, &w.rev, "/jobs").await;
    assert!(!jobs.body.contains("action=\"/jobs\" method=\"post\""));
    let sent = post(
        &w.app,
        &w.rev,
        "/jobs",
        &create(w.a, "x%2Fy%231", "github"),
        true,
    )
    .await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN);
    // A repo they cannot see does not exist for them.
    let sent = post(
        &w.app,
        &w.maint,
        "/jobs",
        &create(w.b, "x%2Fy%231", "github"),
        true,
    )
    .await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);
    // The API's own checks: an issue ref, a known source, no review round.
    for (issue, source) in [
        ("+++", "github"),
        ("x%2Fy%231", "mail"),
        ("x%2Fy%231", "review"),
    ] {
        let sent = post(&w.app, &w.maint, "/jobs", &create(w.a, issue, source), true).await;
        assert_eq!(
            sent.status,
            StatusCode::BAD_REQUEST,
            "{issue} {source}: {}",
            sent.body
        );
    }
    // Without the session's CSRF token.
    let sent = post(
        &w.app,
        &w.maint,
        "/jobs",
        &create(w.a, "x%2Fy%231", "github"),
        false,
    )
    .await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN);
    let none = s
        .list_jobs(&Default::default(), Page::first(10))
        .await
        .unwrap();
    assert!(none.items.is_empty(), "no job was created");
    db.drop().await;
}

#[tokio::test]
async fn a_maintainer_cancels_and_retries() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let w = world(s).await;
    let job = queued(s, w.a, "a-1").await;
    let uri = format!("/jobs/{}", job.0);

    // Cancel is offered on a queued job to its maintainer, not its reviewer.
    let shown = page(&w.app, &w.maint, &uri).await;
    assert!(
        shown.body.contains(&format!("action=\"{uri}/cancel\"")),
        "{}",
        shown.body
    );
    assert!(!shown.body.contains("/retry\""));
    let as_rev = page(&w.app, &w.rev, &uri).await;
    assert!(!as_rev.body.contains("/cancel\"") && !as_rev.body.contains("/retry\""));
    let sent = post(&w.app, &w.rev, &format!("{uri}/cancel"), "", true).await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN);

    let sent = post(&w.app, &w.maint, &format!("{uri}/cancel"), "", true).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(redirected_job(&sent.location, "cancelled"), job);
    assert_eq!(
        s.job(job).await.unwrap().unwrap().status,
        JobStatus::Cancelled
    );
    assert_eq!(last_audit(s).await.2, "job.cancel");

    // An ended job cannot be cancelled again; the cancelled one is retried.
    let again = post(&w.app, &w.maint, &format!("{uri}/cancel"), "", true).await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    let shown = page(&w.app, &w.maint, &uri).await;
    assert!(
        shown.body.contains(&format!("action=\"{uri}/retry\"")),
        "{}",
        shown.body
    );
    let sent = post(&w.app, &w.maint, &format!("{uri}/retry"), "", true).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    let retry = redirected_job(&sent.location, "retried");
    let new = s.job(retry).await.unwrap().unwrap();
    assert_eq!((new.retry_of, new.status), (Some(job), JobStatus::Queued));
    assert_eq!(
        last_audit(s).await,
        (
            ActorKind::User,
            "maint@x.test".into(),
            "job.retry".into(),
            Some(retry.0)
        )
    );
    db.drop().await;
}

#[tokio::test]
async fn a_done_job_is_not_run_again() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let w = world(s).await;
    let done = ended(
        s,
        w.a,
        "a-1",
        JobEnd::Done(aegis_ledger::Outcome::StoppedSteps),
    )
    .await;
    let failed = ended(s, w.a, "a-2", JobEnd::Failed).await;

    let uri = format!("/jobs/{}", done.0);
    let shown = page(&w.app, &w.maint, &uri).await;
    assert!(!shown.body.contains("/retry\"") && !shown.body.contains("/cancel\""));
    let sent = post(&w.app, &w.maint, &format!("{uri}/retry"), "", true).await;
    assert_eq!(sent.status, StatusCode::CONFLICT, "{}", sent.body);

    let uri = format!("/jobs/{}", failed.0);
    let shown = page(&w.app, &w.maint, &uri).await;
    assert!(
        shown.body.contains(&format!("action=\"{uri}/retry\"")),
        "{}",
        shown.body
    );
    db.drop().await;
}
