//! The server's secrets (ADR 0003 §6, #30): read from AWS Secrets Manager
//! at start, kept in memory only, read again every `refresh_s` and when a
//! call with one is refused. `[secrets] source = "env"` reads the process
//! environment instead, for dev and tests only; there is no default.
//!
//! A deployment that runs `customer-jp` repos reads its secrets in
//! `ap-northeast-1`, like everything else of theirs.
//!
//! Secret names: [`DATABASE_URL`], the webhook secrets, the comment token,
//! each mode's `api_key_env`, each repo's `fork.token_env` and
//! `sonar.token_env`. In Secrets Manager, `<prefix><name>`.

use std::sync::OnceLock;
use std::time::Duration;

use aegis_secrets::{EnvSource, Secrets};
use aegis_store::Store;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::Role;
use crate::webhook::{GITHUB_SECRET_ENV, GITLAB_TOKEN_ENV};

pub const DATABASE_URL: &str = "AEGIS_DATABASE_URL";
pub const JAPAN_REGION: &str = "ap-northeast-1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Aws,
    /// Dev and tests only.
    Env,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsSection {
    pub source: SourceKind,
    /// Required with `aws`.
    #[serde(default)]
    pub region: Option<String>,
    /// Before each name in Secrets Manager, e.g. `aegis/prod/`.
    #[serde(default)]
    pub prefix: String,
    #[serde(default = "default_refresh_s")]
    pub refresh_s: u64,
}

fn default_refresh_s() -> u64 {
    300
}

impl SecretsSection {
    pub fn refresh_every(&self) -> Duration {
        Duration::from_secs(self.refresh_s.max(1))
    }

    /// Checks the section; `customer_jp` when that mode is configured.
    pub fn check(&self, customer_jp: bool) -> Result<(), String> {
        match (&self.source, self.region.as_deref()) {
            (SourceKind::Aws, None) => Err("[secrets]: `region` is required with `aws`".into()),
            (SourceKind::Aws, Some(region)) if customer_jp && region != JAPAN_REGION => {
                Err(format!(
                    "[secrets]: a deployment with `customer_jp` reads its secrets in {JAPAN_REGION}, not {region}"
                ))
            }
            (SourceKind::Env, Some(_)) => Err("[secrets]: `region` is for `aws`, not `env`".into()),
            _ => Ok(()),
        }
    }

    /// The store; nothing is read yet.
    pub async fn open(&self, customer_jp: bool) -> Result<Secrets, String> {
        self.check(customer_jp)?;
        match self.source {
            SourceKind::Aws => {
                let region = self.region.as_deref().unwrap_or_default();
                Ok(Secrets::new(
                    aegis_secrets::AwsSource::new(region, &self.prefix).await,
                ))
            }
            SourceKind::Env => {
                tracing::warn!("secrets are read from the environment: for dev and tests only");
                Ok(Secrets::new(EnvSource::for_dev_and_tests()))
            }
        }
    }
}

/// `[secrets]` is required: reading the environment must be chosen.
pub fn section(section: Option<&SecretsSection>) -> Result<&SecretsSection, String> {
    section.ok_or_else(|| {
        "[secrets] is required: `source = \"aws\"` with `region`, or `source = \"env\"` for dev and tests".into()
    })
}

/// What the process cannot start without, read now: the database URL;
/// with the API, also the webhook secrets, which may be absent (their
/// endpoints stay closed). A mode's API key is checked by
/// [`crate::lifecycle::LifecycleRunner::new`].
pub async fn load_at_start(secrets: &Secrets, role: Role) -> Result<(), String> {
    secrets
        .require(&[DATABASE_URL])
        .await
        .map_err(|e| e.to_string())?;
    if role.serves_api() {
        for name in [GITHUB_SECRET_ENV, GITLAB_TOKEN_ENV] {
            secrets.load(name).await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

static INSTALLED: OnceLock<Secrets> = OnceLock::new();

/// Makes `secrets` the process's, for parts built without a handle
/// ([`crate::webhook::WebhookConfig::from_env`]). Once only.
pub fn install(secrets: &Secrets) {
    if INSTALLED.set(secrets.clone()).is_err() {
        tracing::warn!("the secret store was installed already");
    }
}

/// The process's secrets; none when not installed (tests).
pub fn installed() -> Secrets {
    INSTALLED.get().cloned().unwrap_or_else(Secrets::none)
}

/// Reads every known secret again every `every` until `shutdown`; a new
/// database URL is used by new connections.
pub async fn keep_fresh(
    secrets: Secrets,
    store: Store,
    every: Duration,
    shutdown: CancellationToken,
) {
    let mut ticks = tokio::time::interval(every);
    ticks.tick().await;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let changed = secrets.refresh().await;
        if changed.iter().any(|name| name == DATABASE_URL)
            && let Some(url) = secrets.get(DATABASE_URL)
            && let Err(e) = store.set_url(url.expose())
        {
            tracing::error!(error = %e, "the rotated database URL does not parse; keeping the old one");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> SecretsSection {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn customer_jp_reads_its_secrets_in_tokyo() {
        let tokyo = parse("source = \"aws\"\nregion = \"ap-northeast-1\"\nprefix = \"aegis/\"");
        assert!(tokyo.check(true).is_ok());
        let virginia = parse("source = \"aws\"\nregion = \"us-east-1\"");
        assert!(virginia.check(false).is_ok());
        let err = virginia.check(true).unwrap_err();
        assert!(err.contains("ap-northeast-1"), "{err}");
        assert!(parse("source = \"aws\"").check(false).is_err());
    }

    #[test]
    fn the_environment_must_be_chosen() {
        let err = section(None).unwrap_err();
        assert!(err.contains("[secrets] is required"), "{err}");
        let env = parse("source = \"env\"");
        assert_eq!(env.source, SourceKind::Env);
        assert!(env.check(true).is_ok());
        assert!(toml::from_str::<SecretsSection>("").is_err());
        assert!(toml::from_str::<SecretsSection>("source = \"file\"").is_err());
    }

    #[tokio::test]
    async fn an_aws_store_names_its_region() {
        let section =
            parse("source = \"aws\"\nregion = \"ap-northeast-1\"\nprefix = \"aegis/prod/\"");
        let secrets = section.open(true).await.unwrap();
        assert!(
            secrets.source().contains("ap-northeast-1"),
            "{}",
            secrets.source()
        );
    }
}
