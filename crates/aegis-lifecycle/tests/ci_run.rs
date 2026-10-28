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

/// #252: a GitLab issue read through the API of the fork's instance.
#[tokio::test]
async fn a_gitlab_issue_is_read_from_its_project() {
    let gitlab = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/acme%2Fplatform%2Fapp/issues/12"))
        .and(header("PRIVATE-TOKEN", "t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "iid": 12, "title": "Truncate splits runes", "description": "It cuts bytes.",
            "web_url": "https://gitlab.example.com/acme/platform/app/-/issues/12"
        })))
        .mount(&gitlab)
        .await;
    let api = format!("{}/api/v4", gitlab.uri());
    let issue = IssueInput::from_gitlab(&api, "acme/platform/app", 12, "t")
        .await
        .unwrap();
    assert_eq!(
        (issue.id.as_str(), issue.title.as_str(), issue.body.as_str()),
        ("app-12", "Truncate splits runes", "It cuts bytes.")
    );
    let missing = IssueInput::from_gitlab(&api, "acme/platform/app", 13, "t").await;
    assert!(
        matches!(
            missing,
            Err(LifecycleError::Pr(aegis_pr::PrError::GitLab {
                status: 404,
                ..
            }))
        ),
        "{missing:?}"
    );
}

/// #253: a failed GitLab pipeline read as a CI issue: its failed jobs, by
/// stage, and the end of their traces; one that passed is refused.
#[tokio::test]
async fn a_failed_gitlab_pipeline_becomes_a_ci_issue() {
    let gitlab = MockServer::start().await;
    let pipeline = |status: &str| {
        json!({ "id": 9, "status": status, "ref": "main", "sha": "abcdef0123456789",
                "web_url": "https://gitlab.example.com/acme/platform/app/-/pipelines/9" })
    };
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/acme%2Fplatform%2Fapp/pipelines/9"))
        .and(header("PRIVATE-TOKEN", "t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pipeline("failed")))
        .mount(&gitlab)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v4/projects/acme%2Fplatform%2Fapp/pipelines/9/jobs",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 31, "name": "go-test", "stage": "test",
              "web_url": "https://gitlab.example.com/acme/platform/app/-/jobs/31" }
        ])))
        .mount(&gitlab)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/acme%2Fplatform%2Fapp/jobs/31/trace"))
        .respond_with(ResponseTemplate::new(200).set_body_string("--- FAIL: TestTruncate\nFAIL\n"))
        .mount(&gitlab)
        .await;
    let api = format!("{}/api/v4", gitlab.uri());
    let issue = IssueInput::from_gitlab_pipeline(&api, "acme/platform/app", 9, "main", "t")
        .await
        .unwrap();
    assert_eq!(issue.id, "app-pipeline-9");
    assert_eq!(issue.title, "CI failed: pipeline on main");
    assert_eq!(issue.kind, Some(IssueKind::CiFailure));
    assert!(
        issue.body.contains(
            "- `go-test` (https://gitlab.example.com/acme/platform/app/-/jobs/31): failed at `test`"
        ),
        "{}",
        issue.body
    );
    assert!(
        issue.body.contains("--- FAIL: TestTruncate"),
        "{}",
        issue.body
    );

    let passed = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/acme%2Fplatform%2Fapp/pipelines/9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pipeline("success")))
        .mount(&passed)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v4/projects/acme%2Fplatform%2Fapp/pipelines/9/jobs",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&passed)
        .await;
    let api = format!("{}/api/v4", passed.uri());
    let err = IssueInput::from_gitlab_pipeline(&api, "acme/platform/app", 9, "main", "t")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, LifecycleError::Issue(m) if m.contains("pipeline 9 did not fail")),
        "{err}"
    );
}
