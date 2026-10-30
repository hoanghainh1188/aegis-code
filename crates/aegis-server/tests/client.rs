//! The developer client's side of the server (#231): a person makes a
//! token on `/me/tokens`, shown once; with it the API acts as them, with
//! their roles, and audits them by name; revoked, it stops at once.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{NewRepo, Page, RepoMode, RepoRole, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn page(
    app: &Router,
    browser: &common::Browser,
    method: &str,
    uri: &str,
    form: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &browser.cookie);
    let body = match form {
        Some(form) => {
            request = request
                .header(header::ORIGIN, "http://localhost:8080")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            Body::from(format!("{form}&_csrf={}", browser.csrf))
        }
        None => Body::empty(),
    };
    send(app, request.body(body).unwrap()).await
}

async fn api(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, text) = send(app, request).await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn repo(s: &Store, slug: &str) -> aegis_store::RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "go".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

/// The text between `start` and `end` in `html`.
fn between<'a>(html: &'a str, start: &str, end: &str) -> &'a str {
    let from = html
        .find(start)
        .unwrap_or_else(|| panic!("no `{start}` in {html}"))
        + start.len();
    let len = html[from..].find(end).unwrap();
    &html[from..from + len]
}

#[tokio::test]
async fn a_persons_token_acts_as_them_until_they_revoke_it() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let mine = repo(s, "mine").await;
    repo(s, "theirs").await;
    common::grant(s, "lan@example.com", mine, RepoRole::Maintainer).await;
    let browser = common::sign_in(s, "lan@example.com").await;
    let app = router(s.clone(), common::auth(s));

    let (status, html) = page(
        &app,
        &browser,
        "POST",
        "/me/tokens",
        Some("label=laptop&days=30"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("shown only once"), "{html}");
    let token = between(&html, "<pre class=\"token\"><code>", "</code>").to_owned();

    let (status, me) = api(&app, &token, "GET", "/api/v1/me", None).await;
    assert_eq!(
        (status, &me["data"]["name"]),
        (StatusCode::OK, &json!("lan@example.com"))
    );
    let (_, repos) = api(&app, &token, "GET", "/api/v1/repos", None).await;
    assert_eq!(
        repos["data"],
        json!([{ "id": mine.0, "slug": "mine",
        "url": "https://github.com/acme/mine", "default_branch": "main",
        "language": "go", "role": "maintainer" }])
    );
    let new = json!({ "repo_id": mine.0, "issue_ref": "acme/mine#1", "source": "github" });
    let (status, body) = api(&app, &token, "POST", "/api/v1/jobs", Some(new)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let audit = s.audit_log(Some(mine), Page::first(5)).await.unwrap();
    let created = audit
        .items
        .iter()
        .find(|e| e.action == "job.create")
        .unwrap();
    assert_eq!(created.actor.name, "lan@example.com");

    // Listed, never shown again; revoked, refused at once.
    let (_, listed) = page(&app, &browser, "GET", "/me/tokens", None).await;
    assert!(
        listed.contains("laptop") && !listed.contains(&token),
        "{listed}"
    );
    let id = between(&listed, "action=\"/me/tokens/", "/revoke\"");
    let (status, _) = page(
        &app,
        &browser,
        "POST",
        &format!("/me/tokens/{id}/revoke"),
        Some("x=1"),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (status, _) = api(&app, &token, "GET", "/api/v1/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, html) = page(&app, &browser, "POST", "/me/tokens", Some("label=&days=30")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{html}");
    let (status, _) = page(
        &app,
        &browser,
        "POST",
        "/me/tokens",
        Some("label=x&days=365"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    db.drop().await;
}
