//! Wiremock integration tests for Jira Service Management (`jsm`, #453).

use aegis_tracker::{
    Deployment, JsmConfig, Priority, Report, Tracker, TrackerConfig, TrackerError,
};
use serde_json::json;
use wiremock::matchers::{basic_auth, bearer_token, body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cloud_cfg(server: &MockServer) -> TrackerConfig {
    TrackerConfig::Jsm(JsmConfig {
        url: server.uri(),
        deployment: Deployment::Cloud,
        user: Some("bot@acme.com".into()),
        token_env: "AEGIS_JSM_TOKEN".into(),
        jql: "project = SD AND status = \"Waiting for support\"".into(),
        service_desk_id: Some("4".into()),
        sla_field: Some("customfield_10050".into()),
        pr_transition: Some("In Review".into()),
        diagnosis_transition: Some("Needs Triage".into()),
        internal_comments: true,
    })
}

#[test]
fn jsm_configuration_is_checked() {
    let base = JsmConfig {
        url: "https://acme.atlassian.net".into(),
        deployment: Deployment::Cloud,
        user: Some("bot@acme.com".into()),
        token_env: "AEGIS_JSM_TOKEN".into(),
        jql: "project = SD".into(),
        service_desk_id: None,
        sla_field: Some("customfield_10050".into()),
        pr_transition: None,
        diagnosis_transition: None,
        internal_comments: true,
    };
    assert!(Tracker::new(&TrackerConfig::Jsm(base.clone()), "tok").is_ok());

    let no_user = JsmConfig {
        user: None,
        ..base.clone()
    };
    assert!(matches!(
        Tracker::new(&TrackerConfig::Jsm(no_user), "tok"),
        Err(TrackerError::Config(_))
    ));

    let bad_field = JsmConfig {
        sla_field: Some("customfield_1,summary".into()),
        ..base
    };
    assert!(matches!(
        Tracker::new(&TrackerConfig::Jsm(bad_field), "tok"),
        Err(TrackerError::Config(_))
    ));
}

#[tokio::test]
async fn jsm_search_maps_priority_and_sla_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search/jql"))
        .and(basic_auth("bot@acme.com", "secret"))
        .and(query_param(
            "fields",
            "summary,priority,duedate,customfield_10050",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [
                {
                    "key": "SD-101",
                    "fields": {
                        "summary": "Checkout crashes with 500",
                        "priority": { "name": "Highest" },
                        "customfield_10050": {
                            "ongoingCycle": {
                                "breachTime": { "iso8601": "2026-12-01T14:00:00+0000" }
                            }
                        }
                    }
                },
                {
                    "key": "SD-102",
                    "fields": {
                        "summary": "Slow report query",
                        "priority": { "name": "High" },
                        "duedate": "2026-12-05"
                    }
                },
                {
                    "key": "SD-103",
                    "fields": {
                        "summary": "Typo in banner",
                        "priority": { "name": "Low" }
                    }
                }
            ]
        })))
        .mount(&server)
        .await;

    let tracker = Tracker::new(&cloud_cfg(&server), "secret").unwrap();
    let tickets = tracker.search_tickets().await.unwrap();
    assert_eq!(tickets.len(), 3);

    assert_eq!(tickets[0].key, "SD-101");
    assert_eq!(tickets[0].priority, Some(Priority::P1));
    assert_eq!(
        tickets[0].sla_deadline,
        Some(time::macros::datetime!(2026-12-01 14:00:00 UTC))
    );

    assert_eq!(tickets[1].key, "SD-102");
    assert_eq!(tickets[1].priority, Some(Priority::P2));
    assert_eq!(
        tickets[1].sla_deadline,
        Some(time::macros::datetime!(2026-12-05 23:59:59 UTC))
    );

    assert_eq!(tickets[2].key, "SD-103");
    assert_eq!(tickets[2].priority, Some(Priority::P3));
    assert_eq!(tickets[2].sla_deadline, None);
}

#[tokio::test]
async fn jsm_ticket_reads_description_logs_and_sla_and_reports_back() {
    let server = MockServer::start().await;
    let site = server.uri();

    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/SD-42"))
        .and(bearer_token("dc-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": "SD-42",
            "fields": {
                "summary": "Payment webhook panic",
                "description": "Occurs on empty signature header",
                "priority": { "name": "Blocker" },
                "customfield_10050": {
                    "breachTime": { "iso8601": "2026-11-20T09:30:00Z" }
                },
                "attachment": [
                    {
                        "filename": "panic.log",
                        "mimeType": "text/plain",
                        "size": 17,
                        "content": format!("{site}/secure/attachment/1/panic.log")
                    },
                    {
                        "filename": "evil.log",
                        "mimeType": "text/plain",
                        "size": 10,
                        "content": "https://other.example/evil.log"
                    }
                ]
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/secure/attachment/1/panic.log"))
        .and(bearer_token("dc-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_string("thread panicked"))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/servicedeskapi/request/SD-42/comment"))
        .and(body_partial_json(json!({
            "body": "PR ready",
            "public": false
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "10" })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/SD-42/remotelink"))
        .and(body_partial_json(json!({
            "object": { "url": "https://github.com/acme/app/pull/8" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 99 })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/SD-42/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transitions": [{ "id": "31", "name": "In Review" }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/SD-42/transitions"))
        .and(body_partial_json(json!({ "transition": { "id": "31" } })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = TrackerConfig::Jsm(JsmConfig {
        url: site,
        deployment: Deployment::DataCenter,
        user: None,
        token_env: "AEGIS_JSM_TOKEN".into(),
        jql: "project = SD".into(),
        service_desk_id: None,
        sla_field: Some("customfield_10050".into()),
        pr_transition: Some("in review".into()),
        diagnosis_transition: Some("Needs Triage".into()),
        internal_comments: true,
    });
    let tracker = Tracker::new(&cfg, "dc-tok").unwrap();
    let ticket = tracker.ticket("SD-42").await.unwrap();
    assert_eq!(ticket.key, "SD-42");
    assert_eq!(ticket.priority, Some(Priority::P1));
    assert_eq!(
        ticket.sla_deadline,
        Some(time::macros::datetime!(2026-11-20 09:30:00 UTC))
    );
    assert_eq!(ticket.logs.len(), 1);
    assert_eq!(ticket.logs[0].text, "thread panicked");

    tracker
        .report(
            "SD-42",
            &Report {
                comment: "PR ready",
                pr_url: Some("https://github.com/acme/app/pull/8"),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn jsm_diagnosis_report_triggers_diagnosis_transition_and_public_comment() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/servicedeskapi/request/SD-9/comment"))
        .and(body_partial_json(json!({
            "body": "Stopped: configuration mismatch",
            "public": true
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "11" })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/SD-9/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transitions": [{ "id": "44", "name": "Needs Triage" }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/SD-9/transitions"))
        .and(body_partial_json(json!({ "transition": { "id": "44" } })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = TrackerConfig::Jsm(JsmConfig {
        url: server.uri(),
        deployment: Deployment::Cloud,
        user: Some("bot@acme.com".into()),
        token_env: "AEGIS_JSM_TOKEN".into(),
        jql: "project = SD".into(),
        service_desk_id: None,
        sla_field: None,
        pr_transition: None,
        diagnosis_transition: Some("Needs Triage".into()),
        internal_comments: false,
    });
    let tracker = Tracker::new(&cfg, "secret").unwrap();
    tracker
        .report(
            "SD-9",
            &Report {
                comment: "Stopped: configuration mismatch",
                pr_url: None,
            },
        )
        .await
        .unwrap();
}
