//! The Jira client against a faked Jira: Cloud and Data Center.

use aegis_tracker::{Report, Tracker, TrackerConfig, TrackerError};
use serde_json::{Value, json};
use wiremock::matchers::{basic_auth, bearer_token, body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(server: &MockServer, extra: Value) -> TrackerConfig {
    let mut value = json!({
        "kind": "jira",
        "url": server.uri(),
        "user": "bot@acme.com",
        "token_env": "AEGIS_JIRA_TOKEN",
        "jql": "project = APP AND labels = aegis",
    });
    for (k, v) in extra.as_object().unwrap() {
        value[k] = v.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn issue(key: &str) -> Value {
    json!({ "id": "1", "key": key })
}

#[tokio::test]
async fn cloud_search_follows_the_pages_and_skips_odd_keys() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search/jql"))
        .and(basic_auth("bot@acme.com", "tok"))
        .and(query_param("jql", "project = APP AND labels = aegis"))
        .and(query_param("nextPageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [issue("APP-3")], "isLast": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search/jql"))
        .and(basic_auth("bot@acme.com", "tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [issue("APP-1"), issue("../../x"), issue("APP-2")],
            "nextPageToken": "p2", "isLast": false
        })))
        .mount(&server)
        .await;
    let tracker = Tracker::new(&config(&server, json!({})), "tok").unwrap();
    assert_eq!(tracker.search().await.unwrap(), ["APP-1", "APP-2", "APP-3"]);
}

#[tokio::test]
async fn data_center_search_pages_by_start_and_takes_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(bearer_token("pat"))
        .and(query_param("startAt", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "startAt": 2, "total": 3, "issues": [issue("OPS-9")]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(bearer_token("pat"))
        .and(query_param("startAt", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "startAt": 0, "total": 3, "issues": [issue("OPS-7"), issue("OPS-8")]
        })))
        .mount(&server)
        .await;
    let config = config(
        &server,
        json!({ "deployment": "data-center", "user": null }),
    );
    let tracker = Tracker::new(&config, "pat").unwrap();
    assert_eq!(tracker.search().await.unwrap(), ["OPS-7", "OPS-8", "OPS-9"]);
}

