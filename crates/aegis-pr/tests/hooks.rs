//! Webhooks made from the admin's connection page (#376): one pointing at
//! the server's payload URL is created, or the one already there updated,
//! with the events asked for and the secret given; a token without the
//! right to manage webhooks is refused plainly.

use aegis_pr::{GitHub, GitLab, HookDone, PrError};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const URL: &str = "https://aegis.example/webhooks/github";
const SECRET: &str = "s3cret-value";
const TOKEN: &str = "admin-token";

#[tokio::test]
async fn github_creates_a_hook_when_none_points_at_the_server() {
    let forge = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/hooks"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 7, "config": { "url": "https://elsewhere.example/hook" } }
        ])))
        .expect(1)
        .mount(&forge)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/hooks"))
        .and(body_partial_json(json!({
            "name": "web", "active": true, "events": ["issues", "workflow_run"],
            "config": { "url": URL, "content_type": "json", "secret": SECRET, "insecure_ssl": "0" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 42 })))
        .expect(1)
        .mount(&forge)
        .await;
    let gh = GitHub::new(&forge.uri(), "acme", "app", TOKEN).unwrap();
    let done = gh
        .ensure_hook(URL, SECRET, &["issues", "workflow_run"])
        .await
        .unwrap();
    assert_eq!(done, HookDone::Created(42));
}

#[tokio::test]
async fn github_updates_the_hook_already_pointing_at_the_server() {
    let forge = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/hooks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 9, "config": { "url": format!("{URL}/") } }
        ])))
        .mount(&forge)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/app/hooks/9"))
        .and(body_partial_json(
            json!({ "events": ["pull_request_review"], "config": { "secret": SECRET } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 9 })))
        .expect(1)
        .mount(&forge)
        .await;
    let gh = GitHub::new(&forge.uri(), "acme", "app", TOKEN).unwrap();
    let done = gh
        .ensure_hook(URL, SECRET, &["pull_request_review"])
        .await
        .unwrap();
    assert_eq!(done, HookDone::Updated(9));
}

#[tokio::test]
async fn a_token_that_may_not_manage_hooks_is_refused_plainly() {
    let forge = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/hooks"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
        .mount(&forge)
        .await;
    let gh = GitHub::new(&forge.uri(), "acme", "app", TOKEN).unwrap();
    let err = gh.ensure_hook(URL, SECRET, &["issues"]).await.unwrap_err();
    assert!(
        matches!(err, PrError::GitHub { status: 404, .. }),
        "{err:?}"
    );
    let said = err.to_string();
    assert!(!said.contains(TOKEN) && !said.contains(SECRET), "{said}");
}

#[tokio::test]
async fn gitlab_creates_or_updates_the_projects_hook() {
    let forge = MockServer::start().await;
    let url = "https://aegis.example/webhooks/gitlab";
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/hooks"))
        .and(header("private-token", TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&forge)
        .await;
    Mock::given(method("POST"))
        .and(path("/projects/acme%2Fapp/hooks"))
        .and(body_partial_json(json!({
            "url": url, "token": SECRET, "issues_events": true, "pipeline_events": true,
            "push_events": false, "note_events": false, "enable_ssl_verification": true
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 5 })))
        .expect(1)
        .mount(&forge)
        .await;
    let gl = GitLab::new(&forge.uri(), "acme/app", TOKEN).unwrap();
    let done = gl
        .ensure_hook(url, SECRET, &["issues_events", "pipeline_events"])
        .await
        .unwrap();
    assert_eq!(done, HookDone::Created(5));

    let forge = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/hooks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 3, "url": url }])))
        .mount(&forge)
        .await;
    Mock::given(method("PUT"))
        .and(path("/projects/acme%2Fapp/hooks/3"))
        .and(body_partial_json(
            json!({ "note_events": true, "issues_events": false }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 3 })))
        .expect(1)
        .mount(&forge)
        .await;
    let gl = GitLab::new(&forge.uri(), "acme/app", TOKEN).unwrap();
    let done = gl.ensure_hook(url, SECRET, &["note_events"]).await.unwrap();
    assert_eq!(done, HookDone::Updated(3));
}
