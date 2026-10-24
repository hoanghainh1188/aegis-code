//! The internal API (#23): jobs created, listed, cancelled and retried,
//! every answer in the `{data, error, meta}` envelope.

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{NewRepo, RepoMode, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!("not JSON: {}", String::from_utf8_lossy(&bytes));
    });
    (status, json)
}

async fn repo(s: &Store) -> i64 {
    s.create_repo(&NewRepo {
        slug: "typst".into(),
        url: "https://github.com/typst/typst".into(),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
    .0
}

fn assert_error(body: &Value, code: &str) {
    assert_eq!(body["data"], Value::Null, "{body}");
    assert_eq!(body["error"]["code"], code, "{body}");
    assert!(body["error"]["message"].is_string(), "{body}");
}

#[tokio::test]
async fn jobs_are_created_listed_cancelled_and_retried() {
    let Some(db) = test_db().await else { return };
    let app = router(db.store.clone());
    let repo_id = repo(&db.store).await;

    let (status, body) = call(&app, "GET", "/api/v1/health", None).await;
    assert_eq!(
        (status, &body["data"]["status"]),
        (StatusCode::OK, &json!("ok"))
    );

    let new = json!({ "repo_id": repo_id, "issue_ref": "typst/typst#1", "source": "github",
                      "ledger_key": "typst-1" });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(new.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["error"], Value::Null);
    let job = &body["data"];
    assert_eq!(
        (&job["status"], &job["ledger_key"]),
        (&json!("queued"), &json!("TYPST-1"))
    );
    let id = job["id"].as_i64().unwrap();

    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(new)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_error(&body, "conflict");

    for n in 2..5 {
        let issue = json!({ "repo_id": repo_id, "issue_ref": format!("typst/typst#{n}"),
                            "source": "github" });
        let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(issue)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = call(&app, "GET", "/api/v1/jobs?limit=3", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 3);
    assert_eq!(body["meta"]["limit"], 3);
    let cursor = body["meta"]["next_cursor"].as_i64().unwrap();
    let (_, body) = call(
        &app,
        "GET",
        &format!("/api/v1/jobs?limit=3&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["meta"]["next_cursor"], Value::Null);
    assert_eq!(body["data"][0]["id"], id);

    let (status, body) = call(&app, "POST", &format!("/api/v1/jobs/{id}/cancel"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"], "cancelled");
    let (_, body) = call(&app, "GET", "/api/v1/jobs?status=cancelled", None).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 1);

    let (status, body) = call(&app, "POST", &format!("/api/v1/jobs/{id}/retry"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        (&body["data"]["retry_of"], &body["data"]["ledger_key"]),
        (&json!(id), &json!("TYPST-1-R1"))
    );
    let (status, body) = call(&app, "GET", &format!("/api/v1/jobs/{id}/runs"), None).await;
    assert_eq!(
        (status, body["data"].as_array().unwrap().len()),
        (StatusCode::OK, 0)
    );
    db.drop().await;
}

#[tokio::test]
async fn bad_requests_answer_in_the_envelope() {
    let Some(db) = test_db().await else { return };
    let app = router(db.store.clone());
    let repo_id = repo(&db.store).await;

    let (status, body) = call(&app, "GET", "/api/v1/jobs/9999", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "not_found");
    let (status, body) = call(&app, "GET", "/api/v1/jobs/abc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "bad_request");
    let (status, body) = call(&app, "GET", "/api/v1/nothing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "not_found");
    let (status, body) = call(&app, "GET", "/api/v1/jobs?status=sleeping", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_error(&body, "invalid");

    for bad in [
        json!({ "repo_id": repo_id, "issue_ref": "a/b#1", "source": "email" }),
        json!({ "repo_id": repo_id, "issue_ref": " ", "source": "github" }),
        json!({ "repo_id": repo_id, "issue_ref": "a/b#1", "source": "github", "ledger_key": "../x" }),
    ] {
        let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(bad.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert_error(&body, "invalid");
    }
    let unknown_repo = json!({ "repo_id": 9999, "issue_ref": "a/b#1", "source": "github" });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(unknown_repo)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "not_found");
    let extra = json!({ "repo_id": repo_id, "issue_ref": "a/b#1", "source": "github", "x": 1 });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(extra)).await;
    assert!(status.is_client_error(), "{status}");
    assert_error(&body, "bad_request");
    let (status, body) = call(&app, "POST", "/api/v1/jobs/9999/retry", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "not_found");
    db.drop().await;
}
