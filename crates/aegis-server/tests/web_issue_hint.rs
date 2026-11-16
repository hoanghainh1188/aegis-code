//! #564: the jobs page's form takes an issue of the chosen repo by its
//! number, offers the repo's open issues, and names the issue entered
//! before the job is created; GitHub is played by a mock server, read with
//! the repo's own token.

mod common;

use std::path::PathBuf;

use aegis_ledger::IssueId;
use aegis_lifecycle::{ModeConfig, Modes};
use aegis_secrets::Secrets;
use aegis_server::api::router_with;
use aegis_server::web::RepoAdmin;
use aegis_store::testing::test_db;
use aegis_store::{JobFilter, JobSource, NewJob, NewRepo, Page, RepoId, RepoMode, RepoRole, Store};
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
const TOKEN: &str = "fork-token-value-do-not-show";

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String, Option<String>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|l| l.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap(), location)
}

async fn get(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(app, request).await;
    (status, body)
}

async fn post(
    app: &Router,
    who: &Browser,
    uri: &str,
    fields: &str,
) -> (StatusCode, String, Option<String>) {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("_csrf={}&{fields}", who.csrf)))
        .unwrap();
    send(app, request).await
}

/// `acme/shop` on GitHub, read through its fork's token on `api`.
async fn repo(s: &Store, api: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: "shop".into(),
        url: "https://github.com/acme/shop".into(),
        default_branch: "main".into(),
        language: "python".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({ "fork": {
            "owner": "aegis-bot", "name": "shop", "api": api, "token_env": "AEGIS_TEST_FORK_TOKEN"
        } }),
    })
    .await
    .unwrap()
    .id
}

fn app(s: &Store, api: &str) -> Router {
    let mut poc = ModeConfig::from_toml(POC).unwrap();
    poc.run.state_dir = PathBuf::from("/nonexistent/aegis-test-state");
    let admin = RepoAdmin::new(
        Modes::new(Some(poc), None).unwrap(),
        Secrets::fixed([("AEGIS_TEST_FORK_TOKEN", TOKEN)]),
    )
    .with_apis(api, api);
    router_with(s.clone(), common::auth(s), Some(admin))
}

async fn github() -> MockServer {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues"))
        .and(has_header(
            "authorization",
            format!("Bearer {TOKEN}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"number": 12, "title": "Cart accepts a fourth item"},
            {"number": 11, "title": "Bump serde", "pull_request": {"url": "x"}},
            {"number": 7, "title": "Totals round down"},
        ])))
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 12,
            "title": "Cart accepts a fourth item",
            "body": "Steps…",
            "html_url": "https://github.com/acme/shop/issues/12",
        })))
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues/99"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&github)
        .await;
    github
}

#[tokio::test]
async fn an_issue_is_picked_by_its_number_on_the_chosen_repo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let github = github().await;
    let shop = repo(s, &github.uri()).await;
    common::grant(s, "maint@x.test", shop, RepoRole::Maintainer).await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = app(s, &github.uri());
    // Issue 7 has a job already.
    s.create_job(&NewJob {
        repo_id: shop,
        issue_ref: "acme/shop#7".into(),
        ledger_key: IssueId::parse("shop-7").unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
    .unwrap();

    // The form picks this repo's issues by default and offers their list.
    let (status, page) = get(&app, &maint, "/jobs").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains(
            "<option value=\"repo\" selected>This repo&#39;s issues (12 or #12)</option>"
        ) || page.contains(
            "<option value=\"repo\" selected>This repo&#x27;s issues (12 or #12)</option>"
        ),
        "{page}"
    );
    assert!(page.contains("list=\"issue-choices\""), "{page}");
    assert!(
        !page.contains("hx-include=\"closest form\""),
        "no CSRF token in a GET"
    );

    let (status, offered) = get(
        &app,
        &maint,
        &format!("/jobs/issue-choices?repo_id={}", shop.0),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        offered.contains("<option value=\"#12\">Cart accepts a fourth item</option>"),
        "{offered}"
    );
    assert!(
        offered.contains("<option value=\"#7\">Totals round down (has a job)</option>"),
        "{offered}"
    );
    assert!(
        !offered.contains("Bump serde"),
        "pull requests are not issues: {offered}"
    );
    assert!(!offered.contains(TOKEN));

    // The preview names the issue, or says why it cannot.
    let preview = |r: &str| {
        format!(
            "/jobs/issue-preview?repo_id={}&source=repo&issue_ref={r}",
            shop.0
        )
    };
    let (_, named) = get(&app, &maint, &preview("%2312")).await;
    assert!(
        named.contains("acme/shop#12 · Cart accepts a fourth item"),
        "{named}"
    );
    let (_, missing) = get(&app, &maint, &preview("99")).await;
    assert!(
        missing.contains("acme/shop#99 could not be read") && missing.contains("404"),
        "{missing}"
    );
    assert!(!missing.contains(TOKEN));
    let (_, wrong) = get(&app, &maint, &preview("twelve")).await;
    assert!(
        wrong.contains("enter the issue&#39;s number")
            || wrong.contains("enter the issue&#x27;s number"),
        "{wrong}"
    );

    // `#12` makes a job of acme/shop#12 on GitHub.
    let (status, body, location) = post(
        &app,
        &maint,
        "/jobs",
        &format!("repo_id={}&source=repo&issue_ref=%2312", shop.0),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert!(location.unwrap().ends_with("?done=created"));
    let jobs = s
        .list_jobs(
            &JobFilter {
                repo_id: Some(shop),
                status: None,
            },
            Page::first(10),
        )
        .await
        .unwrap()
        .items;
    let made = jobs
        .iter()
        .find(|j| j.issue_ref == "acme/shop#12")
        .expect("the job");
    assert_eq!(made.source, JobSource::Github);

    // Not a number: refused, saying how to write it.
    let (status, body, _) = post(
        &app,
        &maint,
        "/jobs",
        &format!("repo_id={}&source=repo&issue_ref=twelve", shop.0),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("12 or #12"), "{body}");
    db.drop().await;
}

#[tokio::test]
async fn the_hints_say_nothing_of_a_repo_the_viewer_may_not_create_jobs_in() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let github = github().await;
    let shop = repo(s, &github.uri()).await;
    common::grant(s, "rev@x.test", shop, RepoRole::Reviewer).await;
    let rev = common::sign_in(s, "rev@x.test").await;
    let app = app(s, &github.uri());
    let (status, offered) = get(
        &app,
        &rev,
        &format!("/jobs/issue-choices?repo_id={}", shop.0),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!offered.contains("Cart"), "{offered}");
    let (_, named) = get(
        &app,
        &rev,
        &format!(
            "/jobs/issue-preview?repo_id={}&source=repo&issue_ref=12",
            shop.0
        ),
    )
    .await;
    assert!(!named.contains("Cart"), "{named}");
    assert!(
        github.received_requests().await.unwrap().is_empty(),
        "GitHub not asked"
    );
    db.drop().await;
}
