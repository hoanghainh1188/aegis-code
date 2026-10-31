//! #329: an admin sees who has which role in each repo and changes it from
//! the pages, through the store's `set_repo_role` like `aegis-server
//! grant|revoke`, audited as the admin. `/admin/*` does not exist for
//! anyone else.

mod common;

use aegis_ledger::IssueId;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{ActorKind, JobSource, NewJob, NewRepo, Page, RepoId, RepoMode, RepoRole, Store};
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

/// The last audit entry: `(kind, name, action, repo)`.
async fn last_audit(s: &Store) -> (ActorKind, String, String, Option<i64>) {
    let e = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    (e.actor.kind, e.actor.name, e.action, e.repo_id.map(|r| r.0))
}

#[tokio::test]
async fn only_an_admin_sees_admin_pages() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));

    // The masthead offers the admin pages to an admin only.
    assert!(
        get(&app, &admin, "/jobs")
            .await
            .body
            .contains("href=\"/admin/repos\"")
    );
    assert!(
        !get(&app, &maint, "/jobs")
            .await
            .body
            .contains("/admin/repos")
    );

    // Anyone else: the pages and the forms do not exist.
    for uri in ["/admin/repos".to_owned(), format!("/admin/repos/{}", a.0)] {
        assert_eq!(
            get(&app, &maint, &uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let grant = format!("/admin/repos/{}/roles", a.0);
    let sent = post(&app, &maint, &grant, "who=x%40x.test&role=viewer").await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);

    // The admin's list holds every repo, with its settings.
    let list = get(&app, &admin, "/admin/repos").await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(
        list.body.contains("repo-a")
            && list.body.contains("https://github.com/x/repo-a")
            && list
                .body
                .contains(&format!("href=\"/admin/repos/{}\"", a.0)),
        "{}",
        list.body
    );
    db.drop().await;
}

#[tokio::test]
async fn an_admin_grants_and_revokes_roles_at_once() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    s.create_job(&NewJob {
        repo_id: a,
        issue_ref: "x/y#1".into(),
        ledger_key: IssueId::parse("a-1").unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap();
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let new = common::sign_in(s, "new@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/admin/repos/{}", a.0);

    // No role: the repo's jobs are not there for them.
    assert!(!get(&app, &new, "/jobs").await.body.contains("x/y#1"));

    let sent = post(
        &app,
        &admin,
        &format!("{page}/roles"),
        "who=New%40X.test&role=reviewer",
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("{page}?done=granted"));
    assert_eq!(
        last_audit(s).await,
        (
            ActorKind::User,
            "admin@x.test".into(),
            "role.grant".into(),
            Some(a.0)
        )
    );
    let shown = get(&app, &admin, &sent.location).await;
    assert!(
        shown.body.contains("new@x.test") && shown.body.contains("reviewer"),
        "{}",
        shown.body
    );
    assert!(shown.body.contains("role=\"status\""), "{}", shown.body);
    // At once, for their next request.
    assert!(get(&app, &new, "/jobs").await.body.contains("x/y#1"));

    let sent = post(
        &app,
        &admin,
        &format!("{page}/roles/revoke"),
        "who=new%40x.test",
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(last_audit(s).await.2, "role.revoke");
    assert!(!get(&app, &new, "/jobs").await.body.contains("x/y#1"));
    assert!(!get(&app, &admin, &page).await.body.contains("new@x.test"));

    // What the store refuses, said plainly.
    for fields in ["who=&role=viewer", "who=a%40b.test&role=owner"] {
        let sent = post(&app, &admin, &format!("{page}/roles"), fields).await;
        assert_eq!(
            sent.status,
            StatusCode::BAD_REQUEST,
            "{fields}: {}",
            sent.body
        );
    }
    let missing = post(
        &app,
        &admin,
        "/admin/repos/999/roles",
        "who=a%40b.test&role=viewer",
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    db.drop().await;
}
