//! Many repos (#24): each registered repo (`aegis-store`'s `repos`) runs
//! with the configuration of its mode, `poc` or `customer-jp`, and its own
//! settings: language profile and overrides, Docker image, Sonar project,
//! allow-list, license and advisory policy, conventions for the prompt.
//! [`Modes::run_config`] puts the two together into the [`RunConfig`] of
//! one repo's jobs; nothing else is shared between repos.

use std::path::Path;

use aegis_lang::Language;
use aegis_ledger::Tier;
use aegis_provider::ProviderConfig;
use aegis_router::ModelsConfig;
use aegis_sonar_batch::PollConfig;
use aegis_store::{Actor, NewRepo, Repo, RepoId, RepoMode, Store};
use aegis_tracker::TrackerConfig;
use serde::{Deserialize, Serialize};

use crate::LifecycleError;
use crate::config::{
    BudgetSection, DirConfig, ExtraServer, ForkConfig, IdentityConfig, RepoConfig, RunConfig,
    RunSection, SonarConfig,
};

/// Bedrock in Tokyo for `customer-jp` (design §1, §7).
const JAPAN_REGION: &str = "ap-northeast-1";
const JAPAN_PROFILE_PREFIX: &str = "jp.";

/// What a run configuration (`config/poc.toml`, `config/customer-jp.toml`)
/// gives every repo of its mode. Its `[repo]` and `[sonar]` are for CLI
/// runs of one repo and are left out: a registered repo brings its own.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeConfig {
    pub provider: ProviderConfig,
    pub models: ModelsConfig,
    pub run: RunSection,
    #[serde(default)]
    pub budget: BudgetSection,
    pub identity: IdentityConfig,
    #[serde(default)]
    pub codebase_memory: Option<ExtraServer>,
}

impl ModeConfig {
    pub fn from_toml(text: &str) -> Result<Self, LifecycleError> {
        let mut table: toml::Table =
            toml::from_str(text).map_err(|e| LifecycleError::Config(e.to_string()))?;
        table.remove("repo");
        table.remove("sonar");
        table
            .try_into()
            .map_err(|e: toml::de::Error| LifecycleError::Config(e.to_string()))
    }

