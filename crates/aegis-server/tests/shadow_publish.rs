//! #699: a shadow job's PR asked for from its page. A maintainer's button
//! queues a job of its own that opens the PR from the shadow job's patch,
//! once, and not as a shadow job even while the repo still is in shadow
//! mode.

mod common;

use std::time::Duration;

use aegis_ledger::{LedgerEvent, LedgerStore, Outcome, ShadowKept};
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    AttemptEnd, JobId, JobSource, ModeName, NewJob, NewRepo, Patch, RepoId, Store, StoreError,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|l| l.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, location, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn get(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(app, request).await;
    (status, body)
}

async fn post(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String, String) {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
}

async fn shadow_repo(s: &Store) -> RepoId {
    s.create_repo(&NewRepo {
        slug: "pilot".into(),
        url: "https://github.com/x/pilot".into(),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 4,
        config: json!({ "shadow": true }),
    })
    .await
    .unwrap()
    .id
}

const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n";

/// A job of `repo` for ticket `n`, claimed and ended as a shadow job,
/// with what it kept: its answer when `kept`, its patch cut when
/// `truncated`.
async fn shadowed(s: &Store, repo: RepoId, n: u32, kept: bool, truncated: bool) -> JobId {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("x/pilot#{n}"),
        ledger_key: aegis_ledger::IssueId::parse(format!("pilot-{n}")).unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
    .unwrap();
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    assert!(claim.job.shadow);
    let key = claim.job.ledger_key.clone();
    if kept {
        let event = LedgerEvent::ShadowKept(ShadowKept {
            answer:
                r#"{"status": "fixed", "changes": [{"location": "src/lib.rs:1", "note": "b"}]}"#
                    .into(),
            base: "0123456789abcdef0123456789abcdef01234567".into(),
        });
        s.ledger().append(&key, &event).await.unwrap();
    }
    let finished = LedgerEvent::Finished {
        outcome: Outcome::Shadowed,
        wall_time_s: 60.0,
    };
    s.ledger().append(&key, &finished).await.unwrap();
    let end = AttemptEnd::Done {
        outcome: Outcome::Shadowed,
        reason: Some("shadow mode: the gate passed, no PR opened".into()),
        pr_url: None,
        patch: Some(Patch {
            diff: DIFF.into(),
            truncated,
        }),
        diagnosis: None,
    };
    s.finish_attempt(&claim.run, "w", &end).await.unwrap();
    claim.job.id
}

#[tokio::test]
async fn a_maintainer_opens_a_shadow_job_s_pr_once_from_its_page() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = shadow_repo(s).await;
    let id = shadowed(s, repo, 1, true, false).await;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router(s.clone(), common::auth(s));

    let (status, page) = get(&app, &admin, &format!("/jobs/{}", id.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains(&format!("action=\"/jobs/{}/open-pr\"", id.0)),
        "{page}"
    );
    assert!(page.contains("Open a PR from this patch"), "{page}");

    let (status, location, body) = post(&app, &admin, &format!("/jobs/{}/open-pr", id.0)).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let new_id: i64 = location
        .strip_prefix("/jobs/")
        .and_then(|l| l.strip_suffix("?done=pr_asked"))
        .unwrap()
        .parse()
        .unwrap();
    let asked = s.job(JobId(new_id)).await.unwrap().unwrap();
    // Its own job and ledger, on the same ticket, not a shadow job though
    // the repo still is in shadow mode.
    assert_eq!(asked.publish_of, Some(id));
    assert!(!asked.shadow);
    assert_eq!(asked.ledger_key.as_str(), "PILOT-1-PR");
    assert_eq!(
        (asked.issue_ref.as_str(), asked.source),
        ("x/pilot#1", JobSource::Github)
    );
    let (_, page) = get(&app, &admin, &format!("/jobs/{new_id}")).await;
    assert!(page.contains("Opens the PR of shadow job"), "{page}");
    let audit = s
        .audit_log(Some(repo), aegis_store::Page::first(20))
        .await
        .unwrap();
    assert!(
        audit
            .items
            .iter()
            .any(|e| e.action == "job.publish_shadow" && e.detail["shadow_job"] == id.0),
        "{:?}",
        audit.items
    );

    // Once: the button is gone, and asking again is refused.
    let (_, page) = get(&app, &admin, &format!("/jobs/{}", id.0)).await;
    assert!(!page.contains("Open a PR from this patch"), "{page}");
    let (status, _, body) = post(&app, &admin, &format!("/jobs/{}/open-pr", id.0)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // Asked again after it failed, it still opens that shadow job's PR.
    s.request_cancel(asked.id).await.unwrap();
    let again = s.retry_job(asked.id).await.unwrap();
    assert_eq!((again.publish_of, again.shadow), (Some(id), false));
    db.drop().await;
}

#[tokio::test]
async fn a_pr_is_asked_only_of_a_shadow_job_that_kept_a_whole_patch_and_its_answer() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = shadow_repo(s).await;
    let actor = aegis_store::Actor::system();
    let cut = shadowed(s, repo, 1, true, true).await;
    let unkept = shadowed(s, repo, 2, false, false).await;
    for (id, why) in [(cut, "too large"), (unkept, "no answer")] {
        let job = s.job(id).await.unwrap().unwrap();
        let refusal = s.shadow_publish_refusal(&job).await.unwrap().unwrap();
        assert!(refusal.contains(why), "{refusal}");
        let err = s.publish_shadow_by(id, &actor).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "{err}");
    }
    // A job that is not a shadow job's end.
    let queued = s
        .create_job(&NewJob {
            repo_id: repo,
            issue_ref: "x/pilot#3".into(),
            ledger_key: aegis_ledger::IssueId::parse("pilot-3").unwrap(),
            source: JobSource::Github,
            priority: None,
            sla_deadline: None,
        })
        .await
        .unwrap();
    let err = s.publish_shadow_by(queued.id, &actor).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");
    db.drop().await;
}
