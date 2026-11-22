//! #510: an admin pauses every agent from the API or the dashboard, and
//! resumes them; every page says so while paused. Not for anyone else.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{ModeName, NewRepo, Page, PauseLevel, RepoId, RepoRole, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn api(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, text) = send(app, request).await;
    (status, serde_json::from_str(&text).unwrap())
}

async fn page(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn form(app: &Router, who: &Browser, uri: &str, fields: &str) -> (StatusCode, String) {
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
        mode: ModeName::POC,
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
async fn an_admin_token_pauses_and_resumes_every_agent() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "ops").await;
    common::grant(s, "connector", a, RepoRole::Maintainer).await;
    let ops = common::token(s, "ops").await;
    let connector = common::token(s, "connector").await;
    let app = router(s.clone(), common::auth(s));

    // A maintainer of a repo is not an admin: refused, nothing changes.
    let (status, body) = api(&app, &connector, "POST", "/api/v1/admin/pause", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
    let (status, _) = api(&app, &connector, "GET", "/api/v1/admin/pause", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(s.instance_pause().await.unwrap().is_none());

    let (status, body) = api(
        &app,
        &ops,
        "POST",
        "/api/v1/admin/pause",
        json!({"level": "emergency", "reason": "provider bills wrong"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["paused"], true);
    assert_eq!(body["data"]["level"], "emergency");
    assert_eq!(body["data"]["cancelled"], 0);
    assert_eq!(
        last_action(s).await,
        ("ops".into(), "instance.pause".into())
    );
    let (_, shown) = api(&app, &ops, "GET", "/api/v1/admin/pause", json!({})).await;
    assert_eq!(shown["data"]["paused_by"], "ops");
    assert_eq!(shown["data"]["reason"], "provider bills wrong");

    // Twice, or with a level that does not exist, is refused.
    let (status, _) = api(&app, &ops, "POST", "/api/v1/admin/pause", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = api(
        &app,
        &ops,
        "POST",
        "/api/v1/admin/pause",
        json!({"level": "halt"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = api(&app, &connector, "POST", "/api/v1/admin/resume", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = api(&app, &ops, "POST", "/api/v1/admin/resume", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["paused"], false);
    assert!(s.instance_pause().await.unwrap().is_none());
    assert_eq!(
        last_action(s).await,
        ("ops".into(), "instance.resume".into())
    );
    let (status, _) = api(&app, &ops, "POST", "/api/v1/admin/resume", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    db.drop().await;
}

#[tokio::test]
async fn an_admin_pauses_from_the_dashboard_and_every_page_says_so() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));

    let (status, body) = page(&app, &admin, "/admin/pause").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Yes, pause all agents") && body.contains("Running"));
    assert!(!body.contains("role=\"alert\""), "no band while running");
    // Not for anyone else: as if the page did not exist.
    assert_eq!(
        page(&app, &maint, "/admin/pause").await.0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = form(&app, &maint, "/admin/pause", "level=intake").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(s.instance_pause().await.unwrap().is_none());

    let (status, _) = form(&app, &admin, "/admin/pause", "level=intake&reason=odd+PRs").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let pause = s.instance_pause().await.unwrap().unwrap();
    assert_eq!(pause.level, PauseLevel::Intake);
    assert_eq!(pause.reason.as_deref(), Some("odd PRs"));
    assert_eq!(
        last_action(s).await,
        ("admin@x.test".into(), "instance.pause".into())
    );

    // Every page shows the band, to everyone; only an admin gets the link.
    let (_, jobs) = page(&app, &maint, "/jobs").await;
    assert!(
        jobs.contains("role=\"alert\"")
            && jobs.contains("All agents paused")
            && jobs.contains("odd PRs"),
        "{jobs}"
    );
    assert!(!jobs.contains("href=\"/admin/pause\""));
    let (_, home) = page(&app, &admin, "/").await;
    assert!(home.contains("All agents paused") && home.contains("href=\"/admin/pause\""));

    // Raised to an emergency stop, then resumed.
    let (status, _) = form(&app, &admin, "/admin/pause", "level=emergency&reason=").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, shown) = page(&app, &admin, "/admin/pause").await;
    assert!(shown.contains("Emergency stop") && shown.contains("Yes, resume all agents"));
    let (status, _) = form(&app, &maint, "/admin/resume", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = form(&app, &admin, "/admin/resume", "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(s.instance_pause().await.unwrap().is_none());
    let (_, after) = page(&app, &maint, "/jobs").await;
    assert!(!after.contains("All agents paused"));
    db.drop().await;
}

#[tokio::test]
async fn the_command_line_pauses_and_resumes_when_no_page_answers() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let args = |a: &[&str]| a.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    let run = |a: Vec<String>| async move { aegis_server::pause_all::run(s, &a).await };

    let said = run(args(&["pause-all", "--emergency", "--reason", "injection"]))
        .await
        .unwrap();
    assert!(said.contains("emergency"), "{said}");
    let pause = s.instance_pause().await.unwrap().unwrap();
    assert_eq!(pause.level, PauseLevel::Emergency);
    assert_eq!(pause.reason.as_deref(), Some("injection"));
    let (_, action) = last_action(s).await;
    assert_eq!(action, "instance.pause");
    assert!(run(args(&["pause-all"])).await.is_err(), "paused already");
    assert!(run(args(&["pause-all", "--soon"])).await.is_err());

    let said = run(args(&["resume-all"])).await.unwrap();
    assert!(said.contains("resumed"), "{said}");
    assert!(s.instance_pause().await.unwrap().is_none());
    assert!(run(args(&["resume-all"])).await.is_err(), "not paused");
    db.drop().await;
}
