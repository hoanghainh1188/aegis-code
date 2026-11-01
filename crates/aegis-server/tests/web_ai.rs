//! #367: the admin sees each mode's AI (provider, models, budget, the
//! key's secret by name and whether the store has it) and checks the
//! connection with one small request through the provider the workers
//! use, played here by a mock server. Never the key; audited; once a
//! minute per mode.

mod common;

use std::path::PathBuf;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_provider::ProviderConfig;
use aegis_secrets::Secrets;
use aegis_server::api::{router, router_with};
use aegis_server::web::RepoAdmin;
use aegis_store::testing::test_db;
use aegis_store::{NewRepo, Page, RepoMode, RepoRole};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{header as has_header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const POC: &str = include_str!("../../../config/poc.toml");
const KEY_ENV: &str = "AEGIS_TEST_ANTHROPIC_KEY";
const KEY: &str = "sk-test-value-never-shown";

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn get(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn post(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
}

/// The poc mode on the Claude API at `base_url`, its key named
/// [`KEY_ENV`]; no `customer-jp` mode.
fn repo_admin(base_url: &str, secrets: Secrets) -> RepoAdmin {
    let mut poc = ModeConfig::from_toml(POC).unwrap();
    poc.run.state_dir = PathBuf::from("/nonexistent/aegis-test-state");
    poc.provider = ProviderConfig::ClaudeApi {
        api_key_env: KEY_ENV.into(),
        base_url: Some(base_url.into()),
    };
    RepoAdmin::new(Modes::new(Some(poc), None).unwrap(), secrets)
}

fn answer() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "model": "claude-haiku-5-5",
        "content": [{"type": "text", "text": "OK"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 12, "output_tokens": 1}
    }))
}

#[tokio::test]
async fn an_admin_sees_each_modes_ai_and_checks_it() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(has_header("x-api-key", KEY))
        .respond_with(answer())
        .expect(1)
        .mount(&provider)
        .await;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let secrets = Secrets::fixed([(KEY_ENV, KEY)]);
    let app = router_with(
        s.clone(),
        common::auth(s),
        Some(repo_admin(&provider.uri(), secrets)),
    );

    assert!(
        get(&app, &admin, "/admin/repos")
            .await
            .1
            .contains("href=\"/admin/ai\"")
    );
    let (status, page) = get(&app, &admin, "/admin/ai").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    for text in [
        "Claude API",
        &provider.uri(),
        KEY_ENV,
        ">set<",
        "claude-haiku-5-5",
        "claude-sonnet-5-5",
        "claude-opus-5-5",
        "high",
        "model calls",
        "Not configured",
    ] {
        assert!(page.contains(text), "{text}: {page}");
    }
    assert!(!page.contains(KEY), "{page}");

    let (status, page) = post(&app, &admin, "/admin/ai/poc/check").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page.contains("<li class=\"pass\">") && page.contains("answered in"),
        "{page}"
    );
    assert!(!page.contains(KEY), "{page}");
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(
        (entry.action.as_str(), entry.actor.name.as_str()),
        ("ai.check", "admin@x.test")
    );
    assert_eq!(entry.detail["mode"], "poc");
    assert_eq!(entry.detail["ok"], true);
    assert!(!entry.detail.to_string().contains(KEY));

    // At most once a minute per mode: no second request reaches the provider.
    let (status, page) = post(&app, &admin, "/admin/ai/poc/check").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{page}");
    assert!(page.contains("check again in"), "{page}");
    // A mode the server has no file for cannot be checked.
    let (status, _) = post(&app, &admin, "/admin/ai/customer-jp/check").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        post(&app, &admin, "/admin/ai/nope/check").await.0,
        StatusCode::NOT_FOUND
    );
    db.drop().await;
}

#[tokio::test]
async fn a_refused_or_missing_key_is_said_plainly() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": format!("invalid x-api-key {KEY}") }
        })))
        .mount(&provider)
        .await;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;

    let secrets = Secrets::fixed([(KEY_ENV, KEY)]);
    let app = router_with(
        s.clone(),
        common::auth(s),
        Some(repo_admin(&provider.uri(), secrets)),
    );
    let (_, page) = post(&app, &admin, "/admin/ai/poc/check").await;
    assert!(
        page.contains("<li class=\"fail\">") && page.contains("refused the key"),
        "{page}"
    );
    assert!(!page.contains(KEY), "{page}");
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(entry.detail["ok"], false);

    // No key in the store: said, and nothing is sent.
    let app = router_with(
        s.clone(),
        common::auth(s),
        Some(repo_admin(&provider.uri(), Secrets::none())),
    );
    let (_, page) = get(&app, &admin, "/admin/ai").await;
    assert!(page.contains(">missing<"), "{page}");
    let (_, page) = post(&app, &admin, "/admin/ai/poc/check").await;
    assert!(
        page.contains("<li class=\"fail\">") && page.contains(KEY_ENV),
        "{page}"
    );
    db.drop().await;
}

#[tokio::test]
async fn only_an_admin_and_only_where_modes_are_loaded() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = s
        .create_repo(&NewRepo {
            slug: "repo-a".into(),
            url: "https://github.com/x/a".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::Poc,
            max_concurrent_jobs: 1,
            config: json!({}),
        })
        .await
        .unwrap()
        .id;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", repo, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router_with(
        s.clone(),
        common::auth(s),
        Some(repo_admin("http://127.0.0.1:9", Secrets::none())),
    );
    assert_eq!(
        get(&app, &maint, "/admin/ai").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(&app, &maint, "/admin/ai/poc/check").await.0,
        StatusCode::NOT_FOUND
    );

    let bare = router(s.clone(), common::auth(s));
    assert_eq!(
        get(&bare, &admin, "/admin/ai").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    db.drop().await;
}
