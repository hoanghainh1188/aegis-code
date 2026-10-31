//! #310: a job's page and the job list keep themselves up to date while a
//! job is queued or running, through htmx polling only, and stop once
//! nothing is left to wait for.

mod common;

use aegis_ledger::IssueId;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{JobEnd, JobId, JobSource, NewJob, NewRepo, RepoId, RepoMode, RepoRole, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

/// A GET as htmx sends it when `htmx` is true.
async fn get(app: &Router, who: &Browser, uri: &str, htmx: bool) -> Answer {
    let mut request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie);
    if htmx {
        request = request.header("hx-request", "true");
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
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

async fn queued(s: &Store, repo: RepoId, key: &str) -> JobId {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("x/y#{key}"),
        ledger_key: IssueId::parse(key).unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap()
    .id
}

const POLL: &str = "hx-trigger=\"every 5s\"";

#[tokio::test]
async fn a_live_job_s_head_reloads_itself_until_it_ends() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b) = (repo(s, "repo-a").await, repo(s, "repo-b").await);
    common::grant(s, "view@x.test", a, RepoRole::Viewer).await;
    let who = common::sign_in(s, "view@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let job = queued(s, a, "a-1").await;
    let head = format!("/jobs/{}/head?live=1", job.0);

    // The page asks for its head while the job is queued.
    let page = get(&app, &who, &format!("/jobs/{}", job.0), false).await;
    assert!(
        page.body.contains(&format!("hx-get=\"{head}\"")) && page.body.contains(POLL),
        "{}",
        page.body
    );
    let live = get(&app, &who, &head, true).await;
    assert_eq!(live.status, StatusCode::OK);
    assert!(live.body.contains(POLL), "{}", live.body);
    assert!(live.body.contains("id=\"job-head\""), "{}", live.body);
    assert!(!live.body.contains("<html"), "a fragment, not a page");

    // Once it ends, the poll reloads the whole page once, which then
    // stops asking.
    s.end_job(job, JobEnd::Failed).await.unwrap();
    let ended = get(&app, &who, &head, true).await;
    assert_eq!(ended.status, StatusCode::OK);
    assert_eq!(ended.headers["hx-refresh"], "true");
    let page = get(&app, &who, &format!("/jobs/{}", job.0), false).await;
    assert!(!page.body.contains(POLL), "{}", page.body);

    // A job of a repo they cannot see is not there.
    let other = queued(s, b, "b-1").await;
    let hidden = get(&app, &who, &format!("/jobs/{}/head?live=1", other.0), true).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

#[tokio::test]
async fn the_job_list_reloads_while_a_job_is_live() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::grant(s, "view@x.test", a, RepoRole::Viewer).await;
    let who = common::sign_in(s, "view@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let done = queued(s, a, "a-1").await;
    s.end_job(done, JobEnd::Cancelled).await.unwrap();

    // Nothing live: no poll.
    let page = get(&app, &who, "/jobs", false).await;
    assert!(!page.body.contains(POLL), "{}", page.body);

    // A queued job: the table polls itself, filters kept, on the page and
    // in the table htmx swaps in.
    queued(s, a, "a-2").await;
    let uri = format!("/jobs?repo_id={}&status=", a.0);
    let page = get(&app, &who, &uri, false).await;
    assert!(page.body.contains(POLL), "{}", page.body);
    // askama writes `&` as `&#38;`; the browser reads it back.
    let poll = format!("hx-get=\"/jobs?repo_id={}&#38;status=&#38;cursor=\"", a.0);
    assert!(page.body.contains(&poll), "{}", page.body);
    let table = get(&app, &who, &uri, true).await;
    assert!(
        table.body.contains(POLL) && !table.body.contains("<html"),
        "{}",
        table.body
    );
    db.drop().await;
}
