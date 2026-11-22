//! The language a repo's issue comments are written in when its job ends
//! (#589), resolved as for its jobs ([`aegis_lifecycle::resolve_locale`]):
//! the repo's `locale`, its tenant's `default_locale`, the server's
//! `[web] default_locale`.

use aegis_pr::Locale;
use aegis_store::{RepoId, Store};

/// Looks the language up when a comment is about to be written, so a
/// change of the repo's or tenant's setting holds for the next comment.
#[derive(Debug, Clone, Default)]
pub struct Locales {
    /// `None`: every repo writes in `default`.
    store: Option<Store>,
    default: Locale,
}

impl Locales {
    pub fn new(store: Store, default: Locale) -> Self {
        Self {
            store: Some(store),
            default,
        }
    }

    /// Every repo in `default`, with no store to read settings from.
    pub fn fixed(default: Locale) -> Self {
        Self {
            store: None,
            default,
        }
    }

    /// `repo`'s language; the server's default when the repo or its
    /// tenant cannot be read, which is logged.
    pub async fn of(&self, repo: RepoId) -> Locale {
        let Some(store) = &self.store else {
            return self.default;
        };
        match self.lookup(store, repo).await {
            Ok(locale) => locale,
            Err(e) => {
                tracing::warn!(repo_id = repo.0, error = %e, "the repo's locale");
                self.default
            }
        }
    }

    async fn lookup(&self, store: &Store, id: RepoId) -> Result<Locale, String> {
        let repo = store
            .repo(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no repo {}", id.0))?;
        let own: Option<Locale> = match repo.config.get("locale") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => {
                Some(serde_json::from_value(v.clone()).map_err(|e| format!("its locale: {e}"))?)
            }
        };
        if own.is_some() {
            return aegis_lifecycle::resolve_locale(own, None, self.default);
        }
        let tenant = store
            .tenant(repo.tenant_id)
            .await
            .map_err(|e| e.to_string())?;
        aegis_lifecycle::resolve_locale(
            None,
            tenant.as_ref().and_then(|t| t.default_locale.as_deref()),
            self.default,
        )
    }
}
