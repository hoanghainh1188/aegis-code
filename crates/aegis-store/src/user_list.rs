//! People and services as the admin sees them (#369): who they are,
//! whether they are admins, how many repos they have a role in, and when
//! they last signed in.

use chrono::{DateTime, Utc};

use crate::{Page, PageOf, Store, StoreError, User, UserId, UserKind};

/// A user with what the admin pages show of them.
#[derive(Debug, Clone, PartialEq)]
pub struct UserSummary {
    pub user: User,
    /// Repos they have a role in.
    pub repos: i64,
    /// A person who signed in at least once (the SSO linked them).
    pub signed_in: bool,
    /// When their latest session began.
    pub last_session: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

struct SummaryRow {
    id: i64,
    kind: String,
    name: String,
    is_admin: bool,
    created_at: DateTime<Utc>,
    signed_in: bool,
    repos: i64,
    last_session: Option<DateTime<Utc>>,
}

impl TryFrom<SummaryRow> for UserSummary {
    type Error = StoreError;

    fn try_from(r: SummaryRow) -> Result<Self, StoreError> {
        Ok(Self {
            user: User {
                id: UserId(r.id),
                kind: UserKind::decode("users.kind", &r.kind)?,
                name: r.name,
                is_admin: r.is_admin,
            },
            repos: r.repos,
            signed_in: r.signed_in,
            last_session: r.last_session,
            created_at: r.created_at,
        })
    }
}

impl Store {
    /// Users newest first, those whose name holds `search` (case aside)
    /// when given.
    pub async fn list_users(
        &self,
        search: Option<&str>,
        page: Page,
    ) -> Result<PageOf<UserSummary>, StoreError> {
        let users = sqlx::query_as!(
            SummaryRow,
            r#"SELECT u.id, u.kind, u.name, u.is_admin, u.created_at,
                      u.oidc_subject IS NOT NULL AS "signed_in!",
                      (SELECT count(*) FROM repo_roles r WHERE r.user_id = u.id) AS "repos!",
                      (SELECT max(s.created_at) FROM sessions s WHERE s.user_id = u.id) AS last_session
               FROM users u
               WHERE ($1::text IS NULL OR strpos(u.name, lower($1)) > 0)
                 AND ($2::bigint IS NULL OR u.id < $2)
               ORDER BY u.id DESC LIMIT $3"#,
            search,
            page.cursor(),
            page.fetch(),
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(UserSummary::try_from)
        .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(PageOf::new(users, page, |u| u.user.id.0))
    }

    /// The user `id`, as [`Store::list_users`] shows them.
    pub async fn user_summary(&self, id: UserId) -> Result<Option<UserSummary>, StoreError> {
        sqlx::query_as!(
            SummaryRow,
            r#"SELECT u.id, u.kind, u.name, u.is_admin, u.created_at,
                      u.oidc_subject IS NOT NULL AS "signed_in!",
                      (SELECT count(*) FROM repo_roles r WHERE r.user_id = u.id) AS "repos!",
                      (SELECT max(s.created_at) FROM sessions s WHERE s.user_id = u.id) AS last_session
               FROM users u WHERE u.id = $1"#,
            id.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(UserSummary::try_from)
        .transpose()
    }
}
