//! Wiremock tests for the Sentry and Datadog APM connectors (#452).

use aegis_tracker::{ApmClient, ApmConfig, DatadogConfig, SentryConfig, TelemetryQuery};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sentry_fetches_issue_by_id_with_stacktrace_and_breadcrumbs() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/0/issues/987654/"))
        .and(header("authorization", "Bearer sentry-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "987654",
            "title": "ZeroDivisionError: divided by 0",
            "culprit": "cart/total.py in compute",
            "count": "42",
            "userCount": 7,
            "permalink": "https://sentry.io/organizations/acme/issues/987654/"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/0/issues/987654/events/latest/"))
        .and(header("authorization", "Bearer sentry-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tags": [
                { "key": "commit", "value": "deadbeef" }
            ],
            "entries": [
                {
                    "type": "exception",
                    "data": {
                        "values": [{
                            "type": "ZeroDivisionError",
                            "value": "divided by 0",
                            "stacktrace": {
                                "frames": [
                                    { "filename": "cart/total.py", "function": "compute", "lineNo": 18 }
                                ]
                            }
                        }]
                    }
                },
                {
                    "type": "breadcrumbs",
                    "data": {
                        "values": [
                            { "category": "http", "level": "info", "message": "POST /checkout 500" }
                        ]
                    }
                }
            ]
        })))
        .mount(&server)
        .await;

    let cfg = ApmConfig::Sentry(SentryConfig {
        url: server.uri(),
        organization: "acme".into(),
        project: Some("shop".into()),
        token_env: "SENTRY_TOKEN".into(),
    });
    let client = ApmClient::new(&cfg, "sentry-secret", None).unwrap();
    let query = TelemetryQuery::from_ticket(
        "Checkout fails with 500",
        "Customer reported error, see SENTRY-987654",
    );
    let telemetry = client.fetch(&query).await.unwrap().unwrap();
    assert_eq!(telemetry.provider, "sentry");
    assert_eq!(telemetry.release_commit.as_deref(), Some("deadbeef"));
    assert_eq!(telemetry.frequency.as_deref(), Some("42 events, 7 users"));
    let md = telemetry.to_markdown();
    assert!(md.contains("ZeroDivisionError: divided by 0"));
    assert!(md.contains("at compute (cart/total.py:18)"));
    assert!(md.contains("[http/info] POST /checkout 500"));
}

#[tokio::test]
async fn sentry_searches_org_issues_and_returns_none_when_empty() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/issues/"))
        .and(query_param("query", "req-445566"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let cfg = ApmConfig::Sentry(SentryConfig {
        url: server.uri(),
        organization: "acme".into(),
        project: None,
        token_env: "SENTRY_TOKEN".into(),
    });
    let client = ApmClient::new(&cfg, "sentry-secret", None).unwrap();
    let query = TelemetryQuery::from_ticket("500 error", "correlation_id: req-445566");
    assert!(client.fetch(&query).await.unwrap().is_none());
}

#[tokio::test]
async fn datadog_searches_logs_and_extracts_stacktrace() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/v2/logs/events/search"))
        .and(header("dd-api-key", "dd-api"))
        .and(header("dd-application-key", "dd-app"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {
                    "attributes": {
                        "timestamp": "2026-11-15T09:00:00Z",
                        "service": "checkout",
                        "status": "error",
                        "message": "ERR_CART_LOCK failed to acquire lock",
                        "attributes": {
                            "error": { "stack": "Error: lock timeout\n  at lockCart (src/cart.ts:44)" },
                            "git": { "commit": { "sha": "cafe1234" } }
                        }
                    }
                }
            ]
        })))
        .mount(&server)
        .await;

    let cfg = ApmConfig::Datadog(DatadogConfig {
        url: server.uri(),
        token_env: "DD_API_KEY".into(),
        app_key_env: "DD_APP_KEY".into(),
        service: Some("checkout".into()),
        env: Some("prod".into()),
    });
    let client = ApmClient::new(&cfg, "dd-api", Some("dd-app")).unwrap();
    let query = TelemetryQuery::from_ticket("Checkout lock error", "code: ERR_CART_LOCK");
    let telemetry = client.fetch(&query).await.unwrap().unwrap();
    assert_eq!(telemetry.provider, "datadog");
    assert_eq!(telemetry.release_commit.as_deref(), Some("cafe1234"));
    let md = telemetry.to_markdown();
    assert!(md.contains("at lockCart (src/cart.ts:44)"));
    assert!(md.contains("[2026-11-15T09:00:00Z checkout/error]"));
}

#[test]
fn rejects_invalid_apm_configs() {
    let bad_sentry = ApmConfig::Sentry(SentryConfig {
        url: "http://sentry.example.com".into(),
        organization: "acme".into(),
        project: None,
        token_env: "SENTRY_TOKEN".into(),
    });
    assert!(bad_sentry.check().is_err());

    let missing_app_key = ApmConfig::Datadog(DatadogConfig {
        url: "https://api.datadoghq.com".into(),
        token_env: "DD_API_KEY".into(),
        app_key_env: "DD_APP_KEY".into(),
        service: None,
        env: None,
    });
    assert!(ApmClient::new(&missing_app_key, "dd-api", None).is_err());
}
