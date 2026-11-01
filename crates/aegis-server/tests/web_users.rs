//! #369: an admin sees people and services, their roles repo by repo, and
//! gives or takes the admin right from the pages, through the store's
//! `set_admin` like `aegis-server grant|revoke <who> admin`, audited as
//! the admin. The last admin keeps the right, and an admin does not take
//! their own. It does not exist for anyone else.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, ActorKind, NewRepo, Page, RepoId, RepoMode, RepoRole, Store, StoreError, UserKind,
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
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn id_of(s: &Store, name: &str) -> i64 {
    s.list_users(Some(name), Page::first(10))
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|u| u.user.name == name)
        .unwrap()
        .user
        .id
        .0
}

async fn is_admin(s: &Store, name: &str) -> bool {
    let id = aegis_store::UserId(id_of(s, name).await);
    s.user_summary(id).await.unwrap().unwrap().user.is_admin
}

#[tokio::test]
async fn an_admin_sees_people_and_services_and_their_roles() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    let b = repo(s, "repo-b").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "dev@x.test", a, RepoRole::Maintainer).await;
    common::grant(s, "dev@x.test", b, RepoRole::Viewer).await;
    common::grant(s, "ci-bot", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router(s.clone(), common::auth(s));

    // The admin pages lead here.
    assert!(
        get(&app, &admin, "/admin/repos")
            .await
            .body
            .contains("href=\"/admin/users\"")
    );
    let list = get(&app, &admin, "/admin/users").await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    for text in [
        "admin@x.test",
        "dev@x.test",
        "ci-bot",
        "service",
        "never signed in",
    ] {
        assert!(list.body.contains(text), "{text}: {}", list.body);
    }
    let found = get(&app, &admin, "/admin/users?q=DEV").await;
    assert!(
        found.body.contains("dev@x.test") && !found.body.contains("ci-bot"),
        "{}",
        found.body
    );

    let dev = id_of(s, "dev@x.test").await;
    let page = get(&app, &admin, &format!("/admin/users/{dev}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body
            .contains(&format!("href=\"/admin/repos/{}\"", a.0))
            && page.body.contains("maintainer")
            && page
                .body
                .contains(&format!("href=\"/admin/repos/{}\"", b.0))
            && page.body.contains("viewer"),
        "{}",
        page.body
    );
    // Their entries in the audit log are a click away.
    assert!(
        page.body.contains("/admin/audit?actor=dev%40x.test"),
        "{}",
        page.body
    );
    assert_eq!(
        get(&app, &admin, "/admin/users/999999").await.status,
        StatusCode::NOT_FOUND
    );
    db.drop().await;
}

#[tokio::test]
async fn an_admin_gives_and_takes_the_admin_right_but_not_the_last_or_their_own() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "dev@x.test", a, RepoRole::Viewer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let dev_browser = common::sign_in(s, "dev@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let dev = id_of(s, "dev@x.test").await;
    let me = id_of(s, "admin@x.test").await;

    // dev is not an admin: no admin pages for them.
    assert_eq!(
        get(&app, &dev_browser, "/admin/users").await.status,
        StatusCode::NOT_FOUND
    );
    let sent = post(
        &app,
        &admin,
        &format!("/admin/users/{dev}/admin"),
        "admin=true",
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("/admin/users/{dev}?done=granted"));
    assert!(is_admin(s, "dev@x.test").await);
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(
        (
            entry.actor.kind,
            entry.actor.name.as_str(),
            entry.action.as_str()
        ),
        (ActorKind::User, "admin@x.test", "role.grant")
    );
    assert_eq!(entry.detail["role"], "admin");
    // At once, for their next request.
    assert_eq!(
        get(&app, &dev_browser, "/admin/users").await.status,
        StatusCode::OK
    );
    let shown = get(&app, &admin, &sent.location).await;
    assert!(shown.body.contains("role=\"status\""), "{}", shown.body);

    // Not their own right, even with another admin left.
    let own = post(
        &app,
        &admin,
        &format!("/admin/users/{me}/admin"),
        "admin=false",
    )
    .await;
    assert_eq!(own.status, StatusCode::CONFLICT, "{}", own.body);
    assert!(own.body.contains("another admin"), "{}", own.body);
    assert!(is_admin(s, "admin@x.test").await);

    // dev takes admin@'s right; then dev is the last and keeps theirs.
    let taken = post(
        &app,
        &dev_browser,
        &format!("/admin/users/{me}/admin"),
        "admin=false",
    )
    .await;
    assert_eq!(taken.status, StatusCode::SEE_OTHER, "{}", taken.body);
    assert!(!is_admin(s, "admin@x.test").await);
    let last = s
        .set_admin(UserKind::Person, "dev@x.test", false, &Actor::cli())
        .await
        .unwrap_err();
    assert!(
        matches!(&last, StoreError::Conflict(m) if m.contains("last admin")),
        "{last}"
    );
    db.drop().await;
}

#[tokio::test]
async fn an_admin_names_an_admin_before_their_first_sign_in() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router(s.clone(), common::auth(s));

    let sent = post(&app, &admin, "/admin/users", "who=New.Admin%40X.test").await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert!(is_admin(s, "new.admin@x.test").await);
    let list = get(&app, &admin, "/admin/users").await;
    assert!(list.body.contains("new.admin@x.test"), "{}", list.body);

    let refused = post(&app, &admin, "/admin/users", "who=").await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    db.drop().await;
}
