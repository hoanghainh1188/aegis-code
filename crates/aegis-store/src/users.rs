//! Users, their roles per repo, sign-ins on their way through the SSO,
//! dashboard sessions and API tokens (#29).
//! Sessions and tokens are looked up by the SHA-256 of what the client
//! holds; the token itself is never stored.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::audit::{self, Target};
use crate::tx;
use crate::{Actor, ActorKind, RepoId, RepoRole, Store, StoreError, User, UserId, UserKind};

const MAX_NAME_LEN: usize = 320;

/// The SHA-256 of a session or API token.
pub type TokenHash = [u8; 32];

/// A live dashboard session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub user: User,
    /// What the session's forms send back.
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}

/// What `/auth/callback` needs from `/auth/login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLogin {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub next: String,
}

struct UserRow {
    id: i64,
    kind: String,
    name: String,
    is_admin: bool,
}

impl TryFrom<UserRow> for User {
    type Error = StoreError;

    fn try_from(row: UserRow) -> Result<Self, StoreError> {
        Ok(Self {
            id: UserId(row.id),
            kind: UserKind::decode("users.kind", &row.kind)?,
            name: row.name,
            is_admin: row.is_admin,
        })
    }
}

/// Lower case and trimmed; an e-mail for a person.
/// Someone with a role in a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMember {
    pub kind: UserKind,
    pub name: String,
    pub role: RepoRole,
    pub granted_at: chrono::DateTime<chrono::Utc>,
}

fn user_name(kind: UserKind, name: &str) -> Result<String, StoreError> {
    let name = name.trim().to_lowercase();
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.chars().any(char::is_control)
        && (kind == UserKind::Service || name.contains('@'));
    if ok {
        Ok(name)
    } else {
        Err(StoreError::Invalid {
            field: "name",
            message: match kind {
                UserKind::Person => format!("`{name}`: an e-mail, at most {MAX_NAME_LEN} bytes"),
                UserKind::Service => format!("`{name}`: 1 to {MAX_NAME_LEN} bytes"),
            },
        })
    }
}

/// The user `kind`/`name`; created when missing if `create`, else
/// `NotFound`: taking a role away creates no one.
async fn ensure_user(
    conn: &mut PgConnection,
    kind: UserKind,
    name: &str,
    create: bool,
) -> Result<User, StoreError> {
    let name = user_name(kind, name)?;
    if !create {
        return sqlx::query_as!(
            UserRow,
            "SELECT id, kind, name, is_admin FROM users WHERE kind = $1 AND name = $2",
            kind.as_str(),
            name,
        )
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("{} `{name}`", kind.as_str())))?
        .try_into();
    }
    sqlx::query_as!(
        UserRow,
        "INSERT INTO users (kind, name) VALUES ($1, $2)
         ON CONFLICT (kind, name) DO UPDATE SET name = EXCLUDED.name
         RETURNING id, kind, name, is_admin",
        kind.as_str(),
        name,
    )
    .fetch_one(conn)
    .await
    .map_err(StoreError::from_db)?
    .try_into()
}

