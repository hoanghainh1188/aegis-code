//! The instance's GitHub App (ADR 0017 §3): its key read from the secret
//! store (#30) when a token is needed, so a rotated key is used at once,
//! and one [`GitHubApp`] (with its installation and token caches) per key.

use std::sync::{Arc, Mutex, PoisonError};

use aegis_pr::{AppKey, GitHubApp};
use aegis_secrets::Secrets;

use crate::config::GithubAppSection;

pub struct AppHandle {
    section: GithubAppSection,
    issuer: String,
    secrets: Secrets,
    /// The app made from the key last read, with that key.
    current: Mutex<Option<(String, Arc<GitHubApp>)>>,
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
        })
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
        let app = Arc::new(
            GitHubApp::new(&self.section.api, &self.issuer, key).map_err(|e| e.to_string())?,
        );
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((pem.to_owned(), Arc::clone(&app)));
        Ok(app)
    }
}

/// The token a repo's clone is made or fetched with: the secret its fork
/// names on the token path (none when the store lacks it: a public repo),
/// a read token of the app on the app path.
pub async fn read_token(
    secrets: &Secrets,
    app: Option<&AppHandle>,
    settings: &aegis_lifecycle::RepoSettings,
    url: &str,
) -> Result<Option<String>, String> {
    if let Some(fork) = &settings.fork {
        return Ok(secrets
            .get_or_load(&fork.token_env)
            .await
            .map_err(|e| e.to_string())?
            .map(|t| t.expose().to_owned()));
    }
    let app = app.ok_or("`app` needs the instance's GitHub App, `[github_app]` in server.toml")?;
    let (owner, name) = aegis_lifecycle::upstream_of(url)
        .ok_or_else(|| format!("`{url}` is not a GitHub repo's URL"))?;
    let token = app
        .app()
        .await?
        .token(&owner, &name, aegis_pr::TokenUse::Read)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(token))
}
