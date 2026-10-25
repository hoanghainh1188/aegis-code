//! The poll's Web API calls (#27) against a mock SonarQube Server and
//! SonarQube Cloud.

use aegis_sonar_batch::{BatchError, PollConfig, Severity, SonarServer};
use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn issue(key: &str, severity: &str) -> Value {
    json!({
        "key": key, "rule": "rust:S1481", "severity": severity,
        "component": format!("app:src/{key}.rs"), "line": 3, "message": "unused",
        "textRange": {"startLine": 3, "endLine": 3}, "flows": []
    })
}

fn page(issues: Vec<Value>, total: usize) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "paging": {"pageIndex": 1, "pageSize": 100, "total": total},
        "issues": issues
    }))
}

#[tokio::test]
async fn server_new_warnings_are_the_main_branch_s_at_or_above_the_severity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/search"))
        .and(bearer_token("tok"))
        .and(query_param("componentKeys", "app"))
        .and(query_param("resolved", "false"))
        .and(query_param("severities", "CRITICAL,BLOCKER"))
        .and(query_param("s", "CREATION_DATE"))
        .and(query_param("asc", "false"))
        .and(query_param_is_missing("branch"))
        .and(query_param_is_missing("organization"))
        .respond_with(page(
            vec![
                issue("A", "BLOCKER"),
                // A server that ignores `severities` does not widen it.
                issue("B", "MINOR"),
                issue("C", "CRITICAL"),
            ],
            3,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let sonar = SonarServer::new(&server.uri(), "tok", None).unwrap();
    let poll = PollConfig {
        min_severity: Severity::Critical,
        ..PollConfig::default()
    };
    let found = sonar.new_warnings("app", &poll).await.unwrap();
    let keys: Vec<&str> = found.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, ["A", "C"]);
}

#[tokio::test]
async fn cloud_is_asked_with_its_organization_and_pages_up_to_the_limit() {
    let server = MockServer::start().await;
    let first: Vec<Value> = (0..100).map(|i| issue(&format!("K{i}"), "MAJOR")).collect();
    Mock::given(method("GET"))
        .and(path("/api/issues/search"))
        .and(query_param("organization", "acme"))
        .and(query_param("p", "1"))
        .respond_with(page(first, 250))
        // Once per poll: neither needs a second page.
        .expect(2)
        .mount(&server)
        .await;
    let sonar = SonarServer::new(&server.uri(), "tok", Some("acme")).unwrap();
    let poll = PollConfig {
        max_warnings: 30,
        ..PollConfig::default()
    };
    let found = sonar.new_warnings("app", &poll).await.unwrap();
    assert_eq!(found.len(), 30);
    assert_eq!(found[0].key, "K0");

    let poll = PollConfig {
        max_warnings: 100,
        ..PollConfig::default()
    };
    assert_eq!(sonar.new_warnings("app", &poll).await.unwrap().len(), 100);
}

#[tokio::test]
async fn warnings_are_read_again_by_key_and_only_the_open_ones_return() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/search"))
        .and(query_param("issues", "A,B"))
        .and(query_param("componentKeys", "app"))
        .and(query_param("resolved", "false"))
        .respond_with(page(
            vec![
                issue("A", "MAJOR"),
                json!({"key": "X", "rule": "r", "component": "other:src/x.rs", "message": "m"}),
            ],
            2,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let sonar = SonarServer::new(&server.uri(), "tok", None).unwrap();
    let found = sonar
        .warnings("app", &["A".into(), "B".into()])
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key, "A");
    assert!(sonar.warnings("app", &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn errors_say_what_the_server_said_without_the_token() {
    let server = MockServer::start().await;
    Mock::given(path("/api/issues/search"))
        .and(query_param("componentKeys", "gone"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"errors": [{"msg": "Project 'gone' not found"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/issues/search"))
        .and(query_param("componentKeys", "app"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let sonar = SonarServer::new(&server.uri(), "secret-tok", None).unwrap();
    let poll = PollConfig::default();
    let err = sonar.new_warnings("gone", &poll).await.unwrap_err();
    assert!(
        matches!(&err, BatchError::Sonar { status: 404, message } if message.contains("not found")),
        "{err}"
    );
    let err = sonar.new_warnings("app", &poll).await.unwrap_err();
    assert!(
        matches!(err, BatchError::Sonar { status: 401, .. }),
        "{err}"
    );
    assert!(!err.to_string().contains("secret-tok"));
}