impl Store {
    /// The person the SSO vouched for: found by `subject`, else by
    /// `email` when that row has no subject yet (roles granted before the
    /// first sign-in), else created. `Conflict` when the e-mail belongs to
    /// another subject, or when it is not `email_verified` and someone was
    /// granted roles by it: an e-mail anyone could set takes over no one.
    /// Creating and linking are audited.
    pub async fn sign_in(
        &self,
        subject: &str,
        email: &str,
        email_verified: bool,
    ) -> Result<User, StoreError> {
        let email = user_name(UserKind::Person, email)?;
        let mut tx = self.pool.begin().await?;
        let work = async {
            let known = sqlx::query_as!(
                UserRow,
                "SELECT id, kind, name, is_admin FROM users WHERE oidc_subject = $1",
                subject,
            )
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(row) = known {
                return row.try_into();
            }
            let by_email = sqlx::query!(
                "SELECT id, oidc_subject FROM users WHERE kind = 'person' AND name = $1 FOR UPDATE",
                email,
            )
            .fetch_optional(&mut *tx)
            .await?;
            let linked = by_email.is_some();
            let row = match by_email {
                Some(row) if row.oidc_subject.is_some() => {
                    return Err(StoreError::Conflict(format!(
                        "{email} is signed in with another SSO subject"
                    )));
                }
                Some(_) if !email_verified => {
                    return Err(StoreError::Conflict(format!(
                        "{email} has roles and the SSO did not verify it"
                    )));
                }
                Some(row) => sqlx::query_as!(
                    UserRow,
                    "UPDATE users SET oidc_subject = $2 WHERE id = $1
                     RETURNING id, kind, name, is_admin",
                    row.id,
                    subject,
                )
                .fetch_one(&mut *tx)
                .await
                .map_err(StoreError::from_db)?,
                None => sqlx::query_as!(
                    UserRow,
                    "INSERT INTO users (kind, name, oidc_subject) VALUES ('person', $1, $2)
                     RETURNING id, kind, name, is_admin",
                    email,
                    subject,
                )
                .fetch_one(&mut *tx)
                .await
                .map_err(StoreError::from_db)?,
            };
            let user: User = row.try_into()?;
            let action = if linked { "user.link" } else { "user.create" };
            let detail = serde_json::json!({ "subject": subject });
            let actor = Actor::user(&user, ActorKind::User);
            audit::record(&mut tx, &actor, action, Target::default(), detail).await?;
            Ok::<_, StoreError>(user)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Keeps a sign-in on its way through the SSO under `id`; expired
    /// ones go at the same time.
    pub async fn start_login(
        &self,
        id: &TokenHash,
        login: &PendingLogin,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            sqlx::query!("DELETE FROM pending_logins WHERE expires_at < now()")
                .execute(&mut *tx)
                .await?;
            sqlx::query!(
                "INSERT INTO pending_logins (id_hash, state, nonce, verifier, next, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                id.as_slice(),
                login.state,
                login.nonce,
                login.verifier,
                login.next,
                expires_at,
            )
            .execute(&mut *tx)
            .await
            .map_err(StoreError::from_db)?;
            Ok::<_, StoreError>(())
        }
        .await;
        tx::commit(tx, work).await
    }

    /// The sign-in kept under `id`, if it has not expired; taken, so a
    /// second call finds nothing.
    pub async fn take_login(&self, id: &TokenHash) -> Result<Option<PendingLogin>, StoreError> {
        let row = sqlx::query!(
            "DELETE FROM pending_logins WHERE id_hash = $1
             RETURNING state, nonce, verifier, next, expires_at > now() AS \"live!\"",
            id.as_slice(),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.filter(|r| r.live).map(|r| PendingLogin {
            state: r.state,
            nonce: r.nonce,
            verifier: r.verifier,
            next: r.next,
        }))
    }

    /// Opens a session for `user`; expired sessions go at the same time.
    pub async fn start_session(
        &self,
        user: &User,
        token: &TokenHash,
        csrf_token: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            sqlx::query!("DELETE FROM sessions WHERE expires_at < now()")
                .execute(&mut *tx)
                .await?;
            sqlx::query!(
                "INSERT INTO sessions (token_hash, user_id, csrf_token, expires_at)
                 VALUES ($1, $2, $3, $4)",
                token.as_slice(),
                user.id.0,
                csrf_token,
                expires_at,
            )
            .execute(&mut *tx)
            .await
            .map_err(StoreError::from_db)?;
            let actor = Actor::user(user, ActorKind::User);
            audit::record(
                &mut tx,
                &actor,
                "session.login",
                Target::default(),
                serde_json::json!({}),
            )
            .await?;
            Ok::<_, StoreError>(())
        }
        .await;
        tx::commit(tx, work).await
    }

