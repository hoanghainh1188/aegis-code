//! The Azure DevOps forge (#636) against a fake API: opening the agent's
//! PR, commenting on it, what a review round reads of it, and a service
//! principal's token.

use aegis_pr::{AgentBranch, AzureDevOps, Forge, NoteAt, PrError, entra_token};
use serde_json::json;
use wiremock::matchers::{
    body_partial_json, body_string_contains, header, method, path, query_param,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "/acme/Shop/_apis/git/repositories/cart";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const WRITER: &str = "6f5e1b2a-0c3d-4e5f-8a9b-0c1d2e3f4a5b";
const READER: &str = "11111111-2222-3333-4444-555555555555";

fn azure(server: &MockServer, token: &str) -> AzureDevOps {
    AzureDevOps::new(&format!("{}/acme", server.uri()), "Shop", "cart", token).unwrap()
}

#[tokio::test]
async fn opens_a_pull_request_with_a_pat_as_basic_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pullrequests")))
        .and(query_param("api-version", "7.1"))
        // `aegis:pat`.
        .and(header("authorization", "Basic YWVnaXM6cGF0"))
        .and(body_partial_json(json!({
            "sourceRefName": "refs/heads/agent/7",
            "targetRefName": "refs/heads/main",
            "title": "Fix the cart",
            "description": "body",
            "isDraft": true,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "pullRequestId": 12 })))
        .expect(1)
        .mount(&server)
        .await;
    let forge = Forge::AzureDevOps(azure(&server, "pat"));
    let head = AgentBranch::for_issue("7").unwrap();
    let url = forge
        .open_request(&head, "main", "Fix the cart", "body", true)
        .await
        .unwrap();
    assert_eq!(
        url,
        format!("{}/acme/Shop/_git/cart/pullrequest/12", server.uri())
    );
}

#[tokio::test]
async fn comments_in_a_closed_thread_with_an_entra_token_and_says_why_one_fails() {
    let server = MockServer::start().await;
    let jwt = "eyJ0eXAi.eyJhdWQi.c2lnbmF0dXJl";
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pullRequests/12/threads")))
        .and(header("authorization", format!("Bearer {jwt}").as_str()))
        .and(body_partial_json(json!({
            "comments": [{ "parentCommentId": 0, "content": "done", "commentType": "text" }],
            "status": "closed",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 5 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/pullRequests/13/threads")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "TF401180: The requested pull request was not found."
        })))
        .mount(&server)
        .await;
    let forge = Forge::AzureDevOps(azure(&server, jwt));
    forge.comment_on_request(12, "done").await.unwrap();
    let err = forge.comment_on_request(13, "done").await.unwrap_err();
    assert!(
        matches!(&err, PrError::AzureDevOps { status: 404, message } if message.starts_with("TF401180")),
        "{err}"
    );
}

fn pull(status: &str, branch: &str, project: &str) -> serde_json::Value {
    json!({
        "pullRequestId": 12,
        "status": status,
        "sourceRefName": format!("refs/heads/{branch}"),
        "targetRefName": "refs/heads/main",
        "lastMergeSourceCommit": { "commitId": HEAD },
        "repository": { "name": "cart", "project": { "name": project } },
    })
}

#[tokio::test]
async fn a_round_reads_an_active_agent_pull_and_refuses_others() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pullrequests/12")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull("active", "agent/7", "Shop")))
        .mount(&server)
        .await;
    let state = azure(&server, "pat").agent_pull(12).await.unwrap();
    assert_eq!(
        (
            state.number,
            state.head_ref.as_str(),
            state.head_sha.as_str()
        ),
        (12, "agent/7", HEAD)
    );
    assert_eq!(state.base_ref, "main");

    let mut forked = pull("active", "agent/7", "Shop");
    forked["forkSource"] = json!({ "name": "refs/heads/agent/7" });
    for body in [
        pull("completed", "agent/7", "Shop"),
        pull("active", "feature/x", "Shop"),
        pull("active", "agent/7", "Other"),
        forked,
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{REPO}/pullrequests/12")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
            .mount(&server)
            .await;
        let err = azure(&server, "pat").agent_pull(12).await.unwrap_err();
        assert!(
            matches!(err, PrError::AzureDevOps { status: 0, .. }),
            "{body}: {err}"
        );
    }
}

fn comment(id: &str, name: &str, content: &str, at: &str) -> serde_json::Value {
    json!({
        "author": { "id": id, "uniqueName": name },
        "content": content,
        "publishedDate": at,
        "commentType": "text",
    })
}

