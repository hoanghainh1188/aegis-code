use crate::types::RepoMode;
use crate::{NewRepo, Page, PageOf, Repo, RepoId, Store, StoreError};

const MAX_SLUG_LEN: usize = 128;

/// Same rule as the `repos.slug` CHECK, to say what is wrong before the
/// database does: the slug ends up in paths and branch names.
fn check_slug(slug: &str) -> Result<(), StoreError> {
    let ok = slug.len() <= MAX_SLUG_LEN
        && slug
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid {
            field: "slug",
            message: format!(
                "`{slug}`: ASCII letters, digits, `.`, `_`, `-`, starting with a letter or digit, at most {MAX_SLUG_LEN} bytes"
            ),
        })
    }
}

struct RepoRow {
    id: i64,
    slug: String,
    url: String,
    default_branch: String,
    language: String,
    mode: String,
    config: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<RepoRow> for Repo {
    type Error = StoreError;

    fn try_from(row: RepoRow) -> Result<Self, StoreError> {
        Ok(Self {
            id: RepoId(row.id),
            slug: row.slug,
            url: row.url,
            default_branch: row.default_branch,
            language: row.language,
            mode: RepoMode::decode("repos.mode", &row.mode)?,
            config: row.config,
            created_at: row.created_at,
        })
    }
}

impl Store {
    pub async fn create_repo(&self, repo: &NewRepo) -> Result<Repo, StoreError> {
        check_slug(&repo.slug)?;
        sqlx::query_as!(
            RepoRow,
            "INSERT INTO repos (slug, url, default_branch, language, mode, config)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, slug, url, default_branch, language, mode, config, created_at",
            repo.slug,
            repo.url,
            repo.default_branch,
            repo.language,
            repo.mode.as_str(),
            repo.config,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_db)?
        .try_into()
    }

    pub async fn repo(&self, id: RepoId) -> Result<Option<Repo>, StoreError> {
        sqlx::query_as!(
            RepoRow,
            "SELECT id, slug, url, default_branch, language, mode, config, created_at
             FROM repos WHERE id = $1",
            id.0,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Repo::try_from)
        .transpose()
    }

    pub async fn repo_by_slug(&self, slug: &str) -> Result<Option<Repo>, StoreError> {
        sqlx::query_as!(
            RepoRow,
            "SELECT id, slug, url, default_branch, language, mode, config, created_at
             FROM repos WHERE slug = $1",
            slug,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Repo::try_from)
        .transpose()
    }

    /// Oldest first.
    pub async fn list_repos(&self, page: Page) -> Result<PageOf<Repo>, StoreError> {
        let rows = sqlx::query_as!(
            RepoRow,
            "SELECT id, slug, url, default_branch, language, mode, config, created_at
             FROM repos WHERE $1::bigint IS NULL OR id > $1
             ORDER BY id LIMIT $2",
            page.cursor(),
            page.fetch(),
        )
        .fetch_all(&self.pool)
        .await?;
        let repos = rows
            .into_iter()
            .map(Repo::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PageOf::new(repos, page, |r| r.id.0))
    }
}
