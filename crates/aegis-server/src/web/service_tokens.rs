//! Services and their API tokens for the admin (#371): CI or a connector
//! calling `/api/v1`. A service is added by name, its roles given on the
//! repos' pages, and its tokens made and revoked here, as `aegis-server
//! token|revoke-tokens` do. A new token is shown once, in the answer to
//! the request that made it; only its hash is kept, and the audit log
//! keeps its ID. Only an admin: for anyone else it is not found.

use aegis_store::{Store, UserKind};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::admin::admin_only;
use super::users::{summary_of, user_page};
use super::{Me, Nonce, WebError, WebResult};
use crate::auth::{Access, random_token, token_hash};

#[derive(Debug, Deserialize)]
pub struct NewService {
    name: String,
}

/// Adds the service `name`; its page follows.
pub async fn create(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Form(form): Form<NewService>,
) -> WebResult {
    admin_only(&access)?;
    let user = store.create_service(&form.name, &access.actor).await?;
    Ok(Redirect::to(&format!("/admin/users/{}?done=service_added", user.id.0)).into_response())
}

/// The service `id`; a person's tokens are theirs to make at `/me/tokens`.
async fn service_of(store: &Store, id: i64) -> Result<aegis_store::User, WebError> {
    let user = summary_of(store, id).await?.user;
    if user.kind == UserKind::Service {
        Ok(user)
    } else {
        Err(WebError::new(
            StatusCode::BAD_REQUEST,
            "a person makes their own tokens at /me/tokens; this is for services",
        ))
    }
}

/// Makes a token of service `id` and shows it, this once.
pub async fn make(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Path(id): Path<i64>,
) -> WebResult {
    admin_only(&access)?;
    let service = service_of(&store, id).await?;
    let token = random_token().map_err(|e| {
        tracing::error!(error = %e, "no randomness for a token");
        WebError::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;
    store
        .create_token(&service.name, &token_hash(&token), &access.actor)
        .await?;
    user_page(&store, &nonce, &me, &access, id, None, Some(token)).await
}

/// Revokes token `token` of service `id`.
pub async fn revoke(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path((id, token)): Path<(i64, i64)>,
) -> WebResult {
    admin_only(&access)?;
    let service = service_of(&store, id).await?;
    if !store.revoke_token(service.id, token, &access.actor).await? {
        return Err(WebError::not_found(format!("live token {token}")));
    }
    Ok(Redirect::to(&format!("/admin/users/{id}?done=token_revoked")).into_response())
}

/// Revokes every token of service `id`.
pub async fn revoke_all(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    admin_only(&access)?;
    let service = service_of(&store, id).await?;
    store.revoke_tokens(&service.name, &access.actor).await?;
    Ok(Redirect::to(&format!("/admin/users/{id}?done=tokens_revoked")).into_response())
}