    /// Reads `path`; relative paths in it are taken from `base`.
    pub fn load(path: &Path, base: &Path) -> Result<Self, LifecycleError> {
        let text = std::fs::read_to_string(path).map_err(|e| LifecycleError::io(path, &e))?;
        let mut mode = Self::from_toml(&text)?;
        for p in [&mut mode.run.state_dir, &mut mode.run.pricing] {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        if let Some(p) = mode.run.aci_bin.as_mut().filter(|p| p.is_relative()) {
            *p = base.join(&*p);
        }
        if let Some(cbm) = mode.codebase_memory.as_mut() {
            cbm.program = cbm.program_from(base);
        }
        Ok(mode)
    }

    /// The `customer-jp` guard (design §7): Bedrock in Tokyo with the
    /// `jp.` check on, and a `jp.` inference profile for every tier, so a
    /// customer repo cannot be sent anywhere else by a wrong line.
    pub fn check(&self, mode: RepoMode) -> Result<(), LifecycleError> {
        match mode {
            RepoMode::Poc => Ok(()),
            RepoMode::CustomerJp => self.check_japan_only(),
        }
    }

    fn check_japan_only(&self) -> Result<(), LifecycleError> {
        let refuse = |why: String| Err(LifecycleError::Config(format!("customer-jp: {why}")));
        match &self.provider {
            ProviderConfig::Bedrock {
                region,
                require_jp_profile: true,
            } if region == JAPAN_REGION => {}
            ProviderConfig::Bedrock { region, .. } if region != JAPAN_REGION => {
                return refuse(format!("Bedrock region `{region}`, not `{JAPAN_REGION}`"));
            }
            ProviderConfig::Bedrock { .. } => {
                return refuse("`require_jp_profile` must be true".into());
            }
            ProviderConfig::ClaudeApi { .. } | ProviderConfig::Local { .. } => {
                return refuse(format!("the provider must be Bedrock in `{JAPAN_REGION}`"));
            }
        }
        for tier in [Tier::Light, Tier::Core, Tier::Expert] {
            let id = &self.models.for_tier(tier).id;
            if !id.starts_with(JAPAN_PROFILE_PREFIX) {
                return refuse(format!(
                    "model `{id}` of {tier:?} is not a `{JAPAN_PROFILE_PREFIX}` inference profile"
                ));
            }
        }
        Ok(())
    }
}

/// A repo's own settings, kept in `repos.config`. The columns (URL, main
/// branch, language, mode, `max_concurrent_jobs`) are in [`Repo`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSettings {
    /// The repo maintainers' conventions, added to the system prompt.
    #[serde(default)]
    pub conventions: Option<String>,
    /// The gate's image, in place of the language profile's; wins over an
    /// `image` in `overrides`.
    #[serde(default)]
    pub image: Option<String>,
    /// Overrides of the language profile, as `[repo.overrides]`.
    #[serde(default)]
    pub overrides: toml::Table,
    /// Directories with their own profile, as `[[repo.dirs]]` (#71).
    #[serde(default)]
    pub dirs: Vec<DirConfig>,
    /// Packages a patch may add to this repo.
    #[serde(default)]
    pub allowed_packages: Vec<String>,
    /// Licenses its dependencies may have; `policies/licenses.toml` when
    /// unset.
    #[serde(default)]
    pub licenses: Option<Vec<String>>,
    #[serde(default)]
    pub ignored_advisories: Vec<String>,
    #[serde(default)]
    pub hidden_test: Option<Vec<String>>,
    /// This repo's budget, in place of its mode's.
    #[serde(default)]
    pub budget: Option<BudgetSection>,
    /// Its Sonar project; no Sonar step without it.
    #[serde(default)]
    pub sonar: Option<SonarConfig>,
    /// The Jira or Backlog whose tickets it takes (#26): a filter polled
    /// by the server, the ticket told how its job ended.
    #[serde(default)]
    pub tracker: Option<TrackerConfig>,
    /// The schedule on which the server polls its Sonar project for new
    /// warnings and batches them (#27); needs `sonar`.
    #[serde(default)]
    pub sonar_poll: Option<PollConfig>,
    /// Who may trigger a job by labeling an issue (#358).
    #[serde(default, skip_serializing_if = "is_default_issue_trust")]
    pub issue_trust: IssueTrustConfig,
    pub fork: ForkConfig,
}

impl RepoSettings {
    pub fn issue_trust_policy(&self) -> IssueTrustPolicy {
        self.issue_trust.policy()
    }
}

/// Who may trigger a job by labeling an issue (#358).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueTrustPolicy {
    /// Any issue author triggers a job when the trigger label is added.
    #[default]
    Any,
    /// Only issue authors who can write to the repo trigger a job.
    #[serde(
        alias = "collaborators_only",
        alias = "members",
        alias = "members_only"
    )]
    Collaborators,
}

impl IssueTrustPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Collaborators => "collaborators",
        }
    }
}

/// The issue trust configuration: either a direct string or a table with `policy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IssueTrustConfig {
    Direct(IssueTrustPolicy),
    Table {
        #[serde(default)]
        policy: IssueTrustPolicy,
    },
}

impl Default for IssueTrustConfig {
    fn default() -> Self {
        Self::Direct(IssueTrustPolicy::Any)
    }
}

impl IssueTrustConfig {
    pub fn policy(&self) -> IssueTrustPolicy {
        match self {
            Self::Direct(p) => *p,
            Self::Table { policy } => *policy,
        }
    }
}

fn is_default_issue_trust(c: &IssueTrustConfig) -> bool {
    c.policy() == IssueTrustPolicy::Any
}

/// What registering a repo takes.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoSpec {
    pub slug: String,
    pub url: String,
    pub default_branch: String,
    pub language: Language,
    pub mode: RepoMode,
    pub max_concurrent_jobs: u32,
    pub settings: RepoSettings,
}

/// A registered repo with the configuration its jobs run with.
#[derive(Debug, Clone)]
pub struct RepoRun {
    pub repo: Repo,
    pub config: RunConfig,
}

