//! #643: the Azure DevOps notifier as a Microsoft Entra service
//! principal: its comment token is then a client secret, exchanged for
//! access tokens kept while they live.

use aegis_secrets::Secrets;
use aegis_server::webhook::{
    AZURE_DEVOPS_COMMENT_TOKEN_ENV, AzureDevOpsNotifier, AzureServicePrincipal,
};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const WRITER: &str = "6f5e1b2a-0c3d-4e5f-8a9b-0c1d2e3f4a5b";
const BEARER: &str = "Bearer eyJh.eyJ1.c2ln";

#[tokio::test]
async fn the_notifier_checks_writers_and_comments_as_a_service_principal() {
    let server = MockServer::start().await;
    // One token for both calls: it lives an hour.
    Mock::given(method("POST"))
        .and(path("/contoso/oauth2/v2.0/token"))
        .and(body_string_contains("client_id=app-1"))
        .and(body_string_contains("client_secret=s3cret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "expires_in": 3599, "access_token": "eyJh.eyJ1.c2ln"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/acme/_apis/identities"))
        .and(query_param("identityIds", WRITER))
        .and(header("authorization", BEARER))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{ "id": WRITER, "memberOf": ["T;S-1-9-contrib"] }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/acme/_apis/identities"))
        .and(query_param("filterValue", "[Shop]\\Contributors"))
        .and(header("authorization", BEARER))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{ "descriptor": "T;S-1-9-contrib" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/acme/Shop/_apis/git/repositories/cart/pullRequests/12/threads",
        ))
        .and(header("authorization", BEARER))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 1 })))
        .expect(1)
        .mount(&server)
        .await;
    let notifier = AzureDevOpsNotifier::new(
        &format!("{}/acme", server.uri()),
        Secrets::fixed([(AZURE_DEVOPS_COMMENT_TOKEN_ENV, "s3cret")]),
    )
    .with_principal(AzureServicePrincipal {
        tenant: "contoso".into(),
        client_id: "app-1".into(),
        authority: server.uri(),
    });
    assert_eq!(notifier.can_write("Shop", "cart", WRITER).await, Ok(true));
    // The handler's clone keeps the same token.
    notifier
        .clone()
        .comment_on_pull_request("Shop", "cart", 12, "Queued.")
        .await;
}

/// A wrong secret is an error, not a "may not write".
#[tokio::test]
async fn a_refused_secret_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/contoso/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "AADSTS7000215: Invalid client secret provided."
        })))
        .mount(&server)
        .await;
    let notifier = AzureDevOpsNotifier::new(
        &format!("{}/acme", server.uri()),
        Secrets::fixed([(AZURE_DEVOPS_COMMENT_TOKEN_ENV, "bad")]),
    )
    .with_principal(AzureServicePrincipal {
        tenant: "contoso".into(),
        client_id: "app-1".into(),
        authority: server.uri(),
    });
    let err = notifier
        .can_write("Shop", "cart", WRITER)
        .await
        .unwrap_err();
    assert!(err.contains("AADSTS7000215"), "{err}");
}
