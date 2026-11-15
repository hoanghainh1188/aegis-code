//! The GitHub App's installations (ADR 0017 §5, #540), as its webhook
//! told them: kept so a repo on the app path whose installation was
//! suspended or removed takes no job, says why, and so the dashboard can
//! list installed repos not yet registered.

use chrono::{DateTime, Utc};

use crate::audit::{self, Target};
use crate::tx;
use crate::{Actor, Store, StoreError, TenantId};

/// Whether an installation was given all the account's repos or some.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoSelection {
    All,
    Selected,
}

impl RepoSelection {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "all" => Some(Self::All),
            "selected" => Some(Self::Selected),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Selected => "selected",
        }
    }
}

/// An installation as an `installation.created` (or a later event that
/// carries the whole installation) says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationSeen {
    pub id: i64,
    pub account: String,
    pub selection: RepoSelection,
    /// `owner/name` of the repos the event names.
    pub repositories: Vec<String>,
}

/// What the store knows of the app on a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppOnRepo {
    /// An installation on the repo's account has it.
    Installed { installation: i64 },
    /// The installation on its account is suspended.
    Suspended { installation: i64 },
    /// The installation on its account was removed.
    Removed { installation: i64 },
    /// The installation on its account was given other repos only.
    NotGiven { installation: i64 },
}

impl AppOnRepo {
    /// Why no job runs for the repo, when none does.
    pub fn refusal(&self, repo: &str) -> Option<String> {
        match self {
            Self::Installed { .. } => None,
            Self::Suspended { installation } => Some(format!(
                "the GitHub App's installation {installation} on `{repo}`'s account is suspended"
            )),
            Self::Removed { installation } => Some(format!(
                "the GitHub App's installation {installation} on `{repo}`'s account was removed"
            )),
            Self::NotGiven { installation } => Some(format!(
                "the GitHub App's installation {installation} was not given `{repo}`"
            )),
        }
    }
}