impl RepoRun {
    /// Applied by the queue (#23) on top of its global `max_workers`.
    pub fn max_concurrent_jobs(&self) -> u32 {
        self.repo.max_concurrent_jobs
    }
}

/// The configuration of each mode. A mode without one has no repos.
#[derive(Debug, Clone, Default)]
pub struct Modes {
    pub poc: Option<ModeConfig>,
    pub customer_jp: Option<ModeConfig>,
}

impl Modes {
    /// Fails if the `customer-jp` configuration does not pass its guard,
    /// before any repo uses it.
    pub fn new(
        poc: Option<ModeConfig>,
        customer_jp: Option<ModeConfig>,
    ) -> Result<Self, LifecycleError> {
        if let Some(jp) = &customer_jp {
            jp.check(RepoMode::CustomerJp)?;
        }
        Ok(Self { poc, customer_jp })
    }

    fn mode(&self, mode: RepoMode) -> Result<&ModeConfig, LifecycleError> {
        let config = match mode {
            RepoMode::Poc => self.poc.as_ref(),
            RepoMode::CustomerJp => self.customer_jp.as_ref(),
        };
        let config = config.ok_or_else(|| {
            LifecycleError::Config(format!("no configuration for mode `{}`", mode.as_str()))
        })?;
        // Again here: `Modes` has public fields.
        config.check(mode)?;
        Ok(config)
    }

    /// The configuration of `repo`'s jobs. Its clone is
    /// `<state_dir>/repos/<slug>`; module maps, caches and the
    /// Codebase-Memory index are under its slug.
    pub fn run_config(&self, repo: &Repo) -> Result<RunConfig, LifecycleError> {
        self.build(
            &repo.slug,
            &repo.default_branch,
            &repo.language,
            repo.mode,
            &repo.config,
        )
    }

    fn build(
        &self,
        slug: &str,
        default_branch: &str,
        language: &str,
        mode: RepoMode,
        config: &serde_json::Value,
    ) -> Result<RunConfig, LifecycleError> {
        let in_repo = |e: String| LifecycleError::Config(format!("repo `{slug}`: {e}"));
        let mode = self.mode(mode)?;
        let settings: RepoSettings =
            serde_json::from_value(config.clone()).map_err(|e| in_repo(e.to_string()))?;
        let language: Language = serde_json::from_value(serde_json::Value::String(language.into()))
            .map_err(|e| in_repo(format!("language: {e}")))?;
        if let Some(tracker) = &settings.tracker {
            tracker
                .check()
                .map_err(|e| in_repo(format!("tracker: {e}")))?;
        }
        if let Some(poll) = &settings.sonar_poll {
            if settings.sonar.is_none() {
                return Err(in_repo(
                    "sonar_poll needs `sonar`, the project to poll".into(),
                ));
            }
            poll.check().map_err(|e| in_repo(e.to_string()))?;
        }
        let mut overrides = settings.overrides;
        if let Some(image) = settings.image {
            overrides.insert("image".into(), toml::Value::String(image));
        }
        let path = mode.run.state_dir.join("repos").join(slug);
        RunConfig {
            provider: mode.provider.clone(),
            models: mode.models.clone(),
            run: mode.run.clone(),
            budget: settings.budget.unwrap_or(mode.budget),
            repo: RepoConfig {
                path,
                language,
                base: default_branch.to_owned(),
                key: slug.to_owned(),
                conventions: settings.conventions,
                overrides,
                dirs: settings.dirs,
                allowed_packages: None,
                allowed_package_names: settings.allowed_packages,
                licenses: settings.licenses,
                ignored_advisories: settings.ignored_advisories,
                hidden_test: settings.hidden_test,
                fork: settings.fork,
            },
            identity: mode.identity.clone(),
            codebase_memory: mode.codebase_memory.clone(),
            sonar: settings.sonar,
        }
        .check()
        .map_err(|e| match e {
            LifecycleError::Config(message) => in_repo(message),
            other => other,
        })
    }

