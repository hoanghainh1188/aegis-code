//! The Backlog client against a faked Backlog (API v2, API key).

use aegis_tracker::{Report, Tracker, TrackerConfig};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(server: &MockServer, extra: Value) -> TrackerConfig {
    let mut value = json!({
        "kind": "backlog",
        "url": server.uri(),
        "token_env": "AEGIS_BACKLOG_API_KEY",
        "project_ids": [12],
        "status_ids": [1, 2],
        "keyword": "aegis",
    });
    for (k, v) in extra.as_object().unwrap() {
        value[k] = v.clone();
    }
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn search_sends_the_filter_and_follows_the_pages() {
    let server = MockServer::start().await;
    let full: Vec<Value> = (1..=100)
        .map(|n| json!({ "id": n, "issueKey": format!("APP-{n}") }))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/v2/issues"))
        .and(query_param("apiKey", "key"))
        .and(query_param("projectId[]", "12"))
        .and(query_param("statusId[]", "2"))
        .and(query_param("keyword", "aegis"))
        .and(query_param("offset", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 101, "issueKey": "APP-101" }, { "id": 102, "issueKey": "nope" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/issues"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Array(full)))
        .mount(&server)
        .await;
    let tracker = Tracker::new(&config(&server, json!({})), "key").unwrap();
    let keys = tracker.search().await.unwrap();
    assert_eq!(keys.len(), 101);
    assert_eq!(keys.first().map(String::as_str), Some("APP-1"));
    assert_eq!(keys.last().map(String::as_str), Some("APP-101"));
}

#[tokio::test]
async fn a_ticket_brings_its_description_and_text_logs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/issues/APP-7"))
        .and(query_param("apiKey", "key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "issueKey": "APP-7", "summary": "Crash on save",
            "description": "Saving crashes.",
            "attachments": [
                { "id": 31, "name": "server.log", "size": 20 },
                { "id": 32, "name": "photo.jpg", "size": 20 },
                { "id": 33, "name": "huge.log", "size": 50000000 },
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/issues/APP-7/attachments/31"))
        .and(query_param("apiKey", "key"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ERROR disk full"))
        .expect(1)
        .mount(&server)
        .await;
    let tracker = Tracker::new(&config(&server, json!({})), "key").unwrap();
    let ticket = tracker.ticket("APP-7").await.unwrap();
    assert_eq!(ticket.title, "Crash on save");
    assert_eq!(ticket.url, format!("{}/view/APP-7", server.uri()));
    assert_eq!(ticket.logs.len(), 1, "{:?}", ticket.logs);
    assert_eq!(ticket.logs[0].name, "server.log");
    assert_eq!(ticket.logs[0].text, "ERROR disk full");
}

#[tokio::test]
async fn the_report_comments_and_sets_the_status_once_a_pr_is_open() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/issues/APP-7/comments"))
        .and(query_param("apiKey", "key"))
        .and(body_string_contains("content=PR+opened"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v2/issues/APP-7"))
        .and(body_string_contains("statusId=3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 7 })))
        .expect(1)
        .mount(&server)
        .await;
    let tracker = Tracker::new(&config(&server, json!({ "pr_status_id": 3 })), "key").unwrap();
    tracker
        .report(
            "APP-7",
            &Report {
                comment: "PR opened",
                pr_url: Some("https://github.com/me/app/pull/4"),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn errors_never_show_the_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/issues/APP-7"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "errors": [{ "message": "Authentication failure.", "code": 11 }]
        })))
        .mount(&server)
        .await;
    let tracker = Tracker::new(&config(&server, json!({})), "s3cret-key").unwrap();
    let text = tracker.ticket("APP-7").await.unwrap_err().to_string();
    assert!(
        text.contains("401") && text.contains("Authentication failure"),
        "{text}"
    );
    assert!(!text.contains("s3cret"), "{text}");

    // Nothing listening: the transport error names no URL either.
    let gone = Tracker::new(
        &serde_json::from_value(json!({
            "kind": "backlog", "url": "http://127.0.0.1:9", "token_env": "K", "project_ids": [1]
        }))
        .unwrap(),
        "s3cret-key",
    )
    .unwrap();
    let text = gone.search().await.unwrap_err().to_string();
    assert!(!text.contains("s3cret"), "{text}");
}

#[test]
fn the_configuration_is_checked() {
    let check = |value: Value| {
        serde_json::from_value::<TrackerConfig>(value)
            .map_err(|e| e.to_string())
            .and_then(|c| c.check().map_err(|e| e.to_string()))
    };
    let good = json!({
        "kind": "backlog", "url": "https://acme.backlog.com",
        "token_env": "AEGIS_BACKLOG_API_KEY", "project_ids": [12]
    });
    assert!(check(good.clone()).is_ok());
    let mut none = good.clone();
    none["project_ids"] = json!([]);
    assert!(check(none).unwrap_err().contains("project_ids"));
    let mut plain = good;
    plain["url"] = json!("http://acme.backlog.com");
    assert!(check(plain).unwrap_err().contains("https"));
}
