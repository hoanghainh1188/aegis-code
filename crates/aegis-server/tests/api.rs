//! The internal API (#23): jobs created, listed, cancelled and retried,
//! every answer in the `{data, error, meta}` envelope. Here by an admin
//! service; who may do what is in `auth.rs`.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{ModeName, NewRepo, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The API, and the token of an admin service.
struct App {
    router: Router,
    token: String,
}

async fn app(store: &Store) -> App {
    common::admin(store, "ops").await;
    App {
        router: router(store.clone(), common::auth(store)),
        token: common::token(store, "ops").await,
    }
}

async fn call(app: &App, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", app.token))
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
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
        mode: ModeName::POC,
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
    let app = app(&db.store).await;
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
    let app = app(&db.store).await;
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

/// #570: `"source": "repo"` takes the issue's number on the repo's own
/// forge, as the jobs page does, completed from its URL; what is not a
/// number is refused, saying how to write it.
#[tokio::test]
async fn a_job_takes_the_issue_s_number_on_the_repo() {
    let Some(db) = test_db().await else { return };
    let app = app(&db.store).await;
    let repo_id = repo(&db.store).await;
    for (input, n) in [("12", 12), ("#13", 13)] {
        let new = json!({ "repo_id": repo_id, "issue_ref": input, "source": "repo" });
        let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(new)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            body["data"]["issue_ref"],
            format!("typst/typst#{n}"),
            "{body}"
        );
        assert_eq!(body["data"]["source"], "github", "{body}");
    }
    let bad = json!({ "repo_id": repo_id, "issue_ref": "twelve", "source": "repo" });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(bad)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_error(&body, "invalid");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("12 or #12"),
        "{body}"
    );
    let unknown = json!({ "repo_id": 9999, "issue_ref": "12", "source": "repo" });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(unknown)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_error(&body, "not_found");
    db.drop().await;
}