fn lower(names: &[String]) -> Vec<String> {
    let mut names: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn checked(id: i64, account: &str) -> Result<(), StoreError> {
    if id <= 0 {
        return Err(StoreError::Invalid {
            field: "installation",
            message: format!("{id}: not an installation id"),
        });
    }
    if account.is_empty() || account.len() > 255 {
        return Err(StoreError::Invalid {
            field: "account",
            message: "1 to 255 characters".into(),
        });
    }
    Ok(())
}

impl Store {
    /// Records `seen` as installed (again): not suspended, not removed,
    /// with the repos it names.
    pub async fn installation_seen(&self, seen: &InstallationSeen) -> Result<(), StoreError> {
        checked(seen.id, &seen.account)?;
        let repos = lower(&seen.repositories);
        sqlx::query!(
            "INSERT INTO github_installations (id, account, repository_selection, repositories)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET
                 account = EXCLUDED.account,
                 repository_selection = EXCLUDED.repository_selection,
                 repositories = EXCLUDED.repositories,
                 suspended = false,
                 removed = false,
                 updated_at = now()",
            seen.id,
            seen.account,
            seen.selection.as_str(),
            &repos,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Installation `id` suspended or no longer; `false` when the store
    /// does not know it.
    pub async fn installation_suspended(
        &self,
        id: i64,
        suspended: bool,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query!(
            "UPDATE github_installations SET suspended = $2, updated_at = now() WHERE id = $1",
            id,
            suspended,
        )
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Installation `id` removed from its account. Kept, so its repos say
    /// why they take no job.
    pub async fn installation_removed(&self, id: i64, account: &str) -> Result<(), StoreError> {
        checked(id, account)?;
        sqlx::query!(
            "INSERT INTO github_installations (id, account, repository_selection, removed)
             VALUES ($1, $2, 'selected', true)
             ON CONFLICT (id) DO UPDATE SET removed = true, updated_at = now()",
            id,
            account,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Repos given to or taken from installation `id`
    /// (`installation_repositories`), with its selection now.
    pub async fn installation_repos_changed(
        &self,
        id: i64,
        account: &str,
        selection: RepoSelection,
        added: &[String],
        removed: &[String],
    ) -> Result<(), StoreError> {
        checked(id, account)?;
        let (added, removed) = (lower(added), lower(removed));
        sqlx::query!(
            "INSERT INTO github_installations (id, account, repository_selection, repositories)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET
                 account = EXCLUDED.account,
                 repository_selection = EXCLUDED.repository_selection,
                 repositories = ARRAY(
                     SELECT DISTINCT r FROM unnest(github_installations.repositories || $4::text[]) AS r
                     WHERE r <> ALL($5::text[])
                     ORDER BY r),
                 updated_at = now()",
            id,
            account,
            selection.as_str(),
            &added,
            &removed,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// What the store knows of the app on `owner/name`; `None` when no
    /// installation on `owner` was ever recorded (the app's token will
    /// tell). An installation that has the repo wins over others.
    pub async fn app_on_repo(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<Option<AppOnRepo>, StoreError> {
        let full = format!("{owner}/{name}").to_ascii_lowercase();
        let rows = sqlx::query!(
            "SELECT id, repository_selection, repositories, suspended, removed
             FROM github_installations
             WHERE lower(account) = lower($1)
             ORDER BY removed, suspended, updated_at DESC",
            owner,
        )
        .fetch_all(&self.pool)
        .await?;
        let state = |r: &RowState| {
            let has = r.selection == "all" || r.repositories.contains(&full);
            match (r.removed, r.suspended, has) {
                (true, _, _) => AppOnRepo::Removed { installation: r.id },
                (false, true, _) => AppOnRepo::Suspended { installation: r.id },
                (false, false, true) => AppOnRepo::Installed { installation: r.id },
                (false, false, false) => AppOnRepo::NotGiven { installation: r.id },
            }
        };
        let rows: Vec<RowState> = rows
            .into_iter()
            .map(|r| RowState {
                id: r.id,
                selection: r.repository_selection,
                repositories: r.repositories,
                suspended: r.suspended,
                removed: r.removed,
            })
            .collect();
        let states: Vec<AppOnRepo> = rows.iter().map(state).collect();
        Ok(states
            .iter()
            .find(|s| matches!(s, AppOnRepo::Installed { .. }))
            .or_else(|| states.first())
            .cloned())
    }

    /// `owner/name` of the repos live installations were given, as events
    /// named them, that no registered repo's URL ends with.
    pub async fn installed_unregistered(&self) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query_scalar!(
            r#"SELECT DISTINCT r AS "repo!"
               FROM github_installations, unnest(repositories) AS r
               WHERE NOT removed AND NOT suspended
                 AND NOT EXISTS (
                     SELECT 1 FROM repos
                     WHERE right(lower(regexp_replace(url, '(\.git)?/*$', '')), length(r) + 1) = '/' || r)
               ORDER BY 1
               LIMIT 500"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

/// An installation as the admin sees it, to bind it to a tenant.
#[derive(Debug, Clone, PartialEq)]
pub struct Installation {
    pub id: i64,
    pub account: String,
    pub selection: RepoSelection,
    pub repositories: Vec<String>,
    pub suspended: bool,
    pub removed: bool,
    /// `None`: not bound, no token is minted for it.
    pub tenant: Option<TenantId>,
    pub updated_at: DateTime<Utc>,
}

/// Why no token of installation `id` is minted for `tenant`'s repo, if it
/// is not (ADR 0017 §6): the installation is unknown, not bound, or bound
/// to another tenant.
pub fn binding_refusal(
    id: i64,
    bound: Option<Option<TenantId>>,
    tenant: TenantId,
) -> Option<String> {
    match bound {
        None => Some(format!(
            "the GitHub App's installation {id} is not recorded yet: its `installation` event has not arrived"
        )),
        Some(None) => Some(format!(
            "the GitHub App's installation {id} is not bound to a tenant: \
             a platform admin binds it (`aegis-server tenant bind-installation {id} <tenant>`)"
        )),
        Some(Some(t)) if t != tenant => Some(format!(
            "the GitHub App's installation {id} belongs to another tenant"
        )),
        Some(Some(_)) => None,
    }
}

impl Store {
    /// Every installation the app's events told, bound or not.
    pub async fn installations(&self) -> Result<Vec<Installation>, StoreError> {
        let rows = sqlx::query!(
            "SELECT id, account, repository_selection, repositories, suspended, removed,
                    tenant_id, updated_at
             FROM github_installations ORDER BY lower(account), id LIMIT 1000"
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Installation {
                id: r.id,
                account: r.account,
                selection: RepoSelection::parse(&r.repository_selection)
                    .unwrap_or(RepoSelection::Selected),
                repositories: r.repositories,
                suspended: r.suspended,
                removed: r.removed,
                tenant: r.tenant_id.map(TenantId),
                updated_at: r.updated_at,
            })
            .collect())
    }

    /// The tenant installation `id` is bound to: `None` when the store does
    /// not know it, `Some(None)` when it is not bound.
    pub async fn installation_tenant(
        &self,
        id: i64,
    ) -> Result<Option<Option<TenantId>>, StoreError> {
        let row = sqlx::query_scalar!(
            "SELECT tenant_id FROM github_installations WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|t| t.map(TenantId)))
    }

    /// Binds installation `id` to `tenant`, or unbinds it (`None`),
    /// audited `installation.tenant`. A bound installation is moved only
    /// after unbinding it, so a slip cannot hand one tenant's org to
    /// another.
    pub async fn bind_installation_by(
        &self,
        id: i64,
        tenant: Option<TenantId>,
        actor: &Actor,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let work = async {
            let was = sqlx::query_scalar!(
                "SELECT tenant_id FROM github_installations WHERE id = $1 FOR UPDATE",
                id
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("installation {id}")))?;
            if let (Some(was), Some(new)) = (was, tenant)
                && was != new.0
            {
                return Err(StoreError::Conflict(format!(
                    "installation {id} is bound to tenant {was}: unbind it first"
                )));
            }
            sqlx::query!(
                "UPDATE github_installations SET tenant_id = $2, updated_at = now() WHERE id = $1",
                id,
                tenant.map(|t| t.0),
            )
            .execute(&mut *tx)
            .await
            .map_err(|e| match StoreError::from_db(e) {
                StoreError::NotFound(_) => {
                    StoreError::NotFound(format!("tenant {}", tenant.map_or(0, |t| t.0)))
                }
                other => other,
            })?;
            let target = Target {
                repo: None,
                job: None,
            };
            let detail = serde_json::json!({
                "installation": id,
                "tenant": tenant.map(|t| t.0),
                "was": was,
            });
            audit::record(&mut tx, actor, "installation.tenant", target, detail).await?;
            Ok::<_, StoreError>(())
        }
        .await;
        tx::commit(tx, work).await
    }
}

struct RowState {
    id: i64,
    selection: String,
    repositories: Vec<String>,
    suspended: bool,
    removed: bool,
}
