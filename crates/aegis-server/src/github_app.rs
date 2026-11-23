//! The instance's GitHub App (ADR 0017 §3): its key read from the secret
//! store (#30) when a token is needed, so a rotated key is used at once,
//! and one [`GitHubApp`] (with its installation and token caches) per key.

use std::sync::{Arc, Mutex, PoisonError};

use aegis_pr::{AppKey, Gate, GitHubApp};
use aegis_secrets::Secrets;
use aegis_store::{Store, TenantId, binding_refusal};

use crate::config::GithubAppSection;

pub struct AppHandle {
    section: GithubAppSection,
    issuer: String,
    secrets: Secrets,
    /// The app made from the key last read, with that key.
    current: Mutex<Option<(String, Arc<GitHubApp>)>>,
    /// Where an installation's tenant is read before each token (ADR 0017
    /// §6); without it (tests) every token is given.
    store: Option<Store>,
}

impl std::fmt::Debug for AppHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppHandle")
            .field("api", &self.section.api)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl AppHandle {
    pub fn new(section: GithubAppSection, secrets: Secrets) -> Result<Self, String> {
        let issuer = section.issuer()?;
        Ok(Self {
            section,
            issuer,
            secrets,
            current: Mutex::default(),
            store: None,
        })
    }

    /// The same handle, whose app gives a token only for a tenant its
    /// installation is bound to, as `store` says at each token.
    pub fn with_store(self, store: Store) -> Self {
        Self {
            store: Some(store),
            ..self
        }
    }

    /// The name of the app webhook's secret in the store.
    pub fn webhook_secret_env(&self) -> &str {
        &self.section.webhook_secret_env
    }

    /// The app's API root, the default of repos that name none.
    pub fn api(&self) -> &str {
        &self.section.api
    }

    /// The app with the key in the store now: the one made before while
    /// the key is the same, a new one (with empty caches) when it changed.
    pub async fn app(&self) -> Result<Arc<GitHubApp>, String> {
        let name = &self.section.private_key_env;
        let pem = self
            .secrets
            .get_or_load(name)
            .await
            .map_err(|e| format!("reading {name}: {e}"))?
            .ok_or_else(|| {
                format!(
                    "no secret {name} (the GitHub App's private key) in {}",
                    self.secrets.source()
                )
            })?;
        let pem = pem.expose();
        {
            let current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((key, app)) = current.as_ref()
                && key == pem
            {
                return Ok(Arc::clone(app));
            }
        }
        let key = AppKey::from_pem(pem).map_err(|e| format!("{name}: {e}"))?;
        let app =
            GitHubApp::new(&self.section.api, &self.issuer, key).map_err(|e| e.to_string())?;
        let app = Arc::new(match &self.store {
            Some(store) => app.with_gate(tenant_gate(store.clone())),
            None => app,
        });
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((pem.to_owned(), Arc::clone(&app)));
        Ok(app)
    }
}

/// The gate of ADR 0017 §6: a token of installation `i` only for a tenant
/// it is bound to.
fn tenant_gate(store: Store) -> Gate {
    Arc::new(move |installation: u64, holder: Option<i64>| {
        let store = store.clone();
        Box::pin(async move {
            let id = i64::try_from(installation)
                .map_err(|_| format!("installation {installation}: out of range"))?;
            let tenant = holder
                .map(TenantId)
                .ok_or("a token of the GitHub App asked for no tenant")?;
            let bound = store
                .installation_tenant(id)
                .await
                .map_err(|e| e.to_string())?;
            binding_refusal(id, bound, tenant).map_or(Ok(()), Err)
        })
    })
}

/// Why `settings` on the app path may not be registered for `tenant`
/// (ADR 0017 §6): the app's installation on the repo's account is not
/// recorded, or not bound to `tenant`. `Ok` on the token path.
pub async fn check_binding(
    store: &Store,
    settings: &aegis_lifecycle::RepoSettings,
    url: &str,
    tenant: TenantId,
) -> Result<(), String> {
    if settings.app.is_none() {
        return Ok(());
    }
    let (owner, name) = aegis_lifecycle::upstream_of(url)
        .ok_or_else(|| format!("`{url}` is not a GitHub repo's URL"))?;
    let state = store
        .app_on_repo(&owner, &name)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "the GitHub App is not known to be installed on `{owner}`: install it, and \
                 register the repo once its `installation` event has arrived"
            )
        })?;
    let id = match state {
        aegis_store::AppOnRepo::Installed { installation }
        | aegis_store::AppOnRepo::Suspended { installation }
        | aegis_store::AppOnRepo::Removed { installation }
        | aegis_store::AppOnRepo::NotGiven { installation } => installation,
    };
    let bound = store
        .installation_tenant(id)
        .await
        .map_err(|e| e.to_string())?;
    binding_refusal(id, bound, tenant).map_or(Ok(()), Err)
}

/// The token a repo's clone is made or fetched with: the secret its fork
/// names on the token path (none when the store lacks it: a public repo),
/// a read token of the app for `tenant` on the app path.
pub async fn read_token(
    secrets: &Secrets,
    app: Option<&AppHandle>,
    settings: &aegis_lifecycle::RepoSettings,
    url: &str,
    tenant: TenantId,
) -> Result<Option<String>, String> {
    if let Some(fork) = &settings.fork {
        let Some(secret) = secrets
            .get_or_load(&fork.token_env)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        // A service principal's secret is exchanged for a token (#636).
        return aegis_lifecycle::fork_token(fork, secret.expose())
            .await
            .map(Some)
            .map_err(|e| e.to_string());
    }
    let app = app.ok_or("`app` needs the instance's GitHub App, `[github_app]` in server.toml")?;
    let (owner, name) = aegis_lifecycle::upstream_of(url)
        .ok_or_else(|| format!("`{url}` is not a GitHub repo's URL"))?;
    let token = app
        .app()
        .await?
        .token(&owner, &name, aegis_pr::TokenUse::Read, Some(tenant.0))
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(token))
}
