//! #564: the open issues of a GitHub repo, for the jobs page to offer, read
//! through the REST API with the repo's token; pull requests, which the
//! same endpoint lists, are left out.

use aegis_lifecycle::{IssueSummary, open_github_issues};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn open_issues_are_listed_without_the_pull_requests() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues"))
        .and(query_param("state", "open"))
        .and(query_param("sort", "updated"))
        .and(query_param("per_page", "30"))
        .and(header("authorization", "Bearer t0ken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"number": 12, "title": "Cart accepts a fourth item"},
            {"number": 11, "title": "Bump serde", "pull_request": {"url": "x"}},
            {"number": 7, "title": "Totals round down"},
        ])))
        .expect(1)
        .mount(&github)
        .await;
    let issues = open_github_issues(&github.uri(), "acme", "shop", Some("t0ken"), 30)
        .await
        .unwrap();
    assert_eq!(
        issues,
        [
            IssueSummary {
                number: 12,
                title: "Cart accepts a fourth item".into()
            },
            IssueSummary {
                number: 7,
                title: "Totals round down".into()
            },
        ]
    );
}

#[tokio::test]
async fn a_refusal_names_the_repo_and_the_status_never_the_token() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&github)
        .await;
    let err = open_github_issues(&github.uri(), "acme", "shop", Some("t0ken"), 30)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("acme/shop") && err.contains("404"), "{err}");
    assert!(!err.contains("t0ken"), "{err}");
}
