//! The Azure DevOps forge (#636) against a fake API: opening the agent's
//! PR, commenting on it, what a review round reads of it, and a service
//! principal's token.

use aegis_pr::{
    AgentBranch, AzureDevOps, EntraToken, Forge, NoteAt, PrError, TokenSource, entra_token,
};
use serde_json::json;
use std::sync::Arc;
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

/// #641: a project with a space is written `%20` in URLs and by its own
/// name in the group a writer is looked up in.
#[tokio::test]
async fn a_project_with_a_space_opens_its_pr_and_checks_its_writers_by_name() {
    let server = MockServer::start().await;
    let repo = "/acme/My%20Shop/_apis/git/repositories/the%20cart";
    Mock::given(method("POST"))
        .and(path(format!("{repo}/pullrequests")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "pullRequestId": 12 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/acme/_apis/identities"))
        .and(query_param("identityIds", WRITER))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1, "value": [{ "id": WRITER, "memberOf": ["Microsoft.TeamFoundation.Identity;S-1-9-contrib"] }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/acme/_apis/identities"))
        .and(query_param("filterValue", "[My Shop]\\Contributors"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "value": [{ "descriptor": "Microsoft.TeamFoundation.Identity;S-1-9-contrib" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let az = AzureDevOps::new(
        &format!("{}/acme", server.uri()),
        "My Shop",
        "the cart",
        "pat",
    )
    .unwrap();
    let head = AgentBranch::for_issue("7").unwrap();
    let url = Forge::AzureDevOps(az.clone())
        .open_request(&head, "main", "Fix the cart", "body", false)
        .await
        .unwrap();
    assert_eq!(
        url,
        format!(
            "{}/acme/My%20Shop/_git/the%20cart/pullrequest/12",
            server.uri()
        )
    );
    assert!(az.can_write(WRITER).await.unwrap());
}

const PROJECT_ID: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const REPO_ID: &str = "bbbbbbbb-1111-2222-3333-444444444444";
const NAMESPACE: &str = "/acme/_apis/accesscontrollists/2e9eb7ed-3c0a-47d4-87c1-0ffdd275fd87";

/// A person `id` named by `descriptor`, in the project's Contributors
/// when `contributor`, and the repository with its IDs.
async fn person(server: &MockServer, id: &str, descriptor: &str, contributor: bool) {
    let groups = if contributor {
        json!(["Microsoft.TeamFoundation.Identity;S-1-9-contrib"])
    } else {
        json!([])
    };
    Mock::given(method("GET"))
        .and(path("/acme/_apis/identities"))
        .and(query_param("identityIds", id))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "value": [{ "id": id, "descriptor": descriptor, "memberOf": groups }]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(REPO))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": REPO_ID, "name": "cart",
            "project": { "id": PROJECT_ID, "name": "Shop" },
        })))
        .mount(server)
        .await;
}

/// The repository's ACL for `descriptor`, with its effective bits.
async fn acl(server: &MockServer, descriptor: &str, allow: u32, deny: u32) {
    let token = format!("repoV2/{PROJECT_ID}/{REPO_ID}");
    Mock::given(method("GET"))
        .and(path(NAMESPACE))
        .and(query_param("token", token.as_str()))
        .and(query_param("descriptors", descriptor))
        .and(query_param("includeExtendedInfo", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "value": [{
                "inheritPermissions": true,
                "token": token,
                "acesDictionary": { descriptor: {
                    "descriptor": descriptor, "allow": 0, "deny": 0,
                    "extendedInfo": { "effectiveAllow": allow, "effectiveDeny": deny }
                } },
                "includeExtendedInfo": true
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}

/// The project's groups, asked `times` times.
async fn groups(server: &MockServer, times: impl Into<wiremock::Times>) {
    Mock::given(method("GET"))
        .and(path("/acme/_apis/identities"))
        .and(query_param("searchFilter", "General"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "value": [{ "descriptor": "Microsoft.TeamFoundation.Identity;S-1-9-contrib" }]
        })))
        .expect(times)
        .mount(server)
        .await;
}

const WHO: &str = "Microsoft.IdentityModel.Claims.ClaimsIdentity;acme\\rev@acme.example";

/// #642: a contributor whose groups allow `GenericContribute` (read,
/// contribute, …: 2 | 4 | 16) writes; the groups are not asked.
#[tokio::test]
async fn a_contributor_allowed_through_a_group_writes() {
    let server = MockServer::start().await;
    person(&server, WRITER, WHO, true).await;
    acl(&server, WHO, 2 | 4 | 16, 0).await;
    groups(&server, 0).await;
    assert!(azure(&server, "pat").can_write(WRITER).await.unwrap());
}

/// A Contributor denied `GenericContribute` on this repository does not
/// write, though the group would say so.
#[tokio::test]
async fn a_contributor_denied_on_the_repository_does_not_write() {
    let server = MockServer::start().await;
    person(&server, WRITER, WHO, true).await;
    acl(&server, WHO, 2 | 16, 4).await;
    groups(&server, 0).await;
    assert!(!azure(&server, "pat").can_write(WRITER).await.unwrap());
}

/// Someone in no writer group but allowed on the repository itself
/// writes.
#[tokio::test]
async fn someone_allowed_on_the_repository_itself_writes() {
    let server = MockServer::start().await;
    person(&server, READER, WHO, false).await;
    acl(&server, WHO, 2 | 4, 0).await;
    groups(&server, 0).await;
    assert!(azure(&server, "pat").can_write(READER).await.unwrap());
}

/// When the token may not read the repository's security, the groups
/// decide, as before.
#[tokio::test]
async fn the_groups_decide_when_the_acl_cannot_be_read() {
    let server = MockServer::start().await;
    person(&server, WRITER, WHO, true).await;
    Mock::given(method("GET"))
        .and(path(NAMESPACE))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "message": "TF400813: not authorized"
        })))
        .expect(1)
        .mount(&server)
        .await;
    groups(&server, 1..).await;
    assert!(azure(&server, "pat").can_write(WRITER).await.unwrap());
}

