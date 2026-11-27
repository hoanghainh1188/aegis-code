//! Wiremock tests for the Sentry and Datadog APM connectors (#452), and the
//! CloudWatch configuration (#704; its queries are tested in `apm/cloudwatch.rs`),
//! and the error counts per type that error spikes are found from (#706).

use aegis_tracker::{
    ApmClient, ApmConfig, CloudWatchConfig, DatadogConfig, SentryConfig, SpikeRules,
    TelemetryQuery, anomalies,
};
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
    let client = ApmClient::new(&cfg, Some("sentry-secret"), None).unwrap();
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
    let client = ApmClient::new(&cfg, Some("sentry-secret"), None).unwrap();
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
    let client = ApmClient::new(&cfg, Some("dd-api"), Some("dd-app")).unwrap();
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
    assert!(ApmClient::new(&missing_app_key, Some("dd-api"), None).is_err());
}

#[test]
fn cloudwatch_is_configured_without_a_secret() {
    let config: ApmConfig = serde_json::from_value(serde_json::json!({
        "kind": "cloudwatch",
        "region": "ap-northeast-1",
        "log_groups": ["/ecs/checkout", "/ecs/cart"]
    }))
    .unwrap();
    let ApmConfig::CloudWatch(cw) = &config else {
        panic!("{config:?}");
    };
    assert_eq!(cw.lookback_minutes, 24 * 60);
    assert_eq!(cw.profile, None);
    assert_eq!(config.token_env(), None);
    assert_eq!(config.app_key_env(), None);
    assert!(ApmClient::new(&config, None, None).is_ok());

    // A key in the settings is refused: credentials come from the AWS chain.
    let with_key = serde_json::from_value::<ApmConfig>(serde_json::json!({
        "kind": "cloudwatch",
        "region": "ap-northeast-1",
        "log_groups": ["/ecs/checkout"],
        "access_key_id": "AKIA0000"
    }));
    assert!(with_key.is_err());
}

#[test]
fn rejects_invalid_cloudwatch_configs() {
    let valid = CloudWatchConfig {
        region: "ap-northeast-1".into(),
        log_groups: vec!["/ecs/checkout".into()],
        profile: Some("aegis-readonly".into()),
        lookback_minutes: 60,
    };
    assert!(ApmConfig::CloudWatch(valid.clone()).check().is_ok());
    let bad = [
        CloudWatchConfig {
            region: "Tokyo".into(),
            ..valid.clone()
        },
        CloudWatchConfig {
            log_groups: vec![],
            ..valid.clone()
        },
        CloudWatchConfig {
            log_groups: vec![" ".into()],
            ..valid.clone()
        },
        CloudWatchConfig {
            log_groups: vec!["/g".into(); 51],
            ..valid.clone()
        },
        CloudWatchConfig {
            profile: Some(String::new()),
            ..valid.clone()
        },
        CloudWatchConfig {
            lookback_minutes: 0,
            ..valid.clone()
        },
        CloudWatchConfig {
            lookback_minutes: 14 * 24 * 60 + 1,
            ..valid
        },
    ];
    for config in bad {
        let config = ApmConfig::CloudWatch(config);
        assert!(config.check().is_err(), "{config:?}");
    }
}

#[test]
fn sentry_and_datadog_still_need_their_token() {
    let sentry = ApmConfig::Sentry(SentryConfig {
        url: "https://sentry.io".into(),
        organization: "acme".into(),
        project: None,
        token_env: "SENTRY_TOKEN".into(),
    });
    assert_eq!(sentry.token_env(), Some("SENTRY_TOKEN"));
    assert!(ApmClient::new(&sentry, None, None).is_err());
    assert!(ApmClient::new(&sentry, Some(""), None).is_err());
}

fn sentry(server: &MockServer) -> ApmClient {
    let cfg = ApmConfig::Sentry(SentryConfig {
        url: server.uri(),
        organization: "acme".into(),
        project: Some("checkout".into()),
        token_env: "SENTRY_TOKEN".into(),
    });
    ApmClient::new(&cfg, Some("sentry-secret"), None).unwrap()
}

fn discover(rows: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": rows, "meta": {} }))
}