/// The status of a POST with `len` bytes of body, as the router answers.
async fn post_bytes(app: &App, uri: &str, len: usize) -> StatusCode {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", app.token))
        .body(Body::from(vec![b' '; len]))
        .unwrap();
    app.router.clone().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn the_api_keeps_its_64_kb_limit_and_webhooks_have_their_own() {
    let Some(db) = test_db().await else { return };
    let app = app(&db.store).await;

    assert_eq!(
        post_bytes(&app, "/api/v1/jobs", 65 * 1024).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // Past the API's limit, under the webhook's: not refused for its size
    // (404 here, the test server having no webhook secret).
    assert_eq!(
        post_bytes(&app, "/webhooks/github", 200 * 1024).await,
        StatusCode::NOT_FOUND
    );
    db.drop().await;
}

/// #243: a review round is asked for on the job that opened the PR, at
/// most `[reviews] max_rounds` per PR, never through `POST /api/v1/jobs`.
#[tokio::test]
async fn a_review_round_is_asked_for_on_the_job_that_opened_the_pr() {
    let Some(db) = test_db().await else { return };
    let store = db.store.clone().with_max_review_rounds(1);
    let app = app(&store).await;
    let repo_id = repo(&store).await;
    let as_review = json!({ "repo_id": repo_id, "issue_ref": "typst/typst#9", "source": "review" });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(as_review)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("/review"),
        "{body}"
    );

    let new = json!({ "repo_id": repo_id, "issue_ref": "typst/typst#9", "source": "github" });
    let (_, body) = call(&app, "POST", "/api/v1/jobs", Some(new)).await;
    let id = body["data"]["id"].as_i64().unwrap();
    let (status, body) = call(&app, "POST", &format!("/api/v1/jobs/{id}/review"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "no PR yet: {body}");

    let claim = store
        .claim_job("w", std::time::Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let done = aegis_store::AttemptEnd::Done {
        outcome: aegis_ledger::Outcome::PrOpened,
        reason: None,
        pr_url: Some("https://github.com/aegis-bot/typst/pull/4".into()),
        patch: None,
        diagnosis: None,
    };
    store.finish_attempt(&claim.run, "w", &done).await.unwrap();
    let (status, body) = call(&app, "POST", &format!("/api/v1/jobs/{id}/review"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let round = &body["data"];
    assert_eq!(
        (
            &round["source"],
            &round["review_of"],
            &round["review_round"],
            &round["issue_ref"]
        ),
        (
            &json!("review"),
            &json!(id),
            &json!(1),
            &json!("aegis-bot/typst#4")
        )
    );
    let (status, body) = call(&app, "POST", &format!("/api/v1/jobs/{id}/review"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    db.drop().await;
}

/// CVE jobs (#502, ADR 0015): their own route, the target checked, kept as
/// the job's `issue_ref`; never through the plain job route.
#[tokio::test]
async fn a_cve_job_is_asked_for_with_a_checked_target() {
    let Some(db) = test_db().await else { return };
    let app = app(&db.store).await;
    let repo_id = repo(&db.store).await;
    let uri = format!("/api/v1/repos/{repo_id}/cve-jobs");
    let target = json!({ "package": "serde_json", "version": "1.0.140",
                         "advisory": "RUSTSEC-2099-0001" });

    let (status, body) = call(&app, "POST", &uri, Some(target.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let job = &body["data"];
    assert_eq!(job["source"], "cve");
    assert_eq!(job["issue_ref"], "cve:RUSTSEC-2099-0001 serde_json 1.0.140");
    assert_eq!(job["status"], "queued");
    assert!(
        job["ledger_key"]
            .as_str()
            .unwrap()
            .starts_with("CVE-RUSTSEC-2099-0001-SERDE-JSON-1-0-140-"),
        "{job}"
    );
    // One live job per target, as per issue.
    let (status, _) = call(&app, "POST", &uri, Some(target)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    for (bad, field) in [
        (
            json!({ "package": "--config=x", "version": "1", "advisory": "A-1" }),
            "package",
        ),
        (
            json!({ "package": "x", "version": "1 2", "advisory": "A-1" }),
            "version",
        ),
        (
            json!({ "package": "x", "version": "1", "advisory": "a/../b" }),
            "advisory",
        ),
    ] {
        let (status, body) = call(&app, "POST", &uri, Some(bad)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_error(&body, "invalid");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(field),
            "{body}"
        );
    }
    let (status, _) = call(
        &app,
        "POST",
        &uri,
        Some(json!({ "package": "x", "version": "1", "advisory": "A-1", "extra": 1 })),
    )
    .await;
    assert!(status.is_client_error());

    // Not as a plain job, whose issue_ref nothing would check.
    let plain = json!({ "repo_id": repo_id, "issue_ref": "cve:A-1 x 1", "source": "cve" });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(plain)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("/cve-jobs"),
        "{body}"
    );
    // Nor an error spike's diagnosis, which the platform finds itself (#706).
    let apm = json!({ "repo_id": repo_id, "issue_ref": "apm-1", "source": "apm" });
    let (status, body) = call(&app, "POST", "/api/v1/jobs", Some(apm)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    db.drop().await;
}

#[tokio::test]
async fn repo_memories_api_and_merged_pr_extraction_work_end_to_end() {
    let Some(db) = test_db().await else { return };
    let repo_id = repo(&db.store).await;
    let app = app(&db.store).await;
    let uri = format!("/api/v1/repos/{repo_id}/memories");

    // Create a business_rule and a runbook via the API.
    let (status, created_rule) = call(
        &app,
        "POST",
        &uri,
        Some(json!({
            "kind": "business_rule",
            "source_ref": "rule:tax-rounding",
            "title": "Business rule: JPY tax rounds per rate bucket",
            "summary": "Never round JPY consumption tax per line item in src/tax.rs.",
            "paths": ["src/tax.rs"],
            "keywords": "jpy tax rounding bucket"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created_rule}");
    let rule_id = created_rule["data"]["id"].as_i64().unwrap();

    let (status, created_runbook) = call(
        &app,
        "POST",
        &uri,
        Some(json!({
            "kind": "runbook",
            "source_ref": "runbook:tax-mismatch",
            "title": "Runbook: JPY invoice tax total mismatch",
            "summary": "Inspect src/tax.rs bucket aggregation first.",
            "paths": ["src/tax.rs"],
            "keywords": "jpy invoice tax mismatch"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created_runbook}");

    // Simulate a job that opened a PR and had a review round, then extract merged PR memory.
    let pr_url = "https://github.com/typst/typst/pull/55";
    let root_key = aegis_ledger::IssueId::parse("TYPST-55-1".to_owned()).unwrap();
    let root_job = db
        .store
        .create_job(&aegis_store::NewJob {
            repo_id: aegis_store::RepoId(repo_id),
            issue_ref: "typst/typst#55".into(),
            ledger_key: root_key.clone(),
            source: aegis_store::JobSource::Github,
            priority: None,
            sla_deadline: None,
        })
        .await
        .unwrap();
    let claim = db
        .store
        .claim_job("w", std::time::Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let ledger = db.store.ledger();
    aegis_ledger::LedgerStore::append(
        &ledger,
        &root_key,
        &aegis_ledger::LedgerEvent::IssueRead {
            title: "JPY tax rounding off by 1 yen in src/tax.rs".into(),
            url: "https://github.com/typst/typst/issues/55".into(),
        },
    )
    .await
    .unwrap();
    aegis_ledger::LedgerStore::append(
        &ledger,
        &root_key,
        &aegis_ledger::LedgerEvent::Answered(aegis_ledger::AnswerSummary {
            fixed: true,
            cause: "src/tax.rs rounded each line item before summing buckets.".into(),
            changes: vec![aegis_ledger::AnswerChange {
                location: "src/tax.rs:42".into(),
                note: "Round once per tax rate bucket.".into(),
            }],
            repro_test: Some("jpy_bucket_rounding".into()),
            stop_reason: None,
            review_replies: vec![],
            suspicious_input: vec![],
        }),
    )
    .await
    .unwrap();
    db.store
        .finish_attempt(
            &claim.run,
            "w",
            &aegis_store::AttemptEnd::Done {
                outcome: aegis_ledger::Outcome::PrOpened,
                pr_url: Some(pr_url.into()),
                reason: None,
                patch: None,
                diagnosis: None,
            },
        )
        .await
        .unwrap();

    // Add a review round (/aegis fix) and record its reply.
    let round_job = db
        .store
        .review_job_by(root_job.id, &aegis_store::Actor::system())
        .await
        .unwrap();
    aegis_ledger::LedgerStore::append(
        &ledger,
        &round_job.ledger_key,
        &aegis_ledger::LedgerEvent::Answered(aegis_ledger::AnswerSummary {
            fixed: true,
            cause: "Addressed review feedback on zero-decimal currency.".into(),
            changes: vec![aegis_ledger::AnswerChange {
                location: "src/tax.rs:58".into(),
                note: "Assert zero fractional yen in bucket totals.".into(),
            }],
            repro_test: None,
            stop_reason: None,
            review_replies: vec![aegis_ledger::AnswerReply {
                note: 1,
                done: true,
                reply: "Added zero-decimal currency assertion in src/tax.rs.".into(),
            }],
            suspicious_input: vec![],
        }),
    )
    .await
    .unwrap();

    let extracted = aegis_server::memory::extract_and_store(
        &db.store,
        pr_url,
        None,
        &aegis_store::Actor::webhook("github"),
    )
    .await
    .unwrap();
    assert_eq!(extracted.len(), 2, "expected merged_pr + review_lesson");

    // List all 4 memories via GET /api/v1/repos/{id}/memories.
    let (status, listed) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["data"].as_array().unwrap().len(), 4);

    // Enrich an incoming issue and verify >=3 relevant memories are recalled into BP3.
    let mut incoming = aegis_lifecycle::IssueInput {
        id: "typst-99".into(),
        title: "JPY invoice tax rounding bug in src/tax.rs".into(),
        body: "10% JPY tax bucket calculation in src/tax.rs is off by 1 yen.".into(),
        url: "https://github.com/typst/typst/issues/99".into(),
        ..Default::default()
    };
    aegis_server::memory::enrich_with_memory(
        &db.store,
        aegis_store::RepoId(repo_id),
        &mut incoming,
        None,
    )
    .await;
    let bp3 = incoming.first_message("Tax rounding mismatch in src/tax.rs.", true);
    assert!(bp3.contains("<repo-memory count=\"4\">"), "{bp3}");
    assert!(bp3.contains("[merged_pr]"), "{bp3}");
    assert!(bp3.contains("[review_lesson]"), "{bp3}");
    assert!(bp3.contains("[business_rule]"), "{bp3}");
    assert!(bp3.contains("[runbook]"), "{bp3}");

    // Delete the business rule via DELETE /api/v1/repos/{id}/memories/{memory_id}.
    let del_uri = format!("{uri}/{rule_id}");
    let (status, _) = call(&app, "DELETE", &del_uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "DELETE", &del_uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    db.drop().await;
}
