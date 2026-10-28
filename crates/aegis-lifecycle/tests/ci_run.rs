//! #250: a failed GitHub Actions run read as a CI issue, its job logs
//! fetched through GitHub's redirect without the token going along; a run
//! that passed or ran on another branch is refused.

use aegis_ledger::IssueKind;
use aegis_lifecycle::{IssueInput, LifecycleError};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

async fn github(conclusion: &str, storage: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/actions/runs/77"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 77, "name": "test", "html_url": "https://github.com/acme/app/actions/runs/77",
            "head_branch": "main", "head_sha": "0123456789abcdef", "conclusion": conclusion
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/actions/runs/77/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "jobs": [
            { "id": 1, "name": "go", "html_url": "https://github.com/acme/app/actions/runs/77/job/1",
              "conclusion": "failure",
              "steps": [{ "name": "checkout", "conclusion": "success" },
                        { "name": "go test ./...", "conclusion": "failure" }] },
            { "id": 2, "name": "lint", "html_url": "https://github.com/acme/app/actions/runs/77/job/2",
              "conclusion": "success", "steps": [] }
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/actions/jobs/1/logs"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{storage}/logs/1.txt")),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_failed_run_becomes_a_ci_issue_with_the_failed_jobs_log() {
    let storage = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/logs/1.txt"))
        .respond_with(|req: &Request| {
            // The token stays with GitHub.
            let leaked = req.headers.contains_key("authorization");
            ResponseTemplate::new(if leaked { 403 } else { 200 })
                .set_body_string("ok  example.com/demo/x\n--- FAIL: TestTruncate (0.00s)\n")
        })
        .mount(&storage)
        .await;
    let server = github("failure", &storage.uri()).await;
    let issue = IssueInput::from_github_run(&server.uri(), "acme", "app", 77, "main", "t")
        .await
        .unwrap();
    assert_eq!(issue.kind, Some(IssueKind::CiFailure));
    assert_eq!(issue.id, "app-run-77");
    assert!(
        issue.body.contains(
            "- `go` (https://github.com/acme/app/actions/runs/77/job/1): failed at `go test ./...`"
        ),
        "{}",
        issue.body
    );
    assert!(!issue.body.contains("`lint`"), "{}", issue.body);
    assert!(
        issue.body.contains("--- FAIL: TestTruncate"),
        "{}",
        issue.body
    );
}

#[tokio::test]
async fn a_run_that_passed_or_ran_elsewhere_is_refused() {
    let storage = MockServer::start().await;
    let passed = github("success", &storage.uri()).await;
    let err = IssueInput::from_github_run(&passed.uri(), "acme", "app", 77, "main", "t")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, LifecycleError::Issue(m) if m.contains("did not fail")),
        "{err}"
    );
    let failed = github("failure", &storage.uri()).await;
    let err = IssueInput::from_github_run(&failed.uri(), "acme", "app", 77, "develop", "t")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, LifecycleError::Issue(m) if m.contains("not on `develop`")),
        "{err}"
    );
}