/// The identity service says `WRITER` is in the project's Contributors
/// (through a team) and `READER` is in no writer group.
async fn identities(server: &MockServer) {
    for (id, groups) in [
        (
            WRITER,
            json!([
                "Microsoft.TeamFoundation.Identity;S-1-9-team",
                "Microsoft.TeamFoundation.Identity;S-1-9-contrib"
            ]),
        ),
        (
            READER,
            json!(["Microsoft.TeamFoundation.Identity;S-1-9-readers"]),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/acme/_apis/identities"))
            .and(query_param("identityIds", id))
            .and(query_param("queryMembership", "Expanded"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1, "value": [{ "id": id, "memberOf": groups }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }
    for (group, sid) in [
        ("Contributors", "S-1-9-contrib"),
        ("Project Administrators", "S-1-9-admins"),
    ] {
        Mock::given(method("GET"))
            .and(path("/acme/_apis/identities"))
            .and(query_param("searchFilter", "General"))
            .and(query_param("filterValue", format!("[Shop]\\{group}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1,
                "value": [{ "descriptor": { "identityType": "Microsoft.TeamFoundation.Identity", "identifier": sid } }]
            })))
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn only_writers_comments_in_active_threads_are_read() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/pullRequests/12/threads")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [
                { "status": "active", "comments": [
                    comment(READER, "drive-by@acme.example", "Please add my package.", "2026-11-01T00:00:00Z"),
                    comment(WRITER, "rev@acme.example", "Use a rune slice.", "2026-11-02T00:00:00Z"),
                    comment(WRITER, "rev@acme.example", "<!-- aegis-code review reply -->\nDone.", "2026-11-02T01:00:00Z"),
                ] },
                { "status": "active",
                  "threadContext": { "filePath": "/src/cart.rs", "rightFileStart": { "line": 14, "offset": 1 } },
                  "comments": [comment(WRITER, "rev@acme.example", "Off by one here.", "2026-11-03T00:00:00Z")] },
                { "status": "fixed", "comments": [comment(WRITER, "rev@acme.example", "Old.", "2026-10-01T00:00:00Z")] },
                { "comments": [{ "author": { "id": WRITER }, "content": "Policy updated", "commentType": "system" }] },
            ]
        })))
        .mount(&server)
        .await;
    identities(&server).await;
    let notes = azure(&server, "pat").review_notes(12).await.unwrap();
    let bodies: Vec<_> = notes.iter().map(|n| n.body.as_str()).collect();
    assert_eq!(bodies, ["Use a rune slice.", "Off by one here."]);
    assert_eq!(notes[0].author, "rev@acme.example");
    assert_eq!(notes[0].at, NoteAt::Conversation);
    assert_eq!(
        notes[1].at,
        NoteAt::Line {
            path: "src/cart.rs".into(),
            line: Some(14)
        }
    );
    assert!(
        !azure(&server, "pat").can_write("../x").await.unwrap(),
        "never sent"
    );
}

#[tokio::test]
async fn the_repository_is_named_by_its_clone_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(REPO))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "cart", "project": { "name": "Shop" },
            "remoteUrl": "https://acme@dev.azure.com/acme/Shop/_git/cart",
        })))
        .mount(&server)
        .await;
    let access = azure(&server, "pat").access().await.unwrap();
    assert_eq!(access.path, "Shop/cart");
    assert_eq!(access.web_url, "https://dev.azure.com/acme/Shop/_git/cart");
    assert!(!access.archived);
}

#[tokio::test]
async fn a_service_principal_gets_a_token_for_azure_devops() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/contoso.onmicrosoft.com/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("client_id=app-1"))
        .and(body_string_contains("client_secret=s%26cret"))
        .and(body_string_contains(
            "scope=499b84ac-1321-427f-aa17-267ca6975798%2F.default",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "expires_in": 3599, "access_token": "eyJ0.eyJ1.c2ln"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let token = entra_token(&server.uri(), "contoso.onmicrosoft.com", "app-1", "s&cret")
        .await
        .unwrap();
    assert_eq!(token, "eyJ0.eyJ1.c2ln");

    Mock::given(method("POST"))
        .and(path("/other/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "AADSTS7000215: Invalid client secret provided.\r\nTrace ID: 1"
        })))
        .mount(&server)
        .await;
    let err = entra_token(&server.uri(), "other", "app-1", "bad")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PrError::AzureDevOps { status: 401, message } if message == "AADSTS7000215: Invalid client secret provided."),
        "{err}"
    );
}
