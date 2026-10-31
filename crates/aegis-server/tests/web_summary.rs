//! #316: a job's page and the job list say what the job works on (the
//! issue's title, linked) and what the agent did or why it stopped, from
//! the ledger, escaped like everything else.

mod common;

use aegis_ledger::{AnswerChange, AnswerSummary, IssueId, LedgerEvent, LedgerStore};
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{JobId, JobSource, NewJob, NewRepo, RepoId, RepoMode, RepoRole, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

async fn get(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

/// A job whose ledger holds `events`.
async fn job(s: &Store, repo: RepoId, key: &str, events: &[LedgerEvent]) -> JobId {
    let id = IssueId::parse(key).unwrap();
    let job = s
        .create_job(&NewJob {
            repo_id: repo,
            issue_ref: format!("x/y#{key}"),
            ledger_key: id.clone(),
            source: JobSource::Github,
        })
        .await
        .unwrap();
    for e in events {
        s.ledger().append(&id, e).await.unwrap();
    }
    job.id
}

fn read(title: &str, url: &str) -> LedgerEvent {
    LedgerEvent::IssueRead {
        title: title.into(),
        url: url.into(),
    }
}

fn fixed() -> AnswerSummary {
    AnswerSummary {
        fixed: true,
        cause: "`add` compared with `>` so a <b>fourth</b> item was accepted.".into(),
        changes: vec![AnswerChange {
            location: "src/lib.rs:24".into(),
            note: "compare with >=".into(),
        }],
        repro_test: Some("fourth_item_is_refused".into()),
        stop_reason: None,
        review_replies: vec![],
    }
}

#[tokio::test]
async fn the_page_says_what_the_job_works_on_and_what_the_agent_did() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b) = (repo(s, "repo-a").await, repo(s, "repo-b").await);
    common::grant(s, "view@x.test", a, RepoRole::Viewer).await;
    let who = common::sign_in(s, "view@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let id = job(
        s,
        a,
        "a-1",
        &[
            read(
                "Cart accepts a <script>fourth</script> item",
                "https://github.com/x/y/issues/1",
            ),
            LedgerEvent::Answered(fixed()),
        ],
    )
    .await;

    let (status, page) = get(&app, &who, &format!("/jobs/{}", id.0)).await;
    assert_eq!(status, StatusCode::OK);
    // The title, escaped, and the issue ref as a link to it.
    assert!(
        page.contains("Cart accepts a &#60;script&#62;fourth&#60;/script&#62; item"),
        "{page}"
    );
    assert!(!page.contains("<script>fourth"), "{page}");
    assert!(
        page.contains("href=\"https://github.com/x/y/issues/1\""),
        "{page}"
    );
    // What the agent did: cause, changes, reproduction test.
    assert!(page.contains("What the agent did"), "{page}");
    assert!(
        page.contains("a &#60;b&#62;fourth&#60;/b&#62; item"),
        "{page}"
    );
    assert!(
        page.contains("src/lib.rs:24") && page.contains("compare with &#62;="),
        "{page}"
    );
    assert!(page.contains("fourth_item_is_refused"), "{page}");

    // The list shows the title too.
    let (_, list) = get(&app, &who, "/jobs").await;
    assert!(list.contains("Cart accepts a &#60;script&#62;"), "{list}");

    // Another repo's job stays hidden.
    let other = job(
        s,
        b,
        "b-1",
        &[read("Secret", "https://github.com/x/z/issues/2")],
    )
    .await;
    let (status, _) = get(&app, &who, &format!("/jobs/{}", other.0)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    db.drop().await;
}

#[tokio::test]
async fn a_stopped_answer_says_why_and_odd_urls_are_not_links() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::grant(s, "view@x.test", a, RepoRole::Viewer).await;
    let who = common::sign_in(s, "view@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let stopped = AnswerSummary {
        fixed: false,
        cause: String::new(),
        changes: vec![],
        repro_test: None,
        stop_reason: Some("needs a design decision".into()),
        review_replies: vec![],
    };
    let id = job(
        s,
        a,
        "a-1",
        &[
            read("A title", "javascript:alert(1)"),
            LedgerEvent::Answered(stopped),
        ],
    )
    .await;
    let (_, page) = get(&app, &who, &format!("/jobs/{}", id.0)).await;
    assert!(page.contains("needs a design decision"), "{page}");
    assert!(!page.contains("javascript:alert"), "{page}");

    // A job not yet read shows neither, and still renders.
    let fresh = job(s, a, "a-2", &[]).await;
    let (status, page) = get(&app, &who, &format!("/jobs/{}", fresh.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!page.contains("What the agent did"), "{page}");
    db.drop().await;
}
