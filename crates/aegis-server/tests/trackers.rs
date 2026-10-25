//! Jira and Backlog tickets (#26) against a real database, with the
//! trackers played by mock servers: polling queues one job per ticket,
//! the runner reads the ticket with its logs, the end is reported on it.

use aegis_ledger::{IssueId, Outcome};
use aegis_secrets::Secrets;
use aegis_server::runner::Attempt;
use aegis_server::tracker::{TrackerNotifier, poll_if_due, poll_once, ticket_issue};
use aegis_server::worker::JobNotifier;
use aegis_store::testing::test_db;
use aegis_store::{
    Job, JobFilter, JobId, JobSource, JobStatus, NewRepo, Page, Repo, RepoMode, Store,
};
use chrono::Utc;
use serde_json::{Value, json};
use wiremock::matchers::{basic_auth, body_partial_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const JIRA_TOKEN: &str = "AEGIS_JIRA_TOKEN";
const BACKLOG_KEY: &str = "AEGIS_BACKLOG_API_KEY";

fn secrets() -> Secrets {
    Secrets::fixed([(JIRA_TOKEN, "jira-tok"), (BACKLOG_KEY, "backlog-key")])
}

async fn repo(s: &Store, slug: &str, tracker: Option<Value>) -> Repo {
    let mut config = json!({
        "fork": { "owner": "me", "name": slug, "token_env": "GITHUB_TOKEN" }
    });
    if let Some(t) = tracker {
        config["tracker"] = t;
    }
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config,
    })
    .await
    .unwrap()
}

fn jira(server: &MockServer, extra: Value) -> Value {
    let mut t = json!({
        "kind": "jira", "url": server.uri(), "user": "bot@acme.com",
        "token_env": JIRA_TOKEN, "jql": "project = APP AND labels = aegis",
    });
    for (k, v) in extra.as_object().unwrap() {
        t[k] = v.clone();
    }
    t
}

fn backlog(server: &MockServer) -> Value {
    json!({
        "kind": "backlog", "url": server.uri(), "token_env": BACKLOG_KEY,
        "project_ids": [12], "pr_status_id": 3,
    })
}

async fn jobs(s: &Store) -> Vec<Job> {
    let mut jobs = s
        .list_jobs(&JobFilter::default(), Page::first(100))
        .await
        .unwrap()
        .items;
    jobs.sort_by_key(|j| j.id.0);
    jobs
}

fn ended(id: JobId, repo: &Repo, source: JobSource, key: &str, pr: Option<&str>) -> Job {
    Job {
        id,
        repo_id: repo.id,
        issue_ref: key.into(),
        ledger_key: IssueId::parse(format!("{}-1", key.replace(['/', '#'], "-"))).unwrap(),
        source,
        status: JobStatus::Done,
        outcome: Some(if pr.is_some() {
            Outcome::PrOpened
        } else {
            Outcome::StoppedSteps
        }),
        attempts: 1,
        locked_by: None,
        locked_until: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        run_after: Utc::now(),
        retries: 0,
        cancel_requested: false,
        reason: None,
        pr_url: pr.map(str::to_owned),
        retry_of: None,
    }
}

#[tokio::test]
async fn polling_queues_one_job_per_ticket_of_each_tracker() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let jira_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search/jql"))
        .and(basic_auth("bot@acme.com", "jira-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [{ "key": "APP-1" }, { "key": "APP-2" }], "isLast": true
        })))
        .mount(&jira_server)
        .await;
    let backlog_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/issues"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([{ "id": 9, "issueKey": "OPS-9" }])),
        )
        .mount(&backlog_server)
        .await;
    let app = repo(s, "app", Some(jira(&jira_server, json!({})))).await;
    let ops = repo(s, "ops", Some(backlog(&backlog_server))).await;
    repo(s, "plain", None).await;
    // A repo whose secret is missing is skipped, not an error.
    let mut nameless = jira(&jira_server, json!({}));
    nameless["token_env"] = json!("AEGIS_NO_SUCH_TOKEN");
    repo(s, "nameless", Some(nameless)).await;

    assert!(poll_once(s, &secrets()).await.unwrap().is_empty());
    let queued = jobs(s).await;
    let seen: Vec<(i64, &str, JobSource, JobStatus)> = queued
        .iter()
        .map(|j| (j.repo_id.0, j.issue_ref.as_str(), j.source, j.status))
        .collect();
    assert_eq!(
        seen,
        [
            (app.id.0, "APP-1", JobSource::Jira, JobStatus::Queued),
            (app.id.0, "APP-2", JobSource::Jira, JobStatus::Queued),
            (ops.id.0, "OPS-9", JobSource::Backlog, JobStatus::Queued),
        ]
    );

    // The tickets still match: nothing more, also once their jobs ended.
    s.request_cancel(queued[0].id).await.unwrap();
    poll_once(s, &secrets()).await.unwrap();
    assert_eq!(jobs(s).await.len(), 3);
}