/// #642: the evidence the measurement on a real organization records:
/// the identity's entry in the ACL with its effective bits, its groups,
/// and the decision, which the ACL's effective bits make.
#[tokio::test]
async fn the_evidence_shows_the_acl_entry_the_groups_and_the_decision() {
    let server = MockServer::start().await;
    person(&server, WRITER, WHO, true).await;
    acl(&server, WHO, 2 | 16, 4).await;
    groups(&server, 1..).await;
    let evidence = azure(&server, "pat").write_evidence(WRITER).await.unwrap();
    assert_eq!(evidence.descriptor.as_deref(), Some(WHO));
    assert_eq!(
        evidence.entry,
        Ok(aegis_pr::AclEntry {
            allow: 0,
            deny: 0,
            effective: Some((2 | 16, 4)),
        })
    );
    assert!(evidence.in_writer_group);
    assert!(!evidence.writes, "the deny on the repository wins");

    // An identity Azure does not know writes nowhere.
    let unknown = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/acme/_apis/identities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 0, "value": [] })))
        .mount(&unknown)
        .await;
    let evidence = azure(&unknown, "pat").write_evidence(READER).await.unwrap();
    assert_eq!(evidence.descriptor, None);
    assert!(!evidence.writes);
}

/// The authority gives tokens `first` then `second`, each living
/// `lifetime` seconds.
async fn authority(server: &MockServer, lifetime: u64, first: &str, second: &str) {
    Mock::given(method("POST"))
        .and(path("/contoso/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "expires_in": lifetime, "access_token": first
        })))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/contoso/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "expires_in": lifetime, "access_token": second
        })))
        .mount(server)
        .await;
}

fn principal(server: &MockServer) -> Arc<EntraToken> {
    Arc::new(EntraToken::new(&server.uri(), "contoso", "app-1", "s"))
}

/// #643: a service principal's token is kept while it has time left.
#[tokio::test]
async fn a_service_principal_token_is_kept_while_it_lives() {
    let server = MockServer::start().await;
    authority(&server, 3599, "eyJh.eyJ1.b25l", "eyJh.eyJ2.dHdv").await;
    let tokens = principal(&server);
    assert_eq!(tokens.token().await.unwrap(), "eyJh.eyJ1.b25l");
    assert_eq!(tokens.token().await.unwrap(), "eyJh.eyJ1.b25l");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// A token about to expire (less than the margin left) is issued again
/// before a call or a push: a job longer than the token's hour goes on.
#[tokio::test]
async fn an_expiring_token_is_issued_again_before_a_call_and_a_push() {
    let server = MockServer::start().await;
    // Sixty seconds: inside the five minutes' margin, so stale at once.
    authority(&server, 60, "eyJh.eyJ1.b25l", "eyJh.eyJ2.dHdv").await;
    for token in ["eyJh.eyJ1.b25l", "eyJh.eyJ2.dHdv"] {
        Mock::given(method("POST"))
            .and(path(format!("{REPO}/pullRequests/12/threads")))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 1 })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let tokens = principal(&server);
    let az = AzureDevOps::with_source(
        &format!("{}/acme", server.uri()),
        "Shop",
        "cart",
        TokenSource::Entra(Arc::clone(&tokens)),
    )
    .unwrap();
    az.comment_on_pull_request(12, "first").await.unwrap();
    az.comment_on_pull_request(12, "second").await.unwrap();
    // The push asks too, right before it: a third token.
    let push = AzureDevOps::with_source(
        "https://dev.azure.com/acme",
        "Shop",
        "cart",
        TokenSource::Entra(tokens),
    )
    .unwrap();
    let remote = Forge::AzureDevOps(push.clone()).remote_now().await.unwrap();
    assert_eq!(remote.credentials().unwrap().1, "eyJh.eyJ2.dHdv");
    let issued = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/token"))
        .count();
    assert_eq!(issued, 3);
    // A fixed remote cannot be had from an expiring token.
    assert!(matches!(push.remote(), Err(PrError::App(_))));
}

/// A service principal reaches Azure DevOps only.
#[tokio::test]
async fn a_service_principal_is_not_a_github_token() {
    let server = MockServer::start().await;
    let source = TokenSource::Entra(principal(&server));
    assert!(aegis_pr::GitHub::with_source("https://api.github.com", "o", "r", source).is_err());
}
