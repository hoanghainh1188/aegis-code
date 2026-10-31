//! `aegis` against a fake `/api/v1` (#231): the token goes as a
//! bearer, a slug is resolved among the caller's repos, `watch` ends with
//! the job and says whether it opened a PR, and the server's errors are
//! shown with a hint.

use aegis_client::api::Client;
use aegis_client::cli::{Exit, run_with};
use aegis_client::config::Login;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok(data: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "data": data, "error": null, "meta": null }))
}

fn job(id: i64, status: &str, pr_url: Option<&str>) -> serde_json::Value {
    json!({
        "id": id, "repo_id": 1, "issue_ref": "demo-org/demo#1", "ledger_key": "K",
        "source": "github", "status": status,
        "outcome": if pr_url.is_some() { json!("pr_opened") } else { json!(null) },
        "reason": null, "pr_url": pr_url, "attempts": 1, "retries": 0,
        "cancel_requested": false, "retry_of": null, "review_of": null, "review_round": 0,
        "locked_by": null, "locked_until": null, "run_after": "t",
        "created_at": "2026-10-28T10:00:00Z", "updated_at": "2026-10-28T10:05:00Z"
    })
}

/// Runs `args` as a logged-in `aegis`, off the async runtime.
async fn aegis(server: &MockServer, args: &[&str]) -> (Result<Exit, String>, String) {
    let login = Login {
        server: server.uri(),
        token: "tok-lan".into(),
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        let client = Client::new(login).unwrap();
        let mut out = Vec::new();
        let json = args.iter().any(|a| a == "--json");
        let args: Vec<String> = args.into_iter().filter(|a| a != "--json").collect();
        let exit = run_with(&client, &args[0], args[1..].to_vec(), json, &mut out);
        (exit, String::from_utf8(out).unwrap())
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_developer_submits_an_issue_and_watches_it_to_its_pr() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos"))
        .and(header("authorization", "Bearer tok-lan"))
        .respond_with(ok(
            json!([{ "id": 1, "slug": "demo", "url": "https://github.com/demo-org/demo",
            "default_branch": "main", "language": "go", "role": "maintainer" }]),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/jobs"))
        .and(body_json(
            json!({ "repo_id": 1, "issue_ref": "demo-org/demo#1", "source": "github" }),
        ))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(
                json!({ "data": job(7, "queued", None), "error": null, "meta": null }),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (exit, out) = aegis(&server, &["submit", "demo", "demo-org/demo#1"]).await;
    assert_eq!(exit, Ok(Exit::Ok), "{out}");
    assert!(
        out.contains("job #7 queued for demo-org/demo#1 in demo"),
        "{out}"
    );

    Mock::given(method("GET"))
        .and(path("/api/v1/jobs/7"))
        .respond_with(ok(job(
            7,
            "done",
            Some("https://github.com/bot/demo/pull/3"),
        )))
        .mount(&server)
        .await;
    let (exit, out) = aegis(&server, &["watch", "7", "--every", "1"]).await;
    assert_eq!(exit, Ok(Exit::Ok));
    assert!(
        out.contains("#7      done      demo-org/demo#1  PR https://github.com/bot/demo/pull/3"),
        "{out}"
    );

    let (exit, _) = aegis(&server, &["submit", "nope", "demo-org/demo#1"]).await;
    assert!(exit.unwrap_err().contains("no repo `nope`"));
}

#[tokio::test]
async fn a_job_without_a_pr_ends_watch_with_1_and_errors_say_why() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/jobs/8"))
        .respond_with(ok(job(8, "failed", None)))
        .mount(&server)
        .await;
    let (exit, _) = aegis(&server, &["watch", "8"]).await;
    assert_eq!(exit, Ok(Exit::NoPr));

    Mock::given(method("GET"))
        .and(path("/api/v1/jobs"))
        .and(query_param("status", "running"))
        .respond_with(ok(json!([job(9, "running", None)])))
        .mount(&server)
        .await;
    let (exit, out) = aegis(&server, &["list", "--status", "running", "--json"]).await;
    assert_eq!(exit, Ok(Exit::Ok));
    let listed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(listed[0]["id"], 9);

    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "data": null, "error": { "code": "unauthorized", "message": "an API token is needed" }, "meta": null
        })))
        .mount(&server)
        .await;
    let (exit, _) = aegis(&server, &["whoami"]).await;
    let err = exit.unwrap_err();
    assert!(
        err.contains("unauthorized") && err.contains("aegis login"),
        "{err}"
    );
    let (exit, _) = aegis(&server, &["frobnicate"]).await;
    assert!(exit.unwrap_err().contains("unknown command"));
}