#[tokio::test]
async fn the_runner_reads_the_ticket_with_its_logs() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/APP-7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": "APP-7",
            "fields": {
                "summary": "Crash on save",
                "description": "Saving crashes.",
                "attachment": [{
                    "id": "10", "filename": "app.log", "mimeType": "text/plain", "size": 9,
                    "content": format!("{}/attachment/10", server.uri())
                }]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/attachment/10"))
        .respond_with(ResponseTemplate::new(200).set_body_string("panic: x"))
        .mount(&server)
        .await;
    let app = repo(s, "app", Some(jira(&server, json!({})))).await;
    let job = ended(JobId(1), &app, JobSource::Jira, "APP-7", None);

    let issue = ticket_issue(&job, &app, &secrets()).await.unwrap();
    assert_eq!(issue.id, job.ledger_key.as_str());
    assert_eq!(issue.title, "Crash on save");
    assert_eq!(issue.url, format!("{}/browse/APP-7", server.uri()));
    assert!(issue.body.starts_with("Saving crashes."), "{}", issue.body);
    assert!(issue.body.contains("app.log") && issue.body.contains("panic: x"));

    // Jira failing: retried. A ticket that is gone, no tracker or no
    // secret: retrying will not help.
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/APP-8"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let failing = Job {
        issue_ref: "APP-8".into(),
        ..job.clone()
    };
    assert!(matches!(
        ticket_issue(&failing, &app, &secrets()).await,
        Err(Attempt::Infra(_))
    ));
    let gone = Job {
        issue_ref: "APP-9".into(),
        ..job.clone()
    };
    assert!(matches!(
        ticket_issue(&gone, &app, &secrets()).await,
        Err(Attempt::Rejected(_))
    ));
    let plain = repo(s, "plain", None).await;
    assert!(matches!(
        ticket_issue(&job, &plain, &secrets()).await,
        Err(Attempt::Rejected(_))
    ));
    assert!(matches!(
        ticket_issue(&job, &app, &Secrets::fixed([])).await,
        Err(Attempt::Rejected(e)) if e.contains(JIRA_TOKEN)
    ));
}

#[tokio::test]
async fn the_end_of_a_job_is_written_on_its_ticket() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pr = "https://github.com/me/app/pull/4";
    let jira_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/APP-7/comment"))
        .and(body_string_contains("opened a pull request"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "1" })))
        .expect(1)
        .mount(&jira_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/APP-7/remotelink"))
        .and(body_partial_json(json!({ "object": { "url": pr } })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
        .expect(1)
        .mount(&jira_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/APP-7/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transitions": [{ "id": "31", "name": "In Review" }]
        })))
        .mount(&jira_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/APP-7/transitions"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&jira_server)
        .await;
    let backlog_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/issues/OPS-9/comments"))
        .and(body_string_contains("step+limit"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
        .expect(1)
        .mount(&backlog_server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v2/issues/OPS-9"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&backlog_server)
        .await;
    let app = repo(
        s,
        "app",
        Some(jira(&jira_server, json!({ "pr_transition": "In Review" }))),
    )
    .await;
    let ops = repo(s, "ops", Some(backlog(&backlog_server))).await;

    let notifier = TrackerNotifier::new(s.clone(), secrets());
    notifier
        .job_ended(&ended(JobId(1), &app, JobSource::Jira, "APP-7", Some(pr)))
        .await;
    notifier
        .job_ended(&ended(JobId(2), &ops, JobSource::Backlog, "OPS-9", None))
        .await;
    // Jobs from elsewhere are not its business.
    notifier
        .job_ended(&ended(
            JobId(3),
            &app,
            JobSource::Github,
            "me/app#1",
            Some(pr),
        ))
        .await;
}

#[tokio::test]
async fn one_poll_per_period_whoever_asks() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [{ "key": "APP-1" }], "isLast": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    repo(s, "app", Some(jira(&server, json!({})))).await;
    let hour = std::time::Duration::from_secs(3600);
    poll_if_due(s, &secrets(), hour).await.unwrap();
    // Another process, or the same one early: not due.
    poll_if_due(s, &secrets(), hour).await.unwrap();
    assert_eq!(jobs(s).await.len(), 1);
}
