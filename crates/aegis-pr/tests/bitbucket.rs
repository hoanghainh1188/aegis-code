//! The Bitbucket Cloud forge (#634) against a fake API: opening the
//! agent's PR, commenting on it, and what a review round reads of it.

use aegis_pr::{AgentBranch, Bitbucket, BitbucketPermission, Forge, NoteAt, PrError};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "/2.0/repositories/acme/app";

fn bitbucket(server: &MockServer) -> Bitbucket {
    Bitbucket::new(&format!("{}/2.0", server.uri()), "acme", "app", "t").unwrap()
}

#[tokio::test]
async fn opens_a_draft_pull_request_from_the_agents_branch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pullrequests")))
        .and(header("authorization", "Bearer t"))
        .and(body_partial_json(json!({
            "title": "Fix the cart",
            "description": "body",
            "source": { "branch": { "name": "agent/7" } },
            "destination": { "branch": { "name": "main" } },
            "close_source_branch": false,
            "draft": true,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 12,
            "links": { "html": { "href": "https://bitbucket.org/acme/app/pull-requests/12" } }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let forge = Forge::Bitbucket(bitbucket(&server));
    let head = AgentBranch::for_issue("7").unwrap();
    let url = forge
        .open_request(&head, "main", "Fix the cart", "body", true)
        .await
        .unwrap();
    assert_eq!(url, "https://bitbucket.org/acme/app/pull-requests/12");
}

#[tokio::test]
async fn comments_on_a_pull_request_with_secrets_masked() {
    let server = MockServer::start().await;
    let token = format!("ghp_{}", "Q".repeat(36));
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pullrequests/12/comments")))
        .and(body_partial_json(json!({
            "content": { "raw": "done; [REDACTED:github-token]" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 5 })))
        .expect(1)
        .mount(&server)
        .await;
    let forge = Forge::Bitbucket(bitbucket(&server));
    forge
        .comment_on_request(12, &format!("done; {token}"))
        .await
        .unwrap();

    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pullrequests/13/comments")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "type": "error", "error": { "message": "No such pull request" }
        })))
        .mount(&server)
        .await;
    let err = forge.comment_on_request(13, "done").await.unwrap_err();
    assert!(
        matches!(&err, PrError::Bitbucket { status: 404, message } if message == "No such pull request"),
        "{err}"
    );
}

fn pull(state: &str, branch: &str, repo: &str) -> serde_json::Value {
    json!({
        "id": 12,
        "state": state,
        "source": {
            "branch": { "name": branch },
            "commit": { "hash": "0123456789ab" },
            "repository": { "full_name": repo }
        },
        "destination": { "branch": { "name": "main" } },
        "links": { "html": { "href": "https://bitbucket.org/acme/app/pull-requests/12" } }
    })
}

#[tokio::test]
async fn a_round_reads_an_open_agent_pull_with_its_full_head() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pullrequests/12")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull("OPEN", "agent/7", "acme/app")))
        .mount(&server)
        .await;
    let full = "0123456789abcdef0123456789abcdef01234567";
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/commit/0123456789ab")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "hash": full })))
        .mount(&server)
        .await;
    let state = bitbucket(&server).agent_pull(12).await.unwrap();
    assert_eq!(state.number, 12);
    assert_eq!(state.head_ref, "agent/7");
    assert_eq!(state.head_sha, full);
    assert_eq!(state.base_ref, "main");
    assert_eq!(state.url, "https://bitbucket.org/acme/app/pull-requests/12");
}

#[tokio::test]
async fn a_round_refuses_a_pull_that_is_not_the_agents() {
    for (state, branch, repo) in [
        ("MERGED", "agent/7", "acme/app"),
        ("OPEN", "feature/x", "acme/app"),
        ("OPEN", "agent/7", "someone/app"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{REPO}/pullrequests/12")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pull(state, branch, repo)))
            .mount(&server)
            .await;
        let err = bitbucket(&server).agent_pull(12).await.unwrap_err();
        assert!(
            matches!(err, PrError::Bitbucket { status: 0, .. }),
            "{state} {branch} {repo}: {err}"
        );
    }
}

fn comment(uuid: &str, nick: &str, raw: &str, at: &str) -> serde_json::Value {
    json!({
        "user": { "uuid": uuid, "nickname": nick, "type": "user" },
        "content": { "raw": raw },
        "created_on": at,
    })
}

#[tokio::test]
async fn only_writers_comments_are_read_across_pages() {
    let server = MockServer::start().await;
    let next = format!(
        "{}{REPO}/pullrequests/12/comments?pagelen=100&page=2",
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pullrequests/12/comments")))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [
                comment("{w}", "rev", "Use a rune slice.", "2026-11-02T00:00:00Z"),
                {
                    "user": { "uuid": "{w}", "nickname": "rev", "type": "user" },
                    "content": { "raw": "Off by one here." },
                    "inline": { "path": "src/cart.rs", "to": 14 },
                    "created_on": "2026-11-03T00:00:00Z"
                },
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pullrequests/12/comments")))
        .and(query_param("pagelen", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [
                comment("{r}", "drive-by", "Please add my package.", "2026-11-01T00:00:00Z"),
                comment("{w}", "rev", "<!-- aegis-code review reply -->\nDone.", "2026-11-01T01:00:00Z"),
                {
                    "user": { "uuid": "{w}", "nickname": "rev", "type": "user" },
                    "content": { "raw": "deleted" },
                    "deleted": true,
                    "created_on": "2026-11-01T02:00:00Z"
                },
            ],
            "next": next,
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/permissions-config/users/%7Bw%7D")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "permission": "write" })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/permissions-config/users/%7Br%7D")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "permission": "read" })))
        .mount(&server)
        .await;
    let notes = bitbucket(&server).review_notes(12).await.unwrap();
    let bodies: Vec<_> = notes.iter().map(|n| n.body.as_str()).collect();
    assert_eq!(bodies, ["Use a rune slice.", "Off by one here."]);
    assert_eq!(notes[0].author, "rev");
    assert_eq!(notes[0].at, NoteAt::Conversation);
    assert_eq!(
        notes[1].at,
        NoteAt::Line {
            path: "src/cart.rs".into(),
            line: Some(14)
        }
    );
}

#[tokio::test]
async fn someone_unknown_to_the_repo_may_do_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/permissions-config/users/%7Bx%7D")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "type": "error", "error": { "message": "not found" }
        })))
        .mount(&server)
        .await;
    let bitbucket = bitbucket(&server);
    assert_eq!(
        bitbucket.permission("{x}").await.unwrap(),
        BitbucketPermission::None
    );
    assert_eq!(
        bitbucket.permission("../../x").await.unwrap(),
        BitbucketPermission::None,
        "never sent"
    );
}