    /// The repo `id` and its configuration, for the worker that runs its
    /// jobs (#23).
    pub async fn repo_run(&self, store: &Store, id: RepoId) -> Result<RepoRun, LifecycleError> {
        let repo = store
            .repo(id)
            .await?
            .ok_or_else(|| LifecycleError::Config(format!("no repo with id {}", id.0)))?;
        let config = self.run_config(&repo)?;
        Ok(RepoRun { repo, config })
    }

    /// The row `spec` is stored as and the configuration its jobs will
    /// run with, built as it will be read back: refused unless it builds
    /// now (a `customer-jp` repo needs a `customer-jp` mode that passes
    /// the guard).
    pub fn check_spec(&self, spec: &RepoSpec) -> Result<(NewRepo, RunConfig), LifecycleError> {
        let new = NewRepo {
            slug: spec.slug.clone(),
            url: spec.url.clone(),
            default_branch: spec.default_branch.clone(),
            language: spec.language.id().to_owned(),
            mode: spec.mode,
            max_concurrent_jobs: spec.max_concurrent_jobs,
            config: serde_json::to_value(&spec.settings)
                .map_err(|e| LifecycleError::Config(format!("repo `{}`: {e}", spec.slug)))?,
        };
        let config = self.build(
            &new.slug,
            &new.default_branch,
            &new.language,
            new.mode,
            &new.config,
        )?;
        Ok((new, config))
    }

    /// Registers a repo, refused unless its configuration can be built
    /// now ([`Modes::check_spec`]). Its clone is not touched: [`Modes::add`].
    pub async fn register(&self, store: &Store, spec: &RepoSpec) -> Result<Repo, LifecycleError> {
        self.register_by(store, spec, &Actor::cli()).await
    }

    /// [`Modes::register`], audited as `actor`: the pages register a repo
    /// first and make its clone after, with [`Modes::clone_repo`] (#330).
    pub async fn register_by(
        &self,
        store: &Store,
        spec: &RepoSpec,
        actor: &Actor,
    ) -> Result<Repo, LifecycleError> {
        let (new, _) = self.check_spec(spec)?;
        Ok(store.create_repo_by(&new, actor).await?)
    }

    /// [`Modes::update`] without touching the clone, audited as `actor`;
    /// [`Modes::clone_repo`] fetches it after (#330).
    pub async fn update_by(
        &self,
        store: &Store,
        spec: &RepoSpec,
        actor: &Actor,
    ) -> Result<Repo, LifecycleError> {
        let (new, _) = self.check_spec(spec)?;
        if store.repo_by_slug(&new.slug).await?.is_none() {
            return Err(LifecycleError::Config(format!("no repo `{}`", new.slug)));
        }
        Ok(store.update_repo_by(&new, actor).await?)
    }

    /// Makes `repo`'s clone at `<state_dir>/repos/<slug>`, or fetches it if
    /// one is there, `token` its fork's for a private repo.
    pub async fn clone_repo(&self, repo: &Repo, token: Option<&str>) -> Result<(), LifecycleError> {
        let config = self.run_config(repo)?;
        crate::clone::prepare_async(
            &config.repo.path,
            &repo.url,
            &repo.default_branch,
            auth(config.repo.fork.platform, token),
        )
        .await
    }

    /// Registers a repo as [`Modes::register`] does, with its clone made
    /// first at `<state_dir>/repos/<slug>` (or fetched, if one is there),
    /// `token` the fork's for a private repo (#234). A clone this made is
    /// removed again if the repo cannot be stored.
    pub async fn add(
        &self,
        store: &Store,
        spec: &RepoSpec,
        token: Option<&str>,
        actor: &Actor,
    ) -> Result<Repo, LifecycleError> {
        let (new, config) = self.check_spec(spec)?;
        let path = config.repo.path;
        let made = !path.exists();
        crate::clone::prepare_async(
            &path,
            &new.url,
            &new.default_branch,
            auth(spec.settings.fork.platform, token),
        )
        .await?;
        match store.create_repo_by(&new, actor).await {
            Ok(repo) => Ok(repo),
            Err(e) => {
                if made {
                    let _ = std::fs::remove_dir_all(&path);
                }
                Err(e.into())
            }
        }
    }