#[tokio::test]
async fn a_ticket_brings_its_description_and_text_logs() {
    let server = MockServer::start().await;
    let long: String = (0..2000).map(|i| format!("line {i}\n")).collect();
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/APP-7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": "APP-7",
            "fields": {
                "summary": "Crash on save",
                "description": "Saving a file crashes.",
                "attachment": [
                    { "id": "10", "filename": "app.log", "mimeType": "text/plain", "size": 30,
                      "content": format!("{}/attachments/10", server.uri()) },
                    { "id": "11", "filename": "screen.png", "mimeType": "image/png", "size": 30,
                      "content": format!("{}/attachments/11", server.uri()) },
                    { "id": "12", "filename": "other.log", "mimeType": "text/plain", "size": 30,
                      "content": "https://elsewhere.example/attachments/12" },
                    { "id": "13", "filename": "long.txt", "mimeType": "text/plain", "size": 20000,
                      "content": format!("{}/attachments/13", server.uri()) },
                ]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachments/10"))
        .and(basic_auth("bot@acme.com", "tok"))
        .respond_with(ResponseTemplate::new(200).set_body_string("panic: index out of range"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachments/13"))
        .respond_with(ResponseTemplate::new(200).set_body_string(long))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachments/11"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let tracker = Tracker::new(&config(&server, json!({})), "tok").unwrap();
    let ticket = tracker.ticket("APP-7").await.unwrap();
    assert_eq!(ticket.key, "APP-7");
    assert_eq!(ticket.title, "Crash on save");
    assert_eq!(ticket.url, format!("{}/browse/APP-7", server.uri()));
    let names: Vec<&str> = ticket.logs.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        ["app.log", "long.txt"],
        "not images, not other hosts"
    );
    assert_eq!(ticket.logs[0].text, "panic: index out of range");
    assert!(ticket.logs[1].truncated);
    assert!(
        ticket.logs[1].text.ends_with("line 1999\n"),
        "the tail is kept"
    );
    let body = ticket.body_with_logs();
    assert!(body.starts_with("Saving a file crashes."), "{body}");
    assert!(
        body.contains("app.log") && body.contains("panic: index"),
        "{body}"
    );
}

#[tokio::test]
async fn a_key_that_is_not_one_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let tracker = Tracker::new(&config(&server, json!({})), "tok").unwrap();
    for key in ["app-1", "APP-", "APP-1/../2", "APP-1?x"] {
        assert!(
            matches!(tracker.ticket(key).await, Err(TrackerError::Config(_))),
            "{key}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_report_comments_links_the_pr_and_moves_the_ticket() {
    let server = MockServer::start().await;
    let pr = "https://github.com/me/app/pull/4";
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/APP-7/comment"))
        .and(body_json(json!({ "body": "done" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "100" })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/APP-7/remotelink"))
        .and(body_json(json!({
            "globalId": format!("aegis-code-pr={pr}"),
            "object": { "url": pr, "title": "Pull request by aegis-code" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 5 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/APP-7/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transitions": [{ "id": "21", "name": "In Progress" }, { "id": "31", "name": "In Review" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/APP-7/transitions"))
        .and(body_json(json!({ "transition": { "id": "31" } })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let config = config(&server, json!({ "pr_transition": "in review" }));
    let tracker = Tracker::new(&config, "tok").unwrap();
    tracker
        .report(
            "APP-7",
            &Report {
                comment: "done",
                pr_url: Some(pr),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn without_a_pr_only_the_comment_is_written() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/APP-7/comment"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "100" })))
        .expect(1)
        .mount(&server)
        .await;
    let config = config(&server, json!({ "pr_transition": "In Review" }));
    let tracker = Tracker::new(&config, "tok").unwrap();
    tracker
        .report(
            "APP-7",
            &Report {
                comment: "stopped",
                pr_url: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn jira_errors_say_the_status_and_jiras_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/APP-7"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "errorMessages": ["Issue does not exist or you do not have permission to see it."]
        })))
        .mount(&server)
        .await;
    let tracker = Tracker::new(&config(&server, json!({})), "tok").unwrap();
    let error = tracker.ticket("APP-7").await.unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("404") && text.contains("does not exist"),
        "{text}"
    );
    assert!(!text.contains("tok"), "{text}");
}

#[test]
fn the_configuration_is_checked() {
    let check = |value: Value| {
        serde_json::from_value::<TrackerConfig>(value)
            .map_err(|e| e.to_string())
            .and_then(|c| c.check().map_err(|e| e.to_string()))
    };
    let good = json!({
        "kind": "jira", "url": "https://acme.atlassian.net", "user": "bot@acme.com",
        "token_env": "AEGIS_JIRA_TOKEN", "jql": "project = APP"
    });
    assert!(check(good.clone()).is_ok());
    let with = |k: &str, v: Value| {
        let mut c = good.clone();
        c[k] = v;
        c
    };
    for (bad, why) in [
        (with("url", json!("http://jira.acme.com")), "https"),
        (
            with("url", json!("https://acme.atlassian.net/x?y")),
            "query",
        ),
        (with("user", Value::Null), "user"),
        (with("jql", json!("  ")), "jql"),
        (with("token_env", json!("bad name")), "token_env"),
        (with("surprise", json!(1)), "unknown"),
    ] {
        let error = check(bad).unwrap_err();
        assert!(error.contains(why), "{why}: {error}");
    }
    // Data Center with a personal access token needs no user.
    assert!(check(with("deployment", json!("data-center"))).is_ok());
    let dc = with("deployment", json!("data-center"));
    let mut dc = dc;
    dc["user"] = Value::Null;
    assert!(check(dc).is_ok());
}