    /// The live session of `token`.
    pub async fn session(&self, token: &TokenHash) -> Result<Option<Session>, StoreError> {
        let row = sqlx::query!(
            "SELECT u.id, u.kind, u.name, u.is_admin, s.csrf_token, s.expires_at
             FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1 AND s.expires_at > now()",
            token.as_slice(),
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let user = UserRow {
                id: row.id,
                kind: row.kind,
                name: row.name,
                is_admin: row.is_admin,
            };
            Ok(Session {
                user: user.try_into()?,
                csrf_token: row.csrf_token,
                expires_at: row.expires_at,
            })
        })
        .transpose()
    }

    /// Signs the session out; nothing when it is already gone.
    pub async fn end_session(&self, token: &TokenHash, actor: &Actor) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let ended = sqlx::query!(
                "DELETE FROM sessions WHERE token_hash = $1",
                token.as_slice()
            )
            .execute(&mut *tx)
            .await?;
            if ended.rows_affected() > 0 {
                audit::record(
                    &mut tx,
                    actor,
                    "session.logout",
                    Target::default(),
                    serde_json::json!({}),
                )
                .await?;
            }
            Ok::<_, StoreError>(())
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Ends every session of `kind`/`name`, as when someone leaves; how
    /// many were live.
    pub async fn end_sessions_of(
        &self,
        kind: UserKind,
        name: &str,
        actor: &Actor,
    ) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let user = ensure_user(&mut tx, kind, name, false).await?;
            let ended = sqlx::query!("DELETE FROM sessions WHERE user_id = $1", user.id.0)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            let detail = serde_json::json!({ "user": user.name, "ended": ended });
            audit::record(&mut tx, actor, "session.revoke", Target::default(), detail).await?;
            Ok::<_, StoreError>(ended)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// The user of a live API token: a service's, or a person's own token
    /// (#231) that has not ended.
    pub async fn token_user(&self, token: &TokenHash) -> Result<Option<User>, StoreError> {
        sqlx::query_as!(
            UserRow,
            "SELECT u.id, u.kind, u.name, u.is_admin
             FROM api_tokens t JOIN users u ON u.id = t.user_id
             WHERE t.token_hash = $1 AND t.revoked_at IS NULL
               AND (t.expires_at IS NULL OR t.expires_at > now())",
            token.as_slice(),
        )
        .fetch_optional(&self.pool)
        .await?
        .map(User::try_from)
        .transpose()
    }

    /// Who has a role in `repo` (#329), people before services, by name.
    pub async fn repo_members(&self, repo: RepoId) -> Result<Vec<RepoMember>, StoreError> {
        let rows = sqlx::query!(
            "SELECT u.kind, u.name, r.role, r.granted_at
             FROM repo_roles r JOIN users u ON u.id = r.user_id
             WHERE r.repo_id = $1
             ORDER BY u.kind, u.name",
            repo.0,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(RepoMember {
                    kind: UserKind::decode("users.kind", &r.kind)?,
                    name: r.name,
                    role: RepoRole::decode("repo_roles.role", &r.role)?,
                    granted_at: r.granted_at,
                })
            })
            .collect()
    }

    /// The roles `user` has, repo by repo.
    pub async fn repo_roles(&self, user: UserId) -> Result<Vec<(RepoId, RepoRole)>, StoreError> {
        let rows = sqlx::query!(
            "SELECT repo_id, role FROM repo_roles WHERE user_id = $1 ORDER BY repo_id",
            user.0,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    RepoId(r.repo_id),
                    RepoRole::decode("repo_roles.role", &r.role)?,
                ))
            })
            .collect()
    }

    /// Gives `kind`/`name` the `role` in `repo`, or takes its role there
    /// away (`None`). The user is created when missing: a person can be
    /// granted roles before the first sign-in.
    pub async fn set_repo_role(
        &self,
        kind: UserKind,
        name: &str,
        repo: RepoId,
        role: Option<RepoRole>,
        actor: &Actor,
    ) -> Result<User, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let user = ensure_user(&mut tx, kind, name, role.is_some()).await?;
            let target = Target {
                repo: Some(repo),
                job: None,
            };
            match role {
                Some(role) => {
                    sqlx::query!(
                        "INSERT INTO repo_roles (user_id, repo_id, role) VALUES ($1, $2, $3)
                         ON CONFLICT (user_id, repo_id)
                         DO UPDATE SET role = EXCLUDED.role, granted_at = now()",
                        user.id.0,
                        repo.0,
                        role.as_str(),
                    )
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| match StoreError::from_db(e) {
                        StoreError::NotFound(_) => StoreError::NotFound(format!("repo {}", repo.0)),
                        other => other,
                    })?;
                    let detail = serde_json::json!({ "user": user.name, "role": role.as_str() });
                    audit::record(&mut tx, actor, "role.grant", target, detail).await?;
                }
                None => {
                    sqlx::query!(
                        "DELETE FROM repo_roles WHERE user_id = $1 AND repo_id = $2",
                        user.id.0,
                        repo.0,
                    )
                    .execute(&mut *tx)
                    .await?;
                    let detail = serde_json::json!({ "user": user.name });
                    audit::record(&mut tx, actor, "role.revoke", target, detail).await?;
                }
            }
            Ok::<_, StoreError>(user)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Makes `kind`/`name` an admin of every repo, or no longer one.
    /// `Conflict` when that would leave no admin (#369): someone must be
    /// able to give rights from the pages.
    pub async fn set_admin(
        &self,
        kind: UserKind,
        name: &str,
        admin: bool,
        actor: &Actor,
    ) -> Result<User, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let user = ensure_user(&mut tx, kind, name, admin).await?;
            if !admin && user.is_admin {
                // Locked, so two admins cannot take each other's rights at once.
                let admins = sqlx::query_scalar!("SELECT id FROM users WHERE is_admin FOR UPDATE")
                    .fetch_all(&mut *tx)
                    .await?;
                if admins.len() <= 1 {
                    return Err(StoreError::Conflict(format!(
                        "{} is the last admin; make someone else an admin first",
                        user.name
                    )));
                }
            }
            sqlx::query!(
                "UPDATE users SET is_admin = $2 WHERE id = $1",
                user.id.0,
                admin
            )
            .execute(&mut *tx)
            .await?;
            let action = if admin { "role.grant" } else { "role.revoke" };
            let detail = serde_json::json!({ "user": user.name, "role": "admin" });
            audit::record(&mut tx, actor, action, Target::default(), detail).await?;
            Ok::<_, StoreError>(User {
                is_admin: admin,
                ..user
            })
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Adds an API token, by its hash, to the service `service`, created
    /// when missing.
    pub async fn create_token(
        &self,
        service: &str,
        token: &TokenHash,
        actor: &Actor,
    ) -> Result<User, StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let user = ensure_user(&mut tx, UserKind::Service, service, true).await?;
            let id = sqlx::query_scalar!(
                "INSERT INTO api_tokens (user_id, token_hash) VALUES ($1, $2) RETURNING id",
                user.id.0,
                token.as_slice(),
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from_db)?;
            let detail = serde_json::json!({ "user": user.name, "token_id": id });
            audit::record(&mut tx, actor, "token.create", Target::default(), detail).await?;
            Ok::<_, StoreError>(user)
        }
        .await;
        tx::commit(tx, work).await
    }

    /// Revokes every token of the service `service`; how many were live.
    pub async fn revoke_tokens(&self, service: &str, actor: &Actor) -> Result<u64, StoreError> {
        let name = user_name(UserKind::Service, service)?;
        let mut tx = self.pool.begin().await?;
        let work = async {
            let revoked = sqlx::query!(
                "UPDATE api_tokens SET revoked_at = now()
                 WHERE revoked_at IS NULL
                   AND user_id = (SELECT id FROM users WHERE kind = 'service' AND name = $1)",
                name,
            )
            .execute(&mut *tx)
            .await?
            .rows_affected();
            let detail = serde_json::json!({ "user": name, "revoked": revoked });
            audit::record(&mut tx, actor, "token.revoke", Target::default(), detail).await?;
            Ok::<_, StoreError>(revoked)
        }
        .await;
        tx::commit(tx, work).await
    }
}
