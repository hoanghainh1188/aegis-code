use crate::audit::{self, Target};
use crate::types::{RepoMode, count};
use crate::{Actor, NewRepo, Page, PageOf, Repo, RepoId, Store, StoreError};

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

/// Lower case, without a trailing `/` or `.git`: the same as the SQL of
/// [`Store::repo_by_remote`] does to the stored URLs.
fn normalized_remote(remote: &str) -> String {
    let lower = remote.trim().to_ascii_lowercase();
    let bare = lower.trim_end_matches('/');
    bare.strip_suffix(".git").unwrap_or(bare).to_owned()
}

struct RepoRow {
    id: i64,
    slug: String,
    url: String,
    default_branch: String,
    language: String,
    mode: String,
    max_concurrent_jobs: i32,
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
            max_concurrent_jobs: count("repos.max_concurrent_jobs", row.max_concurrent_jobs)?,
            config: row.config,
            created_at: row.created_at,
        })
    }
}

impl Store {
    pub async fn create_repo(&self, repo: &NewRepo) -> Result<Repo, StoreError> {
        self.create_repo_by(repo, &Actor::system()).await
    }

    /// [`Store::create_repo`], audited as done by `actor` with the
    /// settings it registered.
    pub async fn create_repo_by(&self, repo: &NewRepo, actor: &Actor) -> Result<Repo, StoreError> {
        check_slug(&repo.slug)?;
        let max_jobs = i32::try_from(repo.max_concurrent_jobs)
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| StoreError::Invalid {
                field: "max_concurrent_jobs",
                message: format!("{}: from 1 to {}", repo.max_concurrent_jobs, i32::MAX),
            })?;
        let mut tx = self.pool.begin().await?;
        let created: Repo = sqlx::query_as!(
            RepoRow,
            "INSERT INTO repos (slug, url, default_branch, language, mode, max_concurrent_jobs, config)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, slug, url, default_branch, language, mode, max_concurrent_jobs, config, created_at",
            repo.slug,
            repo.url,
            repo.default_branch,
            repo.language,
            repo.mode.as_str(),
            max_jobs,
            repo.config,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::from_db)?
        .try_into()?;
        let detail = serde_json::json!({
            "slug": created.slug,
            "url": created.url,
            "mode": created.mode.as_str(),
            "max_concurrent_jobs": created.max_concurrent_jobs,
            "config": created.config,
        });
        let target = Target {
            repo: Some(created.id),
            job: None,
        };
        audit::record(&mut tx, actor, "repo.register", target, detail).await?;
        tx.commit().await?;
        Ok(created)
    }

    pub async fn repo(&self, id: RepoId) -> Result<Option<Repo>, StoreError> {
        sqlx::query_as!(
            RepoRow,
            "SELECT id, slug, url, default_branch, language, mode, max_concurrent_jobs, config, created_at
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
            "SELECT id, slug, url, default_branch, language, mode, max_concurrent_jobs, config, created_at
             FROM repos WHERE slug = $1",
            slug,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Repo::try_from)
        .transpose()
    }

    /// The repo registered for `remote`: its URL (`https://github.com/typst/typst`,
    /// with or without `.git` or a trailing `/`) or its path `owner/name`
    /// (`group/sub/project` on GitLab), matched against the end of the
    /// URLs. Case does not matter. `Conflict` when more than one repo
    /// matches: which one is meant is not for this to guess.
    pub async fn repo_by_remote(&self, remote: &str) -> Result<Option<Repo>, StoreError> {
        let remote = normalized_remote(remote);
        let (url, path) = if remote.contains("://") {
            (Some(remote.as_str()), None)
        } else {
            (None, Some(remote.trim_start_matches('/')))
        };
        // A bare name would match any owner's repo of that name.
        if url.is_none() && !path.is_some_and(|p| p.contains('/')) {
            return Ok(None);
        }
        let rows = sqlx::query_as!(
            RepoRow,
            r"SELECT id, slug, url, default_branch, language, mode, max_concurrent_jobs, config, created_at
              FROM repos
              WHERE lower(regexp_replace(url, '(\.git)?/*$', '')) = $1
                 OR right(lower(regexp_replace(url, '(\.git)?/*$', '')), length($2) + 1) = '/' || $2
              ORDER BY id LIMIT 2",
            url,
            path,
        )
        .fetch_all(&self.pool)
        .await?;
        match <[RepoRow; 2]>::try_from(rows) {
            Ok([a, b]) => Err(StoreError::Conflict(format!(
                "`{remote}` matches repos {} and {}",
                a.slug, b.slug
            ))),
            Err(rows) => rows.into_iter().next().map(Repo::try_from).transpose(),
        }
    }

    /// Oldest first.
    pub async fn list_repos(&self, page: Page) -> Result<PageOf<Repo>, StoreError> {
        let rows = sqlx::query_as!(
            RepoRow,
            "SELECT id, slug, url, default_branch, language, mode, max_concurrent_jobs, config, created_at
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_are_compared_without_case_slash_or_git() {
        for remote in [
            "https://GitHub.com/typst/typst",
            "https://github.com/typst/typst/",
            " https://github.com/typst/typst.git/ ",
        ] {
            assert_eq!(normalized_remote(remote), "https://github.com/typst/typst");
        }
        assert_eq!(normalized_remote("Typst/Typst"), "typst/typst");
    }
}
