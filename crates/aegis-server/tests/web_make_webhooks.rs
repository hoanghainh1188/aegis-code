//! #376: with `[web] make_webhooks = true`, an admin makes a repo's
//! webhooks from its connection page with a token given for that once:
//! never kept, logged or audited, never in a page. The secret is the
//! server's, never through the browser. The forge is a mock server here.

mod common;

use std::path::PathBuf;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_secrets::Secrets;
use aegis_server::api::router_with;
use aegis_server::config::AuthSection;
use aegis_server::web::RepoAdmin;
use aegis_server::webhook::GITHUB_SECRET_ENV;
use aegis_store::testing::test_db;
use aegis_store::{NewRepo, Page, RepoId, RepoMode, RepoRole, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, header as has_header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const POC: &str = include_str!("../../../config/poc.toml");
const ADMIN_TOKEN: &str = "admin-token-given-once-do-not-keep";
const HOOK_SECRET: &str = "webhook-secret-value-do-not-show";
const PUBLIC: &str = "http://localhost:8080";

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
        .header(header::ORIGIN, PUBLIC)
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("{fields}&_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
}

/// `acme/app` on GitHub, its fork `aegis-bot/app`.
async fn repo(s: &Store, api: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: "app".into(),
        url: "https://github.com/acme/app".into(),
        default_branch: "main".into(),
        language: "go".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({ "fork": {
            "owner": "aegis-bot", "name": "app", "api": api, "token_env": "AEGIS_TEST_FORK_TOKEN"
        } }),
    })
    .await
    .unwrap()
    .id
}

fn app(s: &Store, secrets: Secrets, api: &str, on: bool) -> Router {
    let mut poc = ModeConfig::from_toml(POC).unwrap();
    poc.run.state_dir = PathBuf::from("/nonexistent/aegis-test-state");
    let admin = RepoAdmin::new(Modes::new(Some(poc), None).unwrap(), secrets)
        .with_apis(api, api)
        .with_webhook_making(on);
    let config: AuthSection = toml::from_str(&format!("public_url = \"{PUBLIC}\"")).unwrap();
    router_with(s.clone(), common::auth_with(s, config, None), Some(admin))
}

fn secrets() -> Secrets {
    Secrets::fixed([(GITHUB_SECRET_ENV, HOOK_SECRET)])
}

