//! #374: an admin pauses and resumes a repo from its page, through the
//! store's `pause_repo_by` / `resume_repo_by` as `aegis-server repo
//! pause|resume` does, audited as the admin. The list of repos says which
//! are paused. Not for anyone else.

mod common;

use aegis_ledger::IssueId;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{JobSource, JobStatus, NewJob, NewRepo, Page, RepoId, RepoMode, RepoRole, Store};
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

async fn get(app: &Router, who: &Browser, uri: &str) -> Answer {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn post(app: &Router, who: &Browser, uri: &str, fields: &str) -> Answer {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("{fields}&_csrf={}", who.csrf)))
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
        max_concurrent_jobs: 2,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn last_action(s: &Store) -> (String, String) {
    let e = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    (e.actor.name, e.action)
}

#[tokio::test]
async fn an_admin_pauses_and_resumes_a_repo_from_its_page() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    let queued = s
        .create_job(&NewJob {
            repo_id: a,
            issue_ref: "x/y#1".into(),
            ledger_key: IssueId::parse("a-1").unwrap(),
            source: JobSource::Github,
        })
        .await
        .unwrap();
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/admin/repos/{}", a.0);

    let shown = get(&app, &admin, &page).await;
    assert!(
        shown.body.contains(&format!(r#"action="{page}/pause""#)) && shown.body.contains("Running"),
        "{}",
        shown.body
    );

    let sent = post(
        &app,
        &admin,
        &format!("{page}/pause"),
        "reason=too+many+PRs",
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("{page}?done=paused"));
    assert_eq!(
        last_action(s).await,
        ("admin@x.test".into(), "repo.pause".into())
    );
    let shown = get(&app, &admin, &sent.location).await;
    assert!(
        shown.body.contains("Paused")
            && shown.body.contains("too many PRs")
            && shown.body.contains("admin@x.test")
            && shown.body.contains(&format!(r#"action="{page}/resume""#))
            && shown.body.contains(r#"role="status""#),
        "{}",
        shown.body
    );
    let list = get(&app, &admin, "/admin/repos").await;
    assert!(list.body.contains("Paused"), "{}", list.body);
    // The job waits; it is not lost.
    assert_eq!(
        s.job(queued.id).await.unwrap().unwrap().status,
        JobStatus::Queued
    );

    // Twice is said plainly.
    let again = post(&app, &admin, &format!("{page}/pause"), "reason=").await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert!(again.body.contains("paused already"), "{}", again.body);

    let sent = post(&app, &admin, &format!("{page}/resume"), "").await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("{page}?done=resumed"));
    assert_eq!(
        last_action(s).await,
        ("admin@x.test".into(), "repo.resume".into())
    );
    assert!(s.repo_pause(a).await.unwrap().is_none());

    // With the queued jobs cancelled at once.
    let sent = post(
        &app,
        &admin,
        &format!("{page}/pause"),
        "reason=&cancel_queued=on",
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(
        s.job(queued.id).await.unwrap().unwrap().status,
        JobStatus::Cancelled
    );

    // Not for anyone else, page or form.
    for uri in [format!("{page}/pause"), format!("{page}/resume")] {
        assert_eq!(
            post(&app, &maint, &uri, "").await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    db.drop().await;
}
