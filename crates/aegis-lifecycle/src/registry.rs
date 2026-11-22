//! Many repos (#24): each registered repo (`aegis-store`'s `repos`) runs
//! with the configuration of its mode (`poc`, `customer-jp`, or any other
//! the server's `[modes]` names, #592), and its own
//! settings: language profile and overrides, Docker image, Sonar project,
//! allow-list, license and advisory policy, conventions for the prompt.
//! [`Modes::run_config`] puts the two together into the [`RunConfig`] of
//! one repo's jobs; nothing else is shared between repos.

use std::collections::BTreeMap;
use std::path::Path;

use aegis_lang::Language;
use aegis_ledger::Tier;
use aegis_provider::{ModelPolicy, ProviderConfig, ResidencyConfig};
use aegis_router::ModelsConfig;
use aegis_sonar_batch::PollConfig;
use aegis_store::{Actor, ModeName, NewRepo, Repo, RepoId, Store};
use aegis_tracker::TrackerConfig;
use serde::{Deserialize, Serialize};

use crate::LifecycleError;
use crate::config::{
    BudgetSection, DirConfig, ExtraServer, ForkConfig, IdentityConfig, RepoConfig, RunConfig,
    RunSection, SonarConfig, residency_policy,
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
    /// Where the mode's code may go (#592); see [`ResidencyConfig`].
    #[serde(default)]
    pub residency: ResidencyConfig,
}

