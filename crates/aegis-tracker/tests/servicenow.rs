//! Wiremock integration tests for ServiceNow ITSM (`servicenow`, #453).

use aegis_tracker::{Priority, Report, ServiceNowConfig, Tracker, TrackerConfig, TrackerError};
use serde_json::json;
use wiremock::matchers::{basic_auth, bearer_token, body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sn_cfg(server: &MockServer) -> ServiceNowConfig {
    ServiceNowConfig {
        url: server.uri(),
        user: Some("aegis_bot".into()),
        token_env: "AEGIS_SN_TOKEN".into(),
        table: "incident".into(),
        query: "active=true^state=1".into(),
        pr_state: Some("2".into()),
        diagnosis_state: Some("3".into()),
        work_notes: true,
    }
}

#[test]
fn servicenow_configuration_is_checked() {
    let base = ServiceNowConfig {
        url: "https://acme.service-now.com".into(),
        user: Some("aegis".into()),
        token_env: "AEGIS_SN_TOKEN".into(),
        table: "incident".into(),
        query: "active=true".into(),
        pr_state: None,
        diagnosis_state: None,
        work_notes: true,
    };
    assert!(Tracker::new(&TrackerConfig::Servicenow(base.clone()), "pw").is_ok());

    let bad_table = ServiceNowConfig {
        table: "../sys_user".into(),
        ..base.clone()
    };
    assert!(matches!(
        Tracker::new(&TrackerConfig::Servicenow(bad_table), "pw"),
        Err(TrackerError::Config(_))
    ));

    let empty_query = ServiceNowConfig {
        query: "   ".into(),
        ..base
    };
    assert!(matches!(
        Tracker::new(&TrackerConfig::Servicenow(empty_query), "pw"),
        Err(TrackerError::Config(_))
    ));
}

#[tokio::test]
async fn servicenow_search_maps_priority_matrix_and_sla_due() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(basic_auth("aegis_bot", "secret"))
        .and(query_param("sysparm_query", "active=true^state=1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                {
                    "sys_id": "abc1",
                    "number": "INC0010001",
                    "short_description": "Prod API 502",
                    "priority": { "value": "1", "display_value": "1 - Critical" },
                    "sla_due": { "value": "2026-12-01 08:00:00", "display_value": "2026-12-01 08:00:00" }
                },
                {
                    "sys_id": "abc2",
                    "number": "INC0010002",
                    "short_description": "Batch job lag",
                    "priority": "",
                    "impact": "1",
                    "urgency": "2",
                    "due_date": "2026-12-02T10:15:00Z"
                },
                {
                    "sys_id": "abc3",
                    "number": "INC0010003",
                    "short_description": "Minor UI alignment",
                    "impact": "3",
                    "urgency": "3",
                    "sla_due": ""
                },
                {
                    "sys_id": "bad",
                    "number": "../bad",
                    "short_description": "ignored"
                }
            ]
        })))
        .mount(&server)
        .await;

    let tracker = Tracker::new(&TrackerConfig::Servicenow(sn_cfg(&server)), "secret").unwrap();
    let found = tracker.search_tickets().await.unwrap();
    assert_eq!(found.len(), 3);

    assert_eq!(found[0].key, "INC0010001");
    assert_eq!(found[0].priority, Some(Priority::P1));
    assert_eq!(
        found[0].sla_deadline,
        Some(time::macros::datetime!(2026-12-01 08:00:00 UTC))
    );

    assert_eq!(found[1].key, "INC0010002");
    assert_eq!(found[1].priority, Some(Priority::P2));
    assert_eq!(
        found[1].sla_deadline,
        Some(time::macros::datetime!(2026-12-02 10:15:00 UTC))
    );

    assert_eq!(found[2].key, "INC0010003");
    assert_eq!(found[2].priority, Some(Priority::P3));
    assert_eq!(found[2].sla_deadline, None);
}

#[tokio::test]
async fn servicenow_ticket_reads_attachments_and_patches_work_notes_and_state() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(bearer_token("oauth-tok"))
        .and(query_param("sysparm_query", "number=INC0010009"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{
                "sys_id": "sys999",
                "number": "INC0010009",
                "short_description": "Null pointer in invoice export",
                "description": "Reproduces when tax_id is NULL",
                "priority": "2",
                "sla_due": "2026-12-03 18:00:00"
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/now/attachment"))
        .and(bearer_token("oauth-tok"))
        .and(query_param("sysparm_query", "table_sys_id=sys999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{
                "sys_id": "att1",
                "file_name": "app.log",
                "content_type": "text/plain",
                "size_bytes": "14"
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/now/attachment/att1/file"))
        .and(bearer_token("oauth-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ERROR npe line"))
        .mount(&server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/now/table/incident/sys999"))
        .and(bearer_token("oauth-tok"))
        .and(body_partial_json(json!({
            "work_notes": "Fix opened.\n\nPull Request: https://github.com/acme/app/pull/12",
            "state": "2"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "sys_id": "sys999" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = ServiceNowConfig {
        user: None,
        ..sn_cfg(&server)
    };
    let tracker = Tracker::new(&TrackerConfig::Servicenow(cfg), "oauth-tok").unwrap();

    let ticket = tracker.ticket("INC0010009").await.unwrap();
    assert_eq!(ticket.key, "INC0010009");
    assert_eq!(ticket.title, "Null pointer in invoice export");
    assert_eq!(ticket.body, "Reproduces when tax_id is NULL");
    assert_eq!(ticket.priority, Some(Priority::P2));
    assert_eq!(
        ticket.sla_deadline,
        Some(time::macros::datetime!(2026-12-03 18:00:00 UTC))
    );
    assert!(
        ticket
            .url
            .ends_with("/nav_to.do?uri=incident.do?sys_id=sys999")
    );
    assert_eq!(ticket.logs.len(), 1);
    assert_eq!(ticket.logs[0].text, "ERROR npe line");

    tracker
        .report(
            "INC0010009",
            &Report {
                comment: "Fix opened.",
                pr_url: Some("https://github.com/acme/app/pull/12"),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn servicenow_diagnosis_report_uses_comments_and_diagnosis_state() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_query", "number=INC0010015"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{ "sys_id": "sys15" }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/now/table/incident/sys15"))
        .and(body_partial_json(json!({
            "comments": "Diagnosis: invalid webhook secret",
            "state": "3"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "sys_id": "sys15" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = ServiceNowConfig {
        work_notes: false,
        ..sn_cfg(&server)
    };
    let tracker = Tracker::new(&TrackerConfig::Servicenow(cfg), "secret").unwrap();
    tracker
        .report(
            "INC0010015",
            &Report {
                comment: "Diagnosis: invalid webhook secret",
                pr_url: None,
            },
        )
        .await
        .unwrap();
}