#[tokio::test]
async fn sentry_counts_errors_per_issue_and_a_tenfold_spike_after_a_release_is_one_anomaly() {
    let server = MockServer::start().await;
    let (from, to) = (
        time::macros::datetime!(2026-11-20 09:00 UTC),
        time::macros::datetime!(2026-11-20 10:00 UTC),
    );
    let (base_from, base_to) = (from - time::Duration::days(7), to - time::Duration::days(7));
    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/events/"))
        .and(header("authorization", "Bearer sentry-secret"))
        .and(query_param("query", "event.type:error project:checkout"))
        .and(query_param("start", "2026-11-20T09:00:00Z"))
        .and(query_param("end", "2026-11-20T10:00:00Z"))
        .respond_with(discover(serde_json::json!([
            {"issue": "CHECKOUT-1A", "title": "TypeError: total is undefined", "count()": 240},
            {"issue": "CHECKOUT-7", "title": "TimeoutError", "count()": 33}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/events/"))
        .and(query_param("start", "2026-11-13T09:00:00Z"))
        .respond_with(discover(serde_json::json!([
            {"issue": "CHECKOUT-1A", "title": "TypeError: total is undefined", "count()": 24},
            {"issue": "CHECKOUT-7", "title": "TimeoutError", "count()": 30}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/releases/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "version": "checkout@1.4.0",
                "dateCreated": "2026-11-20T09:05:00Z",
                "dateReleased": "2026-11-20T09:10:00Z",
                "lastCommit": {"id": "cafe1234"}
            }])),
        )
        .mount(&server)
        .await;

    let client = sentry(&server);
    assert!(client.counts_errors());
    let current = client.error_counts(from, to).await.unwrap();
    assert_eq!(current[0].fingerprint, "sentry:CHECKOUT-1A");
    assert_eq!(current[0].count, 240);
    let baseline = client.error_counts(base_from, base_to).await.unwrap();
    let release = client.latest_release().await.unwrap().unwrap();
    assert_eq!(release.commit.as_deref(), Some("cafe1234"));
    assert_eq!(release.at, time::macros::datetime!(2026-11-20 09:10 UTC));

    let found = anomalies(
        &current,
        &baseline,
        Some(&release),
        from,
        &SpikeRules::default(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].fingerprint, "sentry:CHECKOUT-1A");
    assert_eq!(found[0].baseline, 24);

    // The same counts in both windows: nothing.
    assert!(anomalies(&baseline, &baseline, None, from, &SpikeRules::default()).is_empty());
}

#[tokio::test]
async fn datadog_counts_error_logs_per_error_kind() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/logs/analytics/aggregate"))
        .and(header("DD-API-KEY", "dd-api"))
        .and(header("DD-APPLICATION-KEY", "dd-app"))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "filter": {
                "query": "service:checkout env:prod status:error",
                "from": "2026-11-20T09:00:00Z",
                "to": "2026-11-20T10:00:00Z"
            },
            "group_by": [{"facet": "@error.kind"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"buckets": [
                {"by": {"@error.kind": "TypeError"}, "computes": {"c0": 120}},
                {"by": {"@error.kind": ""}, "computes": {"c0": 9}},
                {"by": {"@error.kind": "LockTimeout"}, "computes": {"c0": 4}}
            ]},
            "meta": {"status": "done"}
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
    let client = ApmClient::new(&cfg, Some("dd-api"), Some("dd-app")).unwrap();
    let counts = client
        .error_counts(
            time::macros::datetime!(2026-11-20 09:00 UTC),
            time::macros::datetime!(2026-11-20 10:00 UTC),
        )
        .await
        .unwrap();
    let got: Vec<(&str, u64)> = counts
        .iter()
        .map(|c| (c.fingerprint.as_str(), c.count))
        .collect();
    assert_eq!(
        got,
        [("datadog:TypeError", 120), ("datadog:LockTimeout", 4)]
    );
    assert_eq!(client.latest_release().await.unwrap(), None);
}

#[tokio::test]
async fn cloudwatch_does_not_count_errors() {
    let config: ApmConfig = serde_json::from_value(serde_json::json!({
        "kind": "cloudwatch",
        "region": "ap-northeast-1",
        "log_groups": ["/ecs/checkout"]
    }))
    .unwrap();
    let client = ApmClient::new(&config, None, None).unwrap();
    assert!(!client.counts_errors());
    let now = time::OffsetDateTime::now_utc();
    assert!(client.error_counts(now, now).await.is_err());
}
