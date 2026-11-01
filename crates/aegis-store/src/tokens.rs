//! Personal API tokens (#231): a person signed in to the dashboard makes
//! tokens of their own for the developer client, each named and with an
//! end. A request with one acts as that person, with their roles, and is
//! audited as them. Only the hash is kept, as for service tokens.

use chrono::{DateTime, Utc};

use crate::audit::{self, Target};
use crate::tx;
use crate::{Actor, Store, StoreError, TokenHash, UserId};

/// Live tokens a person may have at once.
pub const MAX_PERSONAL_TOKENS: i64 = 10;
/// The longest a personal token lives, in days.
pub const MAX_TOKEN_DAYS: u32 = 90;

/// A service's live API token, as its page lists it (#371); never the
/// token itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceToken {
    pub id: i64,
    pub created_at: DateTime<Utc>,
}

/// A person's live token, as their page lists it; never the token itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalToken {
    pub id: i64,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Store {
    /// A new token of `person`, called `label`, ending in `days` (1 to
    /// [`MAX_TOKEN_DAYS`]); `token` is its hash. Refused for a service, or
    /// beyond [`MAX_PERSONAL_TOKENS`] live ones.
    pub async fn create_personal_token(
        &self,
        person: UserId,
        label: &str,
        days: u32,
        token: &TokenHash,
        actor: &Actor,
    ) -> Result<PersonalToken, StoreError> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 64 {
            return Err(StoreError::Invalid {
                field: "label",
                message: "1 to 64 characters".into(),
            });
        }
        if !(1..=MAX_TOKEN_DAYS).contains(&days) {
            return Err(StoreError::Invalid {
                field: "days",
                message: format!("1 to {MAX_TOKEN_DAYS}"),
            });
        }
        let mut tx = self.pool.begin().await?;
        let work = async {
            let kind =
                sqlx::query_scalar!("SELECT kind FROM users WHERE id = $1 FOR UPDATE", person.0)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("user {}", person.0)))?;
            if kind != "person" {
                return Err(StoreError::Invalid {
                    field: "user",
                    message:
                        "personal tokens are for people; a service's come from `aegis-server token`"
                            .into(),
                });
            }
            let live = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!" FROM api_tokens
                   WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()"#,
                person.0,
            )
            .fetch_one(&mut *tx)
            .await?;
            if live >= MAX_PERSONAL_TOKENS {
                return Err(StoreError::Conflict(format!(
                    "{MAX_PERSONAL_TOKENS} live tokens already; revoke one first"
                )));
            }
            let row = sqlx::query!(
                "INSERT INTO api_tokens (user_id, token_hash, label, expires_at)
                 VALUES ($1, $2, $3, now() + make_interval(days => $4))
                 RETURNING id, created_at, expires_at AS \"expires_at!\"",
                person.0,
                token.as_slice(),
                label,
                i32::try_from(days).unwrap_or(1),
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?;
            let detail = serde_json::json!({ "token_id": row.id, "label": label, "days": days });
            audit::record(&mut tx, actor, "token.create", Target::default(), detail).await?;
            Ok::<_, StoreError>(PersonalToken {
                id: row.id,
                label: label.to_owned(),
                created_at: row.created_at,
                expires_at: row.expires_at,
            })
        }
        .await;
        tx::commit(tx, work).await
    }

    /// `person`'s live tokens, newest first.
    pub async fn personal_tokens(&self, person: UserId) -> Result<Vec<PersonalToken>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT id, label AS "label!", created_at, expires_at AS "expires_at!"
               FROM api_tokens
               WHERE user_id = $1 AND revoked_at IS NULL AND label IS NOT NULL
                 AND expires_at > now()
               ORDER BY id DESC"#,
            person.0,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| PersonalToken {
                id: r.id,
                label: r.label,
                created_at: r.created_at,
                expires_at: r.expires_at,
            })
            .collect())
    }

    /// Revokes `person`'s token `id`; whether it was one of theirs and
    /// live.
    pub async fn revoke_personal_token(
        &self,
        person: UserId,
        id: i64,
        actor: &Actor,
    ) -> Result<bool, StoreError> {
        self.revoke_token(person, id, actor).await
    }

    /// Revokes token `id` of `user`, a person's or a service's (#371);
    /// `false` when they have no such live token.
    pub async fn revoke_token(
        &self,
        user: UserId,
        id: i64,
        actor: &Actor,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let revoked = sqlx::query!(
                "UPDATE api_tokens SET revoked_at = now()
                 WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
                id,
                user.0,
            )
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if revoked > 0 {
                let detail = serde_json::json!({ "token_id": id });
                audit::record(&mut tx, actor, "token.revoke", Target::default(), detail).await?;
            }
            Ok::<_, StoreError>(revoked > 0)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// A service's live API tokens (#371), newest first: their IDs and
    /// when they were made, never their hash.
    pub async fn service_tokens(&self, service: UserId) -> Result<Vec<ServiceToken>, StoreError> {
        Ok(sqlx::query_as!(
            ServiceToken,
            "SELECT id, created_at FROM api_tokens
             WHERE user_id = $1 AND revoked_at IS NULL AND label IS NULL
             ORDER BY id DESC LIMIT 100",
            service.0,
        )
        .fetch_all(&self.pool)
        .await?)
    }
}