#[tokio::test]
async fn off_by_default_there_is_no_form_to_give_a_token() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let forge = MockServer::start().await;
    let id = repo(s, &forge.uri()).await;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = app(s, secrets(), &forge.uri(), false);
    let page = format!("/admin/repos/{}/connection", id.0);

    let shown = get(&app, &admin, &page).await;
    assert_eq!(shown.status, StatusCode::OK);
    assert!(!shown.body.contains(r#"name="token""#), "{}", shown.body);
    assert!(
        shown.body.contains("make_webhooks"),
        "says how to turn it on: {}",
        shown.body
    );
    let sent = post(
        &app,
        &admin,
        &format!("{page}/webhooks"),
        &format!("place=repo&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

#[tokio::test]
async fn an_admin_makes_the_repos_and_the_forks_webhooks_with_a_token_given_once() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let forge = MockServer::start().await;
    let id = repo(s, &forge.uri()).await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", id, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = app(s, secrets(), &forge.uri(), true);
    let page = format!("/admin/repos/{}/connection", id.0);
    let payload = format!("{PUBLIC}/webhooks/github");

    let shown = get(&app, &admin, &page).await;
    assert!(
        shown.body.contains(r#"type="password" name="token""#)
            && shown.body.contains(&format!(r#"action="{page}/webhooks""#)),
        "{}",
        shown.body
    );

    // The repo's own: none yet, so one is created with the server's secret.
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/hooks"))
        .and(has_header("authorization", format!("Bearer {ADMIN_TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&forge)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/hooks"))
        .and(body_partial_json(json!({
            "events": ["issues", "workflow_run"],
            "config": { "url": payload, "secret": HOOK_SECRET }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 41 })))
        .expect(1)
        .mount(&forge)
        .await;
    let sent = post(
        &app,
        &admin,
        &format!("{page}/webhooks"),
        &format!("place=repo&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("{page}?made=created"));
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(
        (entry.action.as_str(), entry.repo_id),
        ("repo.webhook", Some(id))
    );
    assert_eq!(
        entry.detail,
        json!({ "place": "repo", "hook_id": 41, "created": true })
    );
    let after = get(&app, &admin, &sent.location).await;
    assert!(after.body.contains("Webhook created"), "{}", after.body);

    // The fork's: one already points here, so it is updated.
    Mock::given(method("GET"))
        .and(path("/repos/aegis-bot/app/hooks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 8, "config": { "url": payload } }
        ])))
        .mount(&forge)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/aegis-bot/app/hooks/8"))
        .and(body_partial_json(json!({
            "events": ["pull_request_review", "pull_request_review_comment", "issue_comment"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 8 })))
        .expect(1)
        .mount(&forge)
        .await;
    let sent = post(
        &app,
        &admin,
        &format!("{page}/webhooks"),
        &format!("place=fork&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(
        sent.location,
        format!("{page}?made=updated"),
        "{}",
        sent.body
    );

    // Nothing of the token or the secret anywhere it could be read again.
    let log = s
        .audit_log(None, Page::first(Page::MAX_LIMIT))
        .await
        .unwrap();
    let all = format!(
        "{:?} {} {}",
        log.items,
        after.body,
        get(&app, &admin, &page).await.body
    );
    assert!(!all.contains(ADMIN_TOKEN) && !all.contains(HOOK_SECRET));

    // Not for anyone else.
    let sent = post(
        &app,
        &maint,
        &format!("{page}/webhooks"),
        &format!("place=repo&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

#[tokio::test]
async fn a_token_without_the_right_or_a_missing_secret_is_said_plainly() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let forge = MockServer::start().await;
    let id = repo(s, &forge.uri()).await;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let page = format!("/admin/repos/{}/connection", id.0);
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/hooks"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
        .mount(&forge)
        .await;

    let app = app(s, secrets(), &forge.uri(), true);
    let sent = post(
        &app,
        &admin,
        &format!("{page}/webhooks"),
        &format!("place=repo&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::BAD_GATEWAY, "{}", sent.body);
    assert!(sent.body.contains("admin:repo_hook"), "{}", sent.body);
    assert!(!sent.body.contains(ADMIN_TOKEN) && !sent.body.contains(HOOK_SECRET));

    let sent = post(
        &app,
        &admin,
        &format!("{page}/webhooks"),
        "place=repo&token=",
    )
    .await;
    assert_eq!(sent.status, StatusCode::BAD_REQUEST);
    let sent = post(
        &app,
        &admin,
        &format!("{page}/webhooks"),
        &format!("place=there&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::BAD_REQUEST);

    // A fork whose settings name another API root: the token is not sent
    // there, nor anywhere.
    let elsewhere = self::app(s, secrets(), "https://api.github.example", true);
    let sent = post(
        &elsewhere,
        &admin,
        &format!("{page}/webhooks"),
        &format!("place=fork&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::BAD_REQUEST, "{}", sent.body);
    assert!(
        sent.body.contains("make its webhook by hand"),
        "{}",
        sent.body
    );

    let app = self::app(s, Secrets::none(), &forge.uri(), true);
    let sent = post(
        &app,
        &admin,
        &format!("{page}/webhooks"),
        &format!("place=repo&token={ADMIN_TOKEN}"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::BAD_REQUEST);
    assert!(sent.body.contains(GITHUB_SECRET_ENV), "{}", sent.body);
    assert!(
        s.audit_log(None, Page::first(10))
            .await
            .unwrap()
            .items
            .iter()
            .all(|e| e.action != "repo.webhook")
    );
    db.drop().await;
}
