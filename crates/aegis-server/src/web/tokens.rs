//! `/me/tokens`: a person's own API tokens for the developer client
//! (#231). A token is shown once, on the page that made it, never again
//! and never in a URL; only its hash is kept. A request with it acts as
//! its person, with their roles.

use aegis_store::{MAX_PERSONAL_TOKENS, MAX_TOKEN_DAYS, PersonalToken, Store, StoreError, UserId};
use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::{Access, Auth, random_token, token_hash};

const DEFAULT_DAYS: u32 = 30;

struct Row {
    id: i64,
    label: String,
    created_at: String,
    expires_at: String,
}

impl From<PersonalToken> for Row {
    fn from(t: PersonalToken) -> Self {
        let day = |d: chrono::DateTime<chrono::Utc>| d.format("%Y-%m-%d").to_string();
        Self {
            id: t.id,
            label: t.label,
            created_at: day(t.created_at),
            expires_at: day(t.expires_at),
        }
    }
}

#[derive(Template)]
#[template(path = "tokens.html")]
struct TokensPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    tokens: Vec<Row>,
    /// The token just made, shown this once.
    made: Option<String>,
    error: Option<String>,
    public_url: String,
    max_days: u32,
    max_tokens: i64,
}

#[derive(Debug, Deserialize)]
pub struct NewToken {
    #[serde(default)]
    label: String,
    #[serde(default)]
    days: String,
}

/// The signed-in person; a service has no page here.
fn person(access: &Access) -> Result<UserId, WebError> {
    access.actor.user.ok_or_else(|| {
        WebError::new(
            StatusCode::FORBIDDEN,
            "Only signed-in users can have personal tokens",
        )
    })
}

#[allow(clippy::too_many_arguments)]
async fn page(
    store: &Store,
    auth: &Auth,
    nonce: &Nonce,
    me: &Me,
    user: UserId,
    made: Option<String>,
    error: Option<String>,
    status: StatusCode,
) -> WebResult {
    let tokens = store
        .personal_tokens(user)
        .await?
        .into_iter()
        .map(Row::from)
        .collect();
    let html = render(&TokensPage {
        nonce: nonce.as_str(),
        me,
        tokens,
        made,
        error,
        public_url: auth.public_url(),
        max_days: MAX_TOKEN_DAYS,
        max_tokens: MAX_PERSONAL_TOKENS,
    })?;
    Ok((status, html).into_response())
}

pub async fn list(
    State(store): State<Store>,
    Extension(auth): Extension<Auth>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
) -> WebResult {
    let user = person(&access)?;
    page(&store, &auth, &nonce, &me, user, None, None, StatusCode::OK).await
}

pub async fn create(
    State(store): State<Store>,
    Extension(auth): Extension<Auth>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Form(form): Form<NewToken>,
) -> WebResult {
    let user = person(&access)?;
    let days = if form.days.trim().is_empty() {
        Ok(DEFAULT_DAYS)
    } else {
        form.days.trim().parse::<u32>()
    };
    let refused = |why: String| (None, Some(why), StatusCode::UNPROCESSABLE_ENTITY);
    let (made, error, status) = match days {
        Err(_) => refused(format!("Days must be between 1 and {MAX_TOKEN_DAYS}")),
        Ok(days) => {
            let token = random_token().map_err(|e| {
                tracing::error!(error = %e, "web: no randomness for a token");
                WebError::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            })?;
            match store
                .create_personal_token(user, &form.label, days, &token_hash(&token), &access.actor)
                .await
            {
                Ok(_) => (Some(token), None, StatusCode::OK),
                Err(StoreError::Invalid { field: "label", .. }) => {
                    refused("Token label must be between 1 and 64 characters".into())
                }
                Err(StoreError::Invalid { .. }) => {
                    refused(format!("Days must be between 1 and {MAX_TOKEN_DAYS}"))
                }
                Err(StoreError::Conflict(_)) => refused(format!(
                    "Already reached {MAX_PERSONAL_TOKENS} active tokens; revoke one first"
                )),
                Err(e) => return Err(e.into()),
            }
        }
    };
    page(&store, &auth, &nonce, &me, user, made, error, status).await
}

pub async fn revoke(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Path(id): Path<i64>,
) -> WebResult {
    let user = person(&access)?;
    store.revoke_personal_token(user, id, &access.actor).await?;
    Ok(Redirect::to("/me/tokens").into_response())
}
