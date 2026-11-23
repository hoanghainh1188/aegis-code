//! The Bitbucket Data Center forge (#635) against a fake API: opening the
//! agent's PR, commenting on it, and what a review round reads of it.

use aegis_pr::{AgentBranch, BitbucketDataCenter, Forge, NoteAt, PrError};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "/rest/api/1.0/projects/PAY/repos/cart";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn dc(server: &MockServer) -> BitbucketDataCenter {
    BitbucketDataCenter::new(
        &format!("{}/rest/api/1.0", server.uri()),
        "PAY",
        "cart",
        "t",
    )
    .unwrap()
}

fn link(id: u64) -> String {
    format!("https://git.acme.example/projects/PAY/repos/cart/pull-requests/{id}")
}

#[tokio::test]
async fn opens_a_pull_request_from_the_agents_branch() {
    let server = MockServer::start().await;
    let repo = json!({ "slug": "cart", "project": { "key": "PAY" } });
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pull-requests")))
        .and(header("authorization", "Bearer t"))
        .and(body_partial_json(json!({
            "title": "Fix the cart",
            "description": "body",
            "draft": false,
            "fromRef": { "id": "refs/heads/agent/7", "repository": repo },
            "toRef": { "id": "refs/heads/main", "repository": repo },
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 12, "links": { "self": [{ "href": link(12) }] }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let forge = Forge::BitbucketDataCenter(dc(&server));
    let head = AgentBranch::for_issue("7").unwrap();
    let url = forge
        .open_request(&head, "main", "Fix the cart", "body", false)
        .await
        .unwrap();
    assert_eq!(url, link(12));
}

#[tokio::test]
async fn comments_on_a_pull_request_and_says_why_one_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pull-requests/12/comments")))
        .and(body_partial_json(json!({ "text": "done" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 5 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pull-requests/13/comments")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "errors": [{ "message": "Pull request 13 does not exist in PAY/cart." }]
        })))
        .mount(&server)
        .await;
    let forge = Forge::BitbucketDataCenter(dc(&server));
    forge.comment_on_request(12, "done").await.unwrap();
    let err = forge.comment_on_request(13, "done").await.unwrap_err();
    assert!(
        matches!(&err, PrError::Bitbucket { status: 404, message } if message.contains("does not exist")),
        "{err}"
    );
}

fn pull(state: &str, branch: &str, project: &str) -> serde_json::Value {
    json!({
        "id": 12,
        "state": state,
        "fromRef": {
            "displayId": branch, "latestCommit": HEAD,
            "repository": { "slug": "cart", "project": { "key": project } }
        },
        "toRef": {
            "displayId": "main",
            "repository": { "slug": "cart", "project": { "key": "PAY" } }
        },
        "links": { "self": [{ "href": link(12) }] }
    })
}

#[tokio::test]
async fn a_round_reads_an_open_agent_pull_and_refuses_others() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pull-requests/12")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull("OPEN", "agent/7", "PAY")))
        .mount(&server)
        .await;
    let state = dc(&server).agent_pull(12).await.unwrap();
    assert_eq!(
        (
            state.number,
            state.head_ref.as_str(),
            state.head_sha.as_str()
        ),
        (12, "agent/7", HEAD)
    );
    assert_eq!((state.base_ref.as_str(), state.url), ("main", link(12)));

    for (state, branch, project) in [
        ("MERGED", "agent/7", "PAY"),
        ("OPEN", "feature/x", "PAY"),
        ("OPEN", "agent/7", "OTHER"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{REPO}/pull-requests/12")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pull(state, branch, project)))
            .mount(&server)
            .await;
        let err = dc(&server).agent_pull(12).await.unwrap_err();
        assert!(
            matches!(err, PrError::Bitbucket { status: 0, .. }),
            "{state} {branch} {project}: {err}"
        );
    }
}

fn commented(slug: &str, kind: &str, text: &str, at: i64) -> serde_json::Value {
    json!({
        "action": "COMMENTED",
        "comment": { "text": text, "author": { "slug": slug, "type": kind }, "createdDate": at }
    })
}

#[tokio::test]
async fn only_writers_comments_are_read_across_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pull-requests/12/activities")))
        .and(query_param("start", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [
                commented("drive-by", "NORMAL", "Please add my package.", 1_000),
                commented("ci-bot", "SERVICE", "Build passed.", 2_000),
                { "action": "APPROVED" },
                commented("rev", "NORMAL", "<!-- aegis-code review reply -->\nDone.", 3_000),
            ],
            "isLastPage": false,
            "nextPageStart": 4,
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pull-requests/12/activities")))
        .and(query_param("start", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [
                commented("rev", "NORMAL", "Use a rune slice.", 5_000),
                {
                    "action": "COMMENTED",
                    "comment": { "text": "Off by one here.", "author": { "slug": "rev", "type": "NORMAL" }, "createdDate": 6_000 },
                    "commentAnchor": { "path": "src/cart.rs", "line": 14 }
                },
            ],
            "isLastPage": true,
        })))
        .mount(&server)
        .await;
    for (slug, writes) in [("rev", true), ("drive-by", false)] {
        let values = if writes {
            json!([{ "slug": slug, "name": slug }])
        } else {
            json!([])
        };
        Mock::given(method("GET"))
            .and(path("/rest/api/1.0/users"))
            .and(query_param("filter", slug))
            .and(query_param("permission.1", "REPO_WRITE"))
            .and(query_param("permission.1.projectKey", "PAY"))
            .and(query_param("permission.1.repositorySlug", "cart"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "values": values, "isLastPage": true })),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let notes = dc(&server).review_notes(12).await.unwrap();
    let bodies: Vec<_> = notes.iter().map(|n| n.body.as_str()).collect();
    assert_eq!(bodies, ["Use a rune slice.", "Off by one here."]);
    assert_eq!(notes[0].author, "rev");
    assert_eq!(notes[0].created_at, "1970-01-01T00:00:05Z");
    assert_eq!(
        notes[1].at,
        NoteAt::Line {
            path: "src/cart.rs".into(),
            line: Some(14)
        }
    );
}

#[tokio::test]
async fn the_repository_is_named_by_its_clone_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(REPO))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "slug": "cart", "project": { "key": "PAY" },
            "links": { "clone": [
                { "href": "ssh://git@git.acme.example:7999/pay/cart.git", "name": "ssh" },
                { "href": "https://git.acme.example/scm/pay/cart.git", "name": "http" }
            ] }
        })))
        .mount(&server)
        .await;
    let access = dc(&server).access().await.unwrap();
    assert_eq!(access.path, "PAY/cart");
    assert_eq!(access.web_url, "https://git.acme.example/scm/pay/cart.git");
    assert!(!access.archived);
    assert!(!dc(&server).can_write("../x").await.unwrap(), "never sent");
}