    /// Replaces a registered repo's URL, branch, language, mode, limit and
    /// settings with `spec`'s (its slug stays), checked as [`Modes::add`]
    /// checks them; its clone is made or fetched from the new URL first.
    pub async fn update(
        &self,
        store: &Store,
        spec: &RepoSpec,
        token: Option<&str>,
        actor: &Actor,
    ) -> Result<Repo, LifecycleError> {
        let (new, config) = self.check_spec(spec)?;
        if store.repo_by_slug(&new.slug).await?.is_none() {
            return Err(LifecycleError::Config(format!("no repo `{}`", new.slug)));
        }
        crate::clone::prepare_async(
            &config.repo.path,
            &new.url,
            &new.default_branch,
            auth(spec.settings.fork.platform, token),
        )
        .await?;
        Ok(store.update_repo_by(&new, actor).await?)
    }
}

/// The user `token` is sent as for a repo whose fork is on `platform`.
fn auth(platform: crate::ForkPlatform, token: Option<&str>) -> Option<(&'static str, &str)> {
    let user = match platform {
        crate::ForkPlatform::GitHub => aegis_pr::GITHUB_USER,
        crate::ForkPlatform::GitLab => aegis_pr::GITLAB_USER,
    };
    token.map(|t| (user, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    const POC: &str = include_str!("../../../config/poc.toml");
    const JP: &str = include_str!("../../../config/customer-jp.toml");

    fn modes() -> Modes {
        Modes::new(
            Some(ModeConfig::from_toml(POC).unwrap()),
            Some(ModeConfig::from_toml(JP).unwrap()),
        )
        .unwrap()
    }

    fn settings() -> serde_json::Value {
        serde_json::json!({
            "fork": { "owner": "me", "name": "app", "token_env": "GITHUB_TOKEN" }
        })
    }

    fn repo(slug: &str, mode: RepoMode, config: serde_json::Value) -> Repo {
        Repo {
            id: RepoId(1),
            slug: slug.into(),
            url: format!("https://github.com/me/{slug}"),
            default_branch: "main".into(),
            language: "python".into(),
            mode,
            max_concurrent_jobs: 1,
            config,
            created_at: Default::default(),
        }
    }

    fn jp_with(edit: impl FnOnce(&mut ModeConfig)) -> ModeConfig {
        let mut jp = ModeConfig::from_toml(JP).unwrap();
        edit(&mut jp);
        jp
    }

    #[test]
    fn the_shipped_customer_config_passes_the_guard_and_the_poc_one_does_not() {
        let jp = ModeConfig::from_toml(JP).unwrap();
        jp.check(RepoMode::CustomerJp).unwrap();
        let poc = ModeConfig::from_toml(POC).unwrap();
        poc.check(RepoMode::Poc).unwrap();
        let err = poc.check(RepoMode::CustomerJp).unwrap_err().to_string();
        assert!(err.contains("must be Bedrock"), "{err}");
    }

    /// #266: customer repos get the impact note too, with the same server
    /// as the PoC, and the guard still passes.
    #[test]
    fn the_shipped_customer_config_asks_the_code_graph() {
        let jp = ModeConfig::from_toml(JP).unwrap();
        let poc = ModeConfig::from_toml(POC).unwrap();
        assert_eq!(jp.codebase_memory, poc.codebase_memory);
        assert!(jp.codebase_memory.is_some());
        jp.check(RepoMode::CustomerJp).unwrap();
    }

    /// #233: the local mode's $0 cap does not stop a job of free calls
    /// before it starts, and its context fits what Ollama is told to run.
    #[test]
    fn the_shipped_local_config_runs_free_models() {
        let local = ModeConfig::from_toml(include_str!("../../../config/local.toml")).unwrap();
        local.check(RepoMode::Poc).unwrap();
        assert!(!local.budget.limits().is_spent(0.0));
        assert!(local.budget.limits().is_spent(0.01));
        assert_eq!(local.run.context_window, 32_768);
    }

    #[test]
    fn the_guard_refuses_any_way_out_of_japan() {
        type Edit = fn(&mut ModeConfig);
        let cases: [(Edit, &str); 4] = [
            (
                |m| m.models.core.id = "anthropic.claude-sonnet-5-5".into(),
                "of Core",
            ),
            (
                |m| m.models.light.id = "global.anthropic.claude-haiku-5-5".into(),
                "of Light",
            ),
            (
                |m| {
                    m.provider = ProviderConfig::Bedrock {
                        region: JAPAN_REGION.into(),
                        require_jp_profile: false,
                    }
                },
                "require_jp_profile",
            ),
            (
                |m| {
                    m.provider = ProviderConfig::Bedrock {
                        region: "us-east-1".into(),
                        require_jp_profile: true,
                    }
                },
                "us-east-1",
            ),
        ];
        for (edit, why) in cases {
            let jp = jp_with(edit);
            let err = jp.check(RepoMode::CustomerJp).unwrap_err().to_string();
            assert!(err.contains(why), "{why}: {err}");
            // Refused before any repo uses it, and again if set by hand.
            assert!(Modes::new(None, Some(jp.clone())).is_err());
            let by_hand = Modes {
                poc: None,
                customer_jp: Some(jp),
            };
            let customer = repo("customer", RepoMode::CustomerJp, settings());
            assert!(by_hand.run_config(&customer).is_err(), "{why}");
        }
    }

    #[test]
    fn each_repo_runs_with_its_modes_provider_and_its_own_settings() {
        let modes = modes();
        let mut own = settings();
        own["image"] = "registry.local/app-ci:3.12".into();
        own["allowed_packages"] = serde_json::json!(["attrs"]);
        own["licenses"] = serde_json::json!(["MIT"]);
        own["budget"] = serde_json::json!({ "max_cost_usd": 2.0, "max_model_calls": 20 });
        own["conventions"] = "Tests go in tests/.".into();
        own["sonar"] = serde_json::json!({
            "url": "https://sonar.example.jp", "token_env": "SONAR_TOKEN", "project_key": "jp_app"
        });
        let internal = modes
            .run_config(&repo("scrapy", RepoMode::Poc, settings()))
            .unwrap();
        let customer = modes
            .run_config(&repo("app.jp", RepoMode::CustomerJp, own))
            .unwrap();

        assert!(matches!(
            internal.provider,
            ProviderConfig::ClaudeApi { .. }
        ));
        assert_eq!(internal.models.core.id, "claude-sonnet-5-5");
        assert!(internal.sonar.is_none());
        assert!(matches!(
            customer.provider,
            ProviderConfig::Bedrock {
                require_jp_profile: true,
                ..
            }
        ));
        assert!(customer.models.core.id.starts_with("jp."));

        assert_eq!(internal.repo.key, "scrapy");
        assert_eq!(customer.repo.key, "app.jp");
        assert!(customer.repo.path.ends_with("state-jp/repos/app.jp"));
        assert_eq!(customer.repo.base, "main");
        assert_eq!(customer.repo.language, Language::Python);
        let overrides = customer.repo.profile_overrides().unwrap();
        assert_eq!(
            overrides.image.as_deref(),
            Some("registry.local/app-ci:3.12")
        );
        assert_eq!(customer.repo.allowed_package_names, ["attrs"]);
        assert_eq!(
            customer.repo.licenses.as_deref(),
            Some(&["MIT".to_owned()][..])
        );
        assert_eq!(customer.budget.max_cost_usd, 2.0);
        assert_eq!(internal.budget.max_cost_usd, 4.0);
        assert_eq!(customer.sonar.unwrap().project_key, "jp_app");
        assert!(internal.repo.conventions.is_none());
    }

    #[test]
    fn a_repo_without_its_modes_configuration_or_with_bad_settings_is_refused() {
        let only_poc = Modes::new(Some(ModeConfig::from_toml(POC).unwrap()), None).unwrap();
        let err = only_poc
            .run_config(&repo("app", RepoMode::CustomerJp, settings()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no configuration for mode `customer-jp`"),
            "{err}"
        );

        let modes = modes();
        let mut unknown = settings();
        unknown["imag"] = "typo".into();
        let mut no_language = repo("app", RepoMode::Poc, settings());
        no_language.language = "cobol".into();
        let mut bad_override = settings();
        bad_override["overrides"] = serde_json::json!({ "no_such_step": ["true"] });
        let mut plain_jira = settings();
        plain_jira["tracker"] = serde_json::json!({
            "kind": "jira", "url": "http://jira.acme.com", "user": "bot@acme.com",
            "token_env": "AEGIS_JIRA_TOKEN", "jql": "project = APP"
        });
        let mut poll_without_sonar = settings();
        poll_without_sonar["sonar_poll"] = serde_json::json!({ "every_s": 3600 });
        let mut poll_too_often = settings();
        poll_too_often["sonar"] = serde_json::json!({
            "url": "https://sonarcloud.io", "token_env": "SONAR_TOKEN",
            "organization": "acme", "project_key": "acme_app"
        });
        poll_too_often["sonar_poll"] = serde_json::json!({ "every_s": 5 });
        for bad in [
            repo("app", RepoMode::Poc, poll_without_sonar),
            repo("app", RepoMode::Poc, poll_too_often),
            repo("app", RepoMode::Poc, unknown),
            repo("app", RepoMode::Poc, serde_json::json!({})),
            no_language,
            repo("app", RepoMode::Poc, bad_override),
            repo("app", RepoMode::Poc, plain_jira),
        ] {
            let err = modes.run_config(&bad).unwrap_err().to_string();
            assert!(err.contains("repo `app`"), "{err}");
        }
    }

    #[test]
    fn settings_survive_the_round_trip_through_the_config_column() {
        let mut value = settings();
        value["overrides"] = serde_json::json!({ "gate": { "test": ["pytest", "-q"] } });
        value["hidden_test"] = serde_json::json!(["pytest", "{tests}"]);
        value["dirs"] = serde_json::json!([{
            "path": "web", "language": "typescript",
            "overrides": { "gate": { "test": ["npm", "test"] } }
        }]);
        value["tracker"] = serde_json::json!({
            "kind": "backlog", "url": "https://acme.backlog.jp",
            "token_env": "AEGIS_BACKLOG_API_KEY", "project_ids": [12], "pr_status_id": 3
        });
        value["sonar"] = serde_json::json!({
            "url": "https://sonar.acme.com", "token_env": "SONAR_TOKEN", "project_key": "app"
        });
        value["sonar_poll"] = serde_json::json!({ "min_severity": "CRITICAL", "max_warnings": 20 });
        let parsed: RepoSettings = serde_json::from_value(value).unwrap();
        let back: RepoSettings =
            serde_json::from_value(serde_json::to_value(&parsed).unwrap()).unwrap();
        assert_eq!(back, parsed);
        assert!(parsed.tracker.is_some());
        let poll = parsed.sonar_poll.as_ref().unwrap();
        assert_eq!((poll.every_s, poll.max_warnings), (3600, 20));
        let cfg = modes()
            .run_config(&repo(
                "app",
                RepoMode::Poc,
                serde_json::to_value(&parsed).unwrap(),
            ))
            .unwrap();
        assert_eq!(cfg.repo.dirs[0].path, "web");
    }

    #[test]
    fn issue_trust_config_parses_string_or_table() {
        let direct: RepoSettings = serde_json::from_value(serde_json::json!({
            "fork": { "owner": "o", "name": "n", "token_env": "T" },
            "issue_trust": "collaborators"
        }))
        .unwrap();
        assert_eq!(direct.issue_trust_policy(), IssueTrustPolicy::Collaborators);

        let table: RepoSettings = serde_json::from_value(serde_json::json!({
            "fork": { "owner": "o", "name": "n", "token_env": "T" },
            "issue_trust": { "policy": "collaborators" }
        }))
        .unwrap();
        assert_eq!(table.issue_trust_policy(), IssueTrustPolicy::Collaborators);

        let default_val: RepoSettings = serde_json::from_value(serde_json::json!({
            "fork": { "owner": "o", "name": "n", "token_env": "T" }
        }))
        .unwrap();
        assert_eq!(default_val.issue_trust_policy(), IssueTrustPolicy::Any);
    }
}