fn is_customer_jp(mode: &ModeName) -> bool {
    mode.key() == ModeName::CUSTOMER_JP.key()
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

    /// The residency policy of `mode`'s provider, once the configuration
    /// keeps to it: its `[residency]` (#592) for every mode, and for
    /// `customer-jp` also the guard of design §7 (Bedrock in Tokyo with the
    /// `jp.` check on, a `jp.` inference profile for every tier), so a
    /// repo's code cannot be sent anywhere else by a wrong line.
    pub fn check(&self, mode: &ModeName) -> Result<ModelPolicy, LifecycleError> {
        // A file that asks for the `jp.` check is a Japan configuration
        // whatever the mode is called (`custmer_jp`…): it gets the whole
        // guard, not just the check its provider makes.
        let claims_japan = matches!(
            self.provider,
            ProviderConfig::Bedrock {
                require_jp_profile: true,
                ..
            }
        );
        let customer_jp = is_customer_jp(mode) || claims_japan;
        if customer_jp {
            self.check_japan_only()?;
        }
        let policy = residency_policy(&self.residency, &self.provider, &self.models).map_err(
            |e| match e {
                LifecycleError::Config(why) => LifecycleError::Config(format!("{mode}: {why}")),
                other => other,
            },
        )?;
        Ok(match policy {
            ModelPolicy::Any if customer_jp => ModelPolicy::japan_only(),
            policy => policy,
        })
    }

    fn check_japan_only(&self) -> Result<(), LifecycleError> {
        let refuse = |why: String| {
            Err(LifecycleError::Config(format!(
                "customer-jp guard (design §7): {why}"
            )))
        };
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
            ProviderConfig::ClaudeApi { .. }
            | ProviderConfig::Local { .. }
            | ProviderConfig::OpenAiCompat { .. }
            | ProviderConfig::VertexAi { .. }
            | ProviderConfig::Failover { .. } => {
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
    /// Patch scope restrictions for gate and PR review (#359).
    #[serde(default, skip_serializing_if = "is_default_patch_scope")]
    pub patch_scope: PatchScopeConfig,
    /// Secrets the gate's `secrets` step lets through (#509):
    /// `[[secrets.allow]]` with `path`, `kinds` and `reason`.
    #[serde(default, skip_serializing_if = "is_default_secrets")]
    pub secrets: aegis_gate::SecretPolicy,
    /// Which triaged tickets get a read-only diagnosis written back instead
    /// of a PR (#451): `[triage_lane]` with `categories` and
    /// `min_confidence`; off by default.
    #[serde(default, skip_serializing_if = "crate::TriageLane::is_off")]
    pub triage_lane: crate::TriageLane,
    /// Services the test steps reach (#490): `[[sidecars]]`, as
    /// `[[repo.sidecars]]` of a run configuration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<aegis_gate::SidecarSpec>,
    /// The language of its PRs, review replies, diagnoses and issue
    /// comments (#589); its tenant's `default_locale`, then the server's
    /// `[web] default_locale`, when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<aegis_pr::Locale>,
    /// The token path (ADR 0017 §1): the fork, or the upstream repo itself,
    /// with the secret of its token. Exactly one of `fork` and `app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<ForkConfig>,
    /// The app path: the instance's GitHub App, no secret here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<crate::AppConnection>,
}

impl RepoSettings {
    /// The forge the repo is on: GitLab only through a GitLab fork.
    pub fn platform(&self) -> crate::ForkPlatform {
        self.fork
            .as_ref()
            .map_or(crate::ForkPlatform::GitHub, |f| f.platform)
    }

    /// Where the agent pushes and opens PRs for the repo at `url`, with
    /// the app connection on the app path: exactly one of `fork` and
    /// `app`, the app path only for a GitHub repo's HTTPS URL.
    pub fn connection(
        &self,
        url: &str,
    ) -> Result<(ForkConfig, Option<crate::AppConnection>), String> {
        match (&self.fork, &self.app) {
            (Some(fork), None) => Ok((fork.clone(), None)),
            (None, Some(app)) => {
                let (owner, name) = crate::upstream_of(url).ok_or_else(|| {
                    format!("app: `{url}` is not a GitHub repo's https://<host>/<owner>/<repo> URL")
                })?;
                let fork = ForkConfig {
                    platform: crate::ForkPlatform::GitHub,
                    owner,
                    name,
                    api: app.api.clone().unwrap_or_else(crate::config::default_api),
                    token_env: String::new(),
                    push_to_local: None,
                };
                Ok((fork, Some(app.clone())))
            }
            (Some(_), Some(_)) => Err("name `fork` or `app`, not both".into()),
            (None, None) => Err("needs `fork` (a token) or `app` (the GitHub App)".into()),
        }
    }

    pub fn issue_trust_policy(&self) -> IssueTrustPolicy {
        self.issue_trust.policy()
    }

    pub fn patch_scope_policy(&self) -> &PatchScopeConfig {
        &self.patch_scope
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

/// Patch scope limits and controls (#359).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchScopeConfig {
    /// Maximum number of changed files permitted before warning or failing.
    #[serde(default)]
    pub max_files: Option<usize>,
    /// Maximum number of distinct directories permitted before warning or failing.
    #[serde(default)]
    pub max_dirs: Option<usize>,
    /// Whether deleting test files is allowed (default: false, disallow deletions).
    #[serde(default)]
    pub allow_test_deletion: bool,
    /// Whether adding test skips/ignores is allowed (default: false, disallow skipping tests).
    #[serde(default)]
    pub allow_test_skips: bool,
    /// Whether scope violations fail the gate (`true`) or only warn & flag reviewer (`false`, default).
    #[serde(default)]
    pub enforce: bool,
}

fn is_default_secrets(p: &aegis_gate::SecretPolicy) -> bool {
    p.allow.is_empty()
}

fn is_default_patch_scope(c: &PatchScopeConfig) -> bool {
    *c == PatchScopeConfig::default()
}

/// What registering a repo takes.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoSpec {
    pub slug: String,
    pub url: String,
    pub default_branch: String,
    pub language: Language,
    pub mode: ModeName,
    pub max_concurrent_jobs: u32,
    pub settings: RepoSettings,
    /// The contract it is registered in (#525); `None` is the default
    /// tenant. An update keeps the repo's tenant.
    pub tenant: Option<aegis_store::Tenant>,
}

/// Where the repos of `tenant` live under a mode's `state_dir` (#525):
/// the default tenant's where they always did, another's under
/// `tenants/<id>`, clones, jobs, maps and caches alike.
pub fn tenant_state_dir(
    state_dir: &std::path::Path,
    tenant: aegis_store::TenantId,
) -> std::path::PathBuf {
    if tenant == aegis_store::TenantId::DEFAULT {
        state_dir.to_path_buf()
    } else {
        state_dir.join("tenants").join(tenant.0.to_string())
    }
}

/// The names of the secrets a repo's settings read: every string under a
/// key ending in `_env` (`token_env`, `app_key_env`…).
fn secret_names(config: &serde_json::Value, out: &mut Vec<String>) {
    match config {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                match value {
                    serde_json::Value::String(name) if key.ends_with("_env") => {
                        out.push(name.clone());
                    }
                    other => secret_names(other, out),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| secret_names(v, out)),
        _ => {}
    }
}

/// A repo of a tenant other than the default reads only secrets named for
/// that tenant (#525): `ACME_FORK_TOKEN` for `acme`, so no contract can
/// name another's.
pub fn check_tenant_secrets(
    tenant: &aegis_store::Tenant,
    config: &serde_json::Value,
) -> Result<(), LifecycleError> {
    let Some(prefix) = tenant.secret_prefix() else {
        return Ok(());
    };
    let mut names = Vec::new();
    secret_names(config, &mut names);
    match names.iter().find(|n| !n.starts_with(&prefix)) {
        Some(name) => Err(LifecycleError::Config(format!(
            "secret `{name}`: a repo of tenant `{}` reads only secrets whose names start with \
             `{prefix}`",
            tenant.slug
        ))),
        None => Ok(()),
    }
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

/// The configuration of each mode, by name (#592). A mode without one has
/// no repos. Each was checked ([`ModeConfig::check`]) when it was added.
#[derive(Debug, Clone, Default)]
pub struct Modes {
    /// By [`ModeName::key`]: the name as `[modes]` gave it, its
    /// configuration.
    configs: BTreeMap<String, (ModeName, ModeConfig)>,
    /// Whether the instance has a GitHub App (`[github_app]`, ADR 0017):
    /// without one, a repo on the app path is refused at registration.
    pub github_app: bool,
    /// What a repo writes in when neither it nor its tenant names a
    /// language: the server's `[web] default_locale` (#589).
    pub default_locale: aegis_pr::Locale,
}

impl Modes {
    /// The two modes every deployment knows, either one optional.
    pub fn new(
        poc: Option<ModeConfig>,
        customer_jp: Option<ModeConfig>,
    ) -> Result<Self, LifecycleError> {
        Self::from_configs(
            [(ModeName::POC, poc), (ModeName::CUSTOMER_JP, customer_jp)]
                .into_iter()
                .filter_map(|(name, config)| config.map(|c| (name, c))),
        )
    }

    /// Any modes, by name. Fails if one does not pass its check, before
    /// any repo uses it, or if two names are the same mode (`a_b` and
    /// `a-b`).
    pub fn from_configs(
        configs: impl IntoIterator<Item = (ModeName, ModeConfig)>,
    ) -> Result<Self, LifecycleError> {
        let mut by_key = BTreeMap::new();
        for (name, config) in configs {
            config.check(&name)?;
            let key = name.key();
            if let Some((first, _)) = by_key.get(&key) {
                return Err(LifecycleError::Config(format!(
                    "modes `{first}` and `{name}` are the same mode: `_` and `-` are not told apart"
                )));
            }
            by_key.insert(key, (name, config));
        }
        Ok(Self {
            configs: by_key,
            github_app: false,
            default_locale: aegis_pr::Locale::default(),
        })
    }

    /// The same modes, on an instance that has a GitHub App or not.
    pub fn with_github_app(self, github_app: bool) -> Self {
        Self { github_app, ..self }
    }

    /// The same modes, on an instance whose `[web] default_locale` is
    /// `locale`.
    pub fn with_default_locale(self, locale: aegis_pr::Locale) -> Self {
        Self {
            default_locale: locale,
            ..self
        }
    }

    /// The configuration of `mode`, if the server has one.
    pub fn config(&self, mode: &ModeName) -> Option<&ModeConfig> {
        self.configs.get(&mode.key()).map(|(_, config)| config)
    }

    /// Each mode with its configuration, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&ModeName, &ModeConfig)> {
        self.configs.values().map(|(name, config)| (name, config))
    }

    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    fn mode(&self, mode: &ModeName) -> Result<&ModeConfig, LifecycleError> {
        self.config(mode)
            .ok_or_else(|| LifecycleError::Config(format!("no configuration for mode `{mode}`")))
    }

    /// The configuration of `repo`'s jobs. Its clone is
    /// `<state_dir>/repos/<slug>`; module maps, caches and the
    /// Codebase-Memory index are under its slug.
    pub fn run_config(&self, repo: &Repo) -> Result<RunConfig, LifecycleError> {
        self.run_config_in(repo, None)
    }

    /// [`Self::run_config`] for a repo of `tenant`, whose
    /// `default_locale` stands in for a locale the repo does not name.
    pub fn run_config_in(
        &self,
        repo: &Repo,
        tenant: Option<&aegis_store::Tenant>,
    ) -> Result<RunConfig, LifecycleError> {
        self.build(
            RepoRow {
                slug: &repo.slug,
                url: &repo.url,
                default_branch: &repo.default_branch,
                language: &repo.language,
                mode: &repo.mode,
                config: &repo.config,
            },
            repo.tenant_id,
            tenant.and_then(|t| t.default_locale.as_deref()),
        )
    }

    fn build(
        &self,
        row: RepoRow<'_>,
        tenant: aegis_store::TenantId,
        tenant_locale: Option<&str>,
    ) -> Result<RunConfig, LifecycleError> {
        let RepoRow {
            slug,
            url,
            default_branch,
            language,
            mode,
            config,
        } = row;
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
        let (fork, app) = settings.connection(url).map_err(in_repo)?;
        let mut overrides = settings.overrides;
        if let Some(image) = settings.image {
            overrides.insert("image".into(), toml::Value::String(image));
        }
        let state_dir = tenant_state_dir(&mode.run.state_dir, tenant);
        let path = state_dir.join("repos").join(slug);
        RunConfig {
            provider: mode.provider.clone(),
            models: mode.models.clone(),
            run: crate::config::RunSection {
                state_dir,
                ..mode.run.clone()
            },
            // A repo budget without a call cap keeps its mode's (#471).
            budget: settings.budget.map_or(mode.budget, |own| BudgetSection {
                max_model_calls: own.max_model_calls.or(mode.budget.max_model_calls),
                ..own
            }),
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
                patch_scope: Some(settings.patch_scope),
                secrets: settings.secrets,
                triage_lane: settings.triage_lane,
                sidecars: settings.sidecars,
                locale: crate::resolve_locale(settings.locale, tenant_locale, self.default_locale)
                    .map_err(in_repo)?,
                fork,
                app,
            },
            identity: mode.identity.clone(),
            codebase_memory: mode.codebase_memory.clone(),
            sonar: settings.sonar,
            residency: mode.residency.clone(),
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
        let tenant = store.tenant(repo.tenant_id).await?;
        let config = self.run_config_in(&repo, tenant.as_ref())?;
        Ok(RepoRun { repo, config })
    }

    /// The row `spec` is stored as and the configuration its jobs will
    /// run with, built as it will be read back: refused unless it builds
    /// now (a `customer-jp` repo needs a `customer-jp` mode that passes
    /// the guard).
    pub fn check_spec(&self, spec: &RepoSpec) -> Result<(NewRepo, RunConfig), LifecycleError> {
        if spec.settings.app.is_some() && !self.github_app {
            return Err(LifecycleError::Config(format!(
                "repo `{}`: `app` needs the instance's GitHub App, `[github_app]` in server.toml",
                spec.slug
            )));
        }
        let new = NewRepo {
            slug: spec.slug.clone(),
            url: spec.url.clone(),
            default_branch: spec.default_branch.clone(),
            language: spec.language.id().to_owned(),
            mode: spec.mode.clone(),
            max_concurrent_jobs: spec.max_concurrent_jobs,
            config: serde_json::to_value(&spec.settings)
                .map_err(|e| LifecycleError::Config(format!("repo `{}`: {e}", spec.slug)))?,
        };
        let tenant = spec.tenant.as_ref();
        if let Some(t) = tenant {
            check_tenant_secrets(t, &new.config)?;
        }
        let config = self.build(
            RepoRow {
                slug: &new.slug,
                url: &new.url,
                default_branch: &new.default_branch,
                language: &new.language,
                mode: &new.mode,
                config: &new.config,
            },
            tenant.map_or(aegis_store::TenantId::DEFAULT, |t| t.id),
            tenant.and_then(|t| t.default_locale.as_deref()),
        )?;
        Ok((new, config))
    }

    /// `spec` with the tenant its registered repo is in: an update keeps
    /// it. `Config` when there is no such repo.
    async fn with_its_tenant(
        &self,
        store: &Store,
        spec: &RepoSpec,
    ) -> Result<RepoSpec, LifecycleError> {
        let repo = store
            .repo_by_slug(&spec.slug)
            .await?
            .ok_or_else(|| LifecycleError::Config(format!("no repo `{}`", spec.slug)))?;
        let tenant = store.tenant(repo.tenant_id).await?;
        Ok(RepoSpec {
            tenant,
            ..spec.clone()
        })
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
        Ok(store
            .create_repo_in_by(&new, tenant_of(spec), actor)
            .await?)
    }

    /// [`Modes::update`] without touching the clone, audited as `actor`;
    /// [`Modes::clone_repo`] fetches it after (#330).
    pub async fn update_by(
        &self,
        store: &Store,
        spec: &RepoSpec,
        actor: &Actor,
    ) -> Result<Repo, LifecycleError> {
        let spec = &self.with_its_tenant(store, spec).await?;
        let (new, _) = self.check_spec(spec)?;
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
            auth(spec.settings.platform(), token),
        )
        .await?;
        match store.create_repo_in_by(&new, tenant_of(spec), actor).await {
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
        let spec = &self.with_its_tenant(store, spec).await?;
        let (new, config) = self.check_spec(spec)?;
        crate::clone::prepare_async(
            &config.repo.path,
            &new.url,
            &new.default_branch,
            auth(spec.settings.platform(), token),
        )
        .await?;
        Ok(store.update_repo_by(&new, actor).await?)
    }
}

fn tenant_of(spec: &RepoSpec) -> aegis_store::TenantId {
    spec.tenant
        .as_ref()
        .map_or(aegis_store::TenantId::DEFAULT, |t| t.id)
}

/// The user `token` is sent as for a repo whose fork is on `platform`.
fn auth(platform: crate::ForkPlatform, token: Option<&str>) -> Option<(&'static str, &str)> {
    let user = match platform {
        crate::ForkPlatform::GitHub => aegis_pr::GITHUB_USER,
        crate::ForkPlatform::GitLab => aegis_pr::GITLAB_USER,
    };
    token.map(|t| (user, t))
}

/// The stored columns of a repo, registered or about to be, that its
/// jobs' configuration is built from.
#[derive(Clone, Copy)]
struct RepoRow<'a> {
    slug: &'a str,
    url: &'a str,
    default_branch: &'a str,
    language: &'a str,
    mode: &'a ModeName,
    config: &'a serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(id: i64, slug: &str) -> aegis_store::Tenant {
        aegis_store::Tenant {
            id: aegis_store::TenantId(id),
            slug: slug.into(),
            name: slug.into(),
            monthly_budget_usd: None,
            max_concurrent_jobs: None,
            default_locale: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn another_tenants_repos_live_under_its_own_dir() {
        let root = std::path::Path::new("/state");
        assert_eq!(tenant_state_dir(root, aegis_store::TenantId::DEFAULT), root);
        assert_eq!(
            tenant_state_dir(root, aegis_store::TenantId(7)),
            std::path::Path::new("/state/tenants/7")
        );
    }

    #[test]
    fn a_tenants_repo_names_only_its_own_secrets() {
        let config = serde_json::json!({
            "fork": {"owner": "bot", "name": "r", "token_env": "ACME_FORK_TOKEN"},
            "tracker": {"kind": "jira", "token_env": "ACME_JIRA"},
            "apm": {"datadog": {"token_env": "ACME_DD", "app_key_env": "ACME_DD_APP"}},
            "sidecars": [{"name": "pg", "env": {"POSTGRES_PASSWORD": "x"}}],
        });
        assert!(check_tenant_secrets(&tenant(2, "acme"), &config).is_ok());
        assert!(check_tenant_secrets(&tenant(1, "default"), &config).is_ok());
        let err = check_tenant_secrets(&tenant(3, "globex"), &config)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`GLOBEX_`") && err.contains("ACME_"), "{err}");
        let leaky = serde_json::json!({"apm": {"datadog": {"token_env": "ACME_DD", "app_key_env": "AEGIS_DD_APP"}}});
        assert!(
            check_tenant_secrets(&tenant(2, "acme"), &leaky).is_err(),
            "every name"
        );
    }

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

    #[test]
    fn a_repo_on_the_app_path_pushes_to_itself_with_no_secret() {
        let shop = repo("shop", ModeName::POC, serde_json::json!({"app": {}}));
        let config = modes().run_config(&shop).unwrap();
        let fork = &config.repo.fork;
        assert_eq!((fork.owner.as_str(), fork.name.as_str()), ("me", "shop"));
        assert_eq!(fork.api, "https://api.github.com");
        assert!(fork.token_env.is_empty() && fork.push_to_local.is_none());
        assert_eq!(config.repo.app, Some(crate::AppConnection::default()));

        let ghes = repo(
            "shop",
            ModeName::POC,
            serde_json::json!({"app": {"api": "https://ghe.acme.test/api/v3"}}),
        );
        let config = modes().run_config(&ghes).unwrap();
        assert_eq!(config.repo.fork.api, "https://ghe.acme.test/api/v3");

        // The token path is as it was.
        let token = modes()
            .run_config(&repo("app", ModeName::POC, settings()))
            .unwrap();
        assert_eq!(token.repo.fork.token_env, "GITHUB_TOKEN");
        assert_eq!(token.repo.app, None);
    }

    #[test]
    fn a_repo_names_fork_or_app_exactly_once() {
        let both = serde_json::json!({
            "app": {},
            "fork": { "owner": "me", "name": "app", "token_env": "T" }
        });
        for (config, says) in [(both, "not both"), (serde_json::json!({}), "needs `fork`")] {
            let err = modes()
                .run_config(&repo("app", ModeName::POC, config))
                .unwrap_err()
                .to_string();
            assert!(err.contains(says), "{err}");
        }
        let mut gitlab = repo("app", ModeName::POC, serde_json::json!({"app": {}}));
        gitlab.url = "https://gitlab.example/group/sub/app".into();
        let err = modes().run_config(&gitlab).unwrap_err().to_string();
        assert!(err.contains("not a GitHub repo"), "{err}");
    }

    #[test]
    fn the_app_path_is_refused_on_an_instance_without_an_app() {
        let spec = |settings: serde_json::Value| RepoSpec {
            slug: "shop".into(),
            url: "https://github.com/me/shop".into(),
            default_branch: "main".into(),
            language: Language::Python,
            mode: ModeName::POC,
            max_concurrent_jobs: 1,
            settings: serde_json::from_value(settings).unwrap(),
            tenant: None,
        };
        let on_app = spec(serde_json::json!({"app": {}}));
        let err = modes().check_spec(&on_app).unwrap_err().to_string();
        assert!(err.contains("[github_app]"), "{err}");
        assert!(modes().with_github_app(true).check_spec(&on_app).is_ok());
        assert!(modes().check_spec(&spec(settings())).is_ok());
    }

    #[test]
    fn an_upstream_is_read_from_a_plain_https_url_only() {
        let got = |u: &str| crate::upstream_of(u);
        assert_eq!(
            got("https://github.com/acme/shop.git"),
            Some(("acme".into(), "shop".into()))
        );
        assert_eq!(
            got("https://ghe.acme.test/acme/shop/"),
            Some(("acme".into(), "shop".into()))
        );
        for bad in [
            "http://github.com/acme/shop",
            "git@github.com:acme/shop.git",
            "https://github.com/acme",
            "https://github.com/acme/shop/tree/main",
            "https://github.com/../shop",
        ] {
            assert_eq!(got(bad), None, "{bad}");
        }
    }

    fn repo(slug: &str, mode: ModeName, config: serde_json::Value) -> Repo {
        Repo {
            id: RepoId(1),
            slug: slug.into(),
            url: format!("https://github.com/me/{slug}"),
            default_branch: "main".into(),
            language: "python".into(),
            mode,
            max_concurrent_jobs: 1,
            config,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            tenant_id: aegis_store::TenantId::DEFAULT,
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
        jp.check(&ModeName::CUSTOMER_JP).unwrap();
        let poc = ModeConfig::from_toml(POC).unwrap();
        poc.check(&ModeName::POC).unwrap();
        let err = poc.check(&ModeName::CUSTOMER_JP).unwrap_err().to_string();
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
        jp.check(&ModeName::CUSTOMER_JP).unwrap();
    }

    /// #233: the local mode's $0 cap does not stop a job of free calls
    /// before it starts, and its context fits what Ollama is told to run.
    #[test]
    fn the_shipped_local_config_runs_free_models() {
        let local = ModeConfig::from_toml(include_str!("../../../config/local.toml")).unwrap();
        local.check(&ModeName::POC).unwrap();
        assert!(!local.budget.cost_spent(0.0));
        assert!(local.budget.cost_spent(0.01));
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
            let err = jp.check(&ModeName::CUSTOMER_JP).unwrap_err().to_string();
            assert!(err.contains(why), "{why}: {err}");
            // Refused before any repo uses it, under either spelling.
            assert!(Modes::new(None, Some(jp.clone())).is_err());
            let underscore = ModeName::parse("customer_jp").unwrap();
            assert!(Modes::from_configs([(underscore, jp)]).is_err(), "{why}");
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
            .run_config(&repo("scrapy", ModeName::POC, settings()))
            .unwrap();
        let customer = modes
            .run_config(&repo("app.jp", ModeName::CUSTOMER_JP, own))
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

    /// #471: a repo budget that sets only the cost keeps its mode's call
    /// cap; with none in either, the language profile's applies later.
    #[test]
    fn a_repo_budget_without_a_call_cap_keeps_its_modes() {
        let capped = POC.replacen("[budget]\n", "[budget]\nmax_model_calls = 25\n", 1);
        let capped = Modes::new(Some(ModeConfig::from_toml(&capped).unwrap()), None).unwrap();
        let mut own = settings();
        own["budget"] = serde_json::json!({ "max_cost_usd": 2.0 });
        let cfg = capped
            .run_config(&repo("app", ModeName::POC, own.clone()))
            .unwrap();
        assert_eq!(cfg.budget.max_cost_usd, 2.0);
        assert_eq!(cfg.budget.max_model_calls, Some(25));

        let cfg = modes()
            .run_config(&repo("app", ModeName::POC, own))
            .unwrap();
        assert_eq!(cfg.budget.max_model_calls, None);
    }

    /// A mode `customer-eu` on Bedrock in Frankfurt, its models `prefix`.
    fn eu_mode(prefix: &str) -> ModeConfig {
        let eu = JP
            .replace(
                "allowed_model_prefixes = [\"jp.\"]",
                "allowed_model_prefixes = [\"eu.\"]",
            )
            .replace("ap-northeast-1", "eu-central-1")
            .replace("require_jp_profile = true", "require_jp_profile = false")
            .replace("\"jp.", &format!("\"{prefix}"));
        ModeConfig::from_toml(&eu).unwrap()
    }

    fn airgap_mode() -> ModeConfig {
        let local = include_str!("../../../config/local.toml");
        ModeConfig::from_toml(&format!("{local}\n[residency]\nlocal_only = true\n")).unwrap()
    }

    /// #592: a mode only the server's configuration names runs its
    /// repos, with its own residency policy.
    #[test]
    fn modes_of_any_name_run_their_repos_within_their_residency() {
        let eu = ModeName::parse("customer-eu").unwrap();
        let airgap = ModeName::parse("local-airgap").unwrap();
        let modes = Modes::from_configs([
            (ModeName::POC, ModeConfig::from_toml(POC).unwrap()),
            (eu.clone(), eu_mode("eu.")),
            (airgap.clone(), airgap_mode()),
        ])
        .unwrap();
        let names: Vec<&str> = modes.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["customer-eu", "local-airgap", "poc"]);

        let config = modes
            .run_config(&repo("shop.eu", eu.clone(), settings()))
            .unwrap();
        assert!(config.models.core.id.starts_with("eu."));
        assert_eq!(
            config.model_policy().unwrap(),
            ModelPolicy::AllowedPrefixes(vec!["eu.".into()])
        );
        let config = modes
            .run_config(&repo("vault", airgap.clone(), settings()))
            .unwrap();
        assert!(matches!(config.provider, ProviderConfig::Local { .. }));
        assert_eq!(config.model_policy().unwrap(), ModelPolicy::LocalOnly);
        assert_eq!(
            modes.config(&eu).unwrap().check(&eu).unwrap(),
            ModelPolicy::AllowedPrefixes(vec!["eu.".into()])
        );
        assert_eq!(
            modes
                .config(&ModeName::POC)
                .unwrap()
                .check(&ModeName::POC)
                .unwrap(),
            ModelPolicy::Any
        );
    }

    /// #592: a mode whose models leave its prefixes, or whose provider is
    /// not local under `local_only`, stops the start.
    #[test]
    fn a_mode_that_breaks_its_residency_is_refused_before_any_repo_uses_it() {
        let eu = ModeName::parse("customer-eu").unwrap();
        let err = Modes::from_configs([(eu.clone(), eu_mode("global."))])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("customer-eu") && err.contains("`eu.`"),
            "{err}"
        );

        let cloud = format!("{POC}\n[residency]\nlocal_only = true\n");
        let airgap = ModeName::parse("local-airgap").unwrap();
        let err = Modes::from_configs([(airgap, ModeConfig::from_toml(&cloud).unwrap())])
            .unwrap_err()
            .to_string();
        assert!(err.contains("local_only"), "{err}");
    }

    /// A Japan configuration under another name (a typo of
    /// `customer_jp`) keeps the whole guard.
    #[test]
    fn a_japan_file_under_any_name_keeps_the_guard() {
        let typo = ModeName::parse("custmer_jp").unwrap();
        let tokyo = ModeConfig::from_toml(JP).unwrap();
        assert_eq!(tokyo.check(&typo).unwrap(), ModelPolicy::japan_only());
        let virginia = jp_with(|m| {
            m.provider = ProviderConfig::Bedrock {
                region: "us-east-1".into(),
                require_jp_profile: true,
            }
        });
        let err = virginia.check(&typo).unwrap_err().to_string();
        assert!(err.contains("us-east-1"), "{err}");
    }

    #[test]
    fn underscores_and_dashes_name_the_same_mode() {
        let jp = ModeConfig::from_toml(JP).unwrap();
        let underscore = ModeName::parse("customer_jp").unwrap();
        let modes = Modes::from_configs([(underscore.clone(), jp.clone())]).unwrap();
        assert!(modes.config(&ModeName::CUSTOMER_JP).is_some());
        assert!(modes.config(&underscore).is_some());
        // The customer-jp guard holds under either name.
        assert_eq!(jp.check(&underscore).unwrap(), ModelPolicy::japan_only());
        let err = Modes::from_configs([(underscore, jp.clone()), (ModeName::CUSTOMER_JP, jp)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("same mode"), "{err}");
    }

    #[test]
    fn a_repo_without_its_modes_configuration_or_with_bad_settings_is_refused() {
        let only_poc = Modes::new(Some(ModeConfig::from_toml(POC).unwrap()), None).unwrap();
        let err = only_poc
            .run_config(&repo("app", ModeName::CUSTOMER_JP, settings()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no configuration for mode `customer-jp`"),
            "{err}"
        );
        // A mode the store takes but the server has no configuration for
        // (#591).
        let eu = ModeName::parse("customer-eu").unwrap();
        let err = modes()
            .run_config(&repo("app", eu.clone(), settings()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no configuration for mode `customer-eu`"),
            "{err}"
        );
        assert!(modes().config(&eu).is_none());
        assert!(modes().config(&ModeName::POC).is_some());

        let modes = modes();
        let mut unknown = settings();
        unknown["imag"] = "typo".into();
        let mut no_language = repo("app", ModeName::POC, settings());
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
            repo("app", ModeName::POC, poll_without_sonar),
            repo("app", ModeName::POC, poll_too_often),
            repo("app", ModeName::POC, unknown),
            repo("app", ModeName::POC, serde_json::json!({})),
            no_language,
            repo("app", ModeName::POC, bad_override),
            repo("app", ModeName::POC, plain_jira),
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
                ModeName::POC,
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

    #[test]
    fn patch_scope_config_parses_table_and_defaults() {
        let custom: RepoSettings = serde_json::from_value(serde_json::json!({
            "fork": { "owner": "o", "name": "n", "token_env": "T" },
            "patch_scope": {
                "max_files": 15,
                "max_dirs": 3,
                "allow_test_deletion": true,
                "allow_test_skips": false,
                "enforce": true
            }
        }))
        .unwrap();
        assert_eq!(
            custom.patch_scope_policy(),
            &PatchScopeConfig {
                max_files: Some(15),
                max_dirs: Some(3),
                allow_test_deletion: true,
                allow_test_skips: false,
                enforce: true,
            }
        );

        let default_val: RepoSettings = serde_json::from_value(serde_json::json!({
            "fork": { "owner": "o", "name": "n", "token_env": "T" }
        }))
        .unwrap();
        assert_eq!(
            default_val.patch_scope_policy(),
            &PatchScopeConfig::default()
        );
    }
}
