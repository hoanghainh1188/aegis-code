//! #370: an admin signs a person out everywhere from their page, as
//! `aegis-server revoke-sessions` does, their personal tokens revoked too
//! when asked; it takes effect at once, and is audited as the admin. An
//! admin who signs themself out lands on the signed-out page.

mod common;

use aegis_server::api::router;
use aegis_server::auth::{random_token, token_hash};
use aegis_store::testing::test_db;
use aegis_store::{Actor, Page, RepoRole, Store, UserId};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
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

/// `GET /api/v1/me` with a bearer `token`.
async fn api_me(app: &Router, token: &str) -> StatusCode {
    let request = Request::get("/api/v1/me")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    send(app, request).await.status
}

/// A personal token of `who`, made as `/me/tokens` makes one.
async fn personal_token(s: &Store, who: UserId) -> String {
    let token = random_token().unwrap();
    s.create_personal_token(who, "laptop", 7, &token_hash(&token), &Actor::cli())
        .await
        .unwrap();
    token
}

/// The page's way to sign in again: the guard sends a browser there.
fn signed_out(a: &Answer) -> bool {
    a.status == StatusCode::SEE_OTHER && a.location.starts_with("/auth/login")
}

#[tokio::test]
async fn an_admin_signs_a_person_out_everywhere_and_revokes_their_tokens() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let dev = common::sign_in(s, "dev@x.test").await;
    let dev_laptop = common::sign_in(s, "dev@x.test").await;
    let token = personal_token(s, dev.user).await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/admin/users/{}", dev.user.0);

    let shown = get(&app, &admin, &page).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    assert!(shown.body.contains("2 live sessions"), "{}", shown.body);
    assert!(shown.body.contains("Sign out everywhere"), "{}", shown.body);
    // Never a session's token, nor its hash.
    for secret in [&dev.cookie, &dev_laptop.cookie] {
        let value = secret.split('=').nth(1).unwrap();
        assert!(!shown.body.contains(value), "{}", shown.body);
    }
    assert_eq!(api_me(&app, &token).await, StatusCode::OK);

    let sent = post(&app, &admin, &format!("{page}/sign-out"), "tokens=on").await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("{page}?done=signed_out"));
    // At once, on every device and for the CLI.
    assert!(signed_out(&get(&app, &dev, "/jobs").await));
    assert!(signed_out(&get(&app, &dev_laptop, "/jobs").await));
    assert_eq!(api_me(&app, &token).await, StatusCode::UNAUTHORIZED);
    let actions: Vec<(String, String)> = s
        .audit_log(None, Page::first(2))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|e| (e.action, e.actor.name))
        .collect();
    assert_eq!(
        actions,
        [
            ("token.revoke".to_owned(), "admin@x.test".to_owned()),
            ("session.revoke".to_owned(), "admin@x.test".to_owned())
        ]
    );
    let after = get(&app, &admin, &sent.location).await;
    assert!(
        after.body.contains("role=\"status\"") && after.body.contains("No live session"),
        "{}",
        after.body
    );
    db.drop().await;
}

#[tokio::test]
async fn tokens_stay_unless_asked_and_only_an_admin_signs_others_out() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = s
        .create_repo(&aegis_store::NewRepo {
            slug: "repo-a".into(),
            url: "https://github.com/x/a".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: aegis_store::RepoMode::Poc,
            max_concurrent_jobs: 1,
            config: serde_json::json!({}),
        })
        .await
        .unwrap()
        .id;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", repo, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let dev = common::sign_in(s, "dev@x.test").await;
    let token = personal_token(s, dev.user).await;
    let app = router(s.clone(), common::auth(s));
    let uri = format!("/admin/users/{}/sign-out", dev.user.0);

    // Anyone else: the form does not exist.
    assert_eq!(
        post(&app, &maint, &uri, "x=1").await.status,
        StatusCode::NOT_FOUND
    );
    assert!(!signed_out(&get(&app, &dev, "/jobs").await));

    let sent = post(&app, &admin, &uri, "x=1").await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert!(signed_out(&get(&app, &dev, "/jobs").await));
    assert_eq!(
        api_me(&app, &token).await,
        StatusCode::OK,
        "the CLI keeps working"
    );

    // An admin signing themself out lands on the signed-out page.
    let own = post(
        &app,
        &admin,
        &format!("/admin/users/{}/sign-out", admin.user.0),
        "x=1",
    )
    .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER, "{}", own.body);
    assert_eq!(own.location, "/auth/signed-out");
    assert!(signed_out(&get(&app, &admin, "/admin/users").await));
    db.drop().await;
}
