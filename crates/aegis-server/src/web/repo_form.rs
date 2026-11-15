//! The form an admin adds or changes a repo with (#330), turned into the
//! [`RepoFile`] `aegis-server repo add|update` reads, so the pages and the
//! command line check a repo the same way. The fork and the conventions
//! have fields; the rest of `[settings]` is TOML. No field takes a secret:
//! the fork's token is named (`token_env`), its value stays in the secret
//! store.

use aegis_lang::Language;
use aegis_lifecycle::RepoSettings;
use serde::Deserialize;

use crate::repo_cli::RepoFile;

/// The fork's keys that have fields of their own.
const FORK_FIELDS: [&str; 4] = ["platform", "owner", "name", "token_env"];
/// The settings that have fields of their own.
const SETTINGS_FIELDS: [&str; 2] = ["fork", "conventions"];
/// The fork's API when its `[fork]` says none.
const GITHUB_API: &str = "https://api.github.com";
/// The ways to connect a repo (ADR 0017 §1): a fork with its token, or the
/// instance's GitHub App.
pub(super) const CONNECTIONS: [&str; 2] = ["token", "app"];

fn token_path() -> String {
    "token".into()
}

/// What the form posts; also what it shows, filled from a repo to change.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoForm {
    #[serde(default)]
    pub slug: String,
    pub url: String,
    pub default_branch: String,
    pub language: String,
    pub mode: String,
    pub max_concurrent_jobs: String,
    /// `token` or `app`.
    #[serde(default = "token_path")]
    pub connection: String,
    #[serde(default)]
    pub fork_platform: String,
    #[serde(default)]
    pub fork_owner: String,
    #[serde(default)]
    pub fork_name: String,
    #[serde(default)]
    pub token_env: String,
    #[serde(default)]
    pub conventions: String,
    /// The rest of `[settings]`, as TOML.
    #[serde(default)]
    pub settings: String,
}

impl RepoForm {
    /// An empty form with the defaults a repo file has.
    pub fn blank() -> Self {
        Self {
            default_branch: "main".into(),
            language: Language::Rust.id().into(),
            mode: "poc".into(),
            max_concurrent_jobs: "1".into(),
            connection: token_path(),
            fork_platform: "github".into(),
            ..Self::default()
        }
    }

    /// The repo file this form says, `slug` the repo's when it is changed
    /// (its slug cannot be).
    pub fn to_file(&self, slug: Option<&str>) -> Result<RepoFile, String> {
        let slug = slug.unwrap_or(self.slug.trim()).to_owned();
        let language: Language =
            serde_json::from_value(serde_json::Value::String(self.language.trim().into()))
                .map_err(|_| format!("unknown language `{}`", self.language.trim()))?;
        let max_concurrent_jobs = self
            .max_concurrent_jobs
            .trim()
            .parse::<u32>()
            .map_err(|_| "jobs at once: a whole number".to_owned())?;
        let mut settings: toml::Table = toml::from_str(&self.settings)
            .map_err(|e| format!("other settings (TOML): {}", e.message()))?;
        if let Some(key) = SETTINGS_FIELDS
            .iter()
            .find(|k| **k != "fork" && settings.contains_key(**k))
        {
            return Err(format!("other settings: set `{key}` in its field"));
        }
        match self.connection.trim() {
            "token" => {
                if settings.contains_key("app") {
                    return Err("other settings: `app` is for the app connection".into());
                }
                let fork = self.fork_table(settings.remove("fork"))?;
                settings.insert("fork".into(), toml::Value::Table(fork));
            }
            "app" => {
                if settings.contains_key("fork") {
                    return Err("other settings: `fork` is for the token connection".into());
                }
                if [&self.fork_owner, &self.fork_name, &self.token_env]
                    .iter()
                    .any(|v| !v.trim().is_empty())
                {
                    return Err("the app connection has no fork: leave its fields empty".into());
                }
                settings
                    .entry("app")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            }
            other => return Err(format!("unknown connection `{other}`")),
        }
        let conventions = self.conventions.trim();
        if !conventions.is_empty() {
            settings.insert(
                "conventions".into(),
                toml::Value::String(conventions.into()),
            );
        }
        let settings: RepoSettings = toml::Value::Table(settings)
            .try_into()
            .map_err(|e: toml::de::Error| format!("settings: {}", e.message()))?;
        Ok(RepoFile {
            slug,
            url: self.url.trim().into(),
            default_branch: self.default_branch.trim().into(),
            language,
            mode: self.mode.trim().into(),
            max_concurrent_jobs,
            // The pages register in the default tenant; `repo add` and
            // `tenant move` place a repo in another (#525).
            tenant: None,
            settings,
        })
    }

    /// The fork's table: the fields', with what other settings add.
    fn fork_table(&self, given: Option<toml::Value>) -> Result<toml::Table, String> {
        let mut fork = match given {
            None => toml::Table::new(),
            Some(toml::Value::Table(t)) => t,
            Some(_) => return Err("other settings: `fork` is a table".into()),
        };
        if let Some(key) = FORK_FIELDS.iter().find(|k| fork.contains_key(**k)) {
            return Err(format!(
                "other settings: set the fork's `{key}` in its field"
            ));
        }
        for (key, value) in [
            ("platform", &self.fork_platform),
            ("owner", &self.fork_owner),
            ("name", &self.fork_name),
            ("token_env", &self.token_env),
        ] {
            fork.insert(key.into(), toml::Value::String(value.trim().into()));
        }
        Ok(fork)
    }

    /// The form that changes `file`'s repo.
    pub fn of(file: &RepoFile) -> Result<Self, String> {
        let mut rest = settings_table(&file.settings)?;
        for key in SETTINGS_FIELDS {
            rest.remove(key);
        }
        let Some(fork) = &file.settings.fork else {
            // The app connection: its table, with `api` if any, stays in
            // the other settings.
            if rest
                .get("app")
                .is_some_and(|a| a.as_table().is_some_and(|t| t.is_empty()))
            {
                rest.remove("app");
            }
            return Ok(Self {
                connection: "app".into(),
                fork_platform: "github".into(),
                fork_owner: String::new(),
                fork_name: String::new(),
                token_env: String::new(),
                ..Self::common(file, rest)?
            });
        };
        let mut fork_rest = toml::Table::new();
        if fork.api != GITHUB_API {
            fork_rest.insert("api".into(), toml::Value::String(fork.api.clone()));
        }
        if let Some(path) = &fork.push_to_local {
            fork_rest.insert(
                "push_to_local".into(),
                toml::Value::String(path.display().to_string()),
            );
        }
        if !fork_rest.is_empty() {
            rest.insert("fork".into(), toml::Value::Table(fork_rest));
        }
        Ok(Self {
            fork_platform: match fork.platform {
                aegis_lifecycle::ForkPlatform::GitHub => "github".into(),
                aegis_lifecycle::ForkPlatform::GitLab => "gitlab".into(),
            },
            fork_owner: fork.owner.clone(),
            fork_name: fork.name.clone(),
            token_env: fork.token_env.clone(),
            ..Self::common(file, rest)?
        })
    }

    /// What both connections fill alike.
    fn common(file: &RepoFile, rest: toml::Table) -> Result<Self, String> {
        Ok(Self {
            slug: file.slug.clone(),
            url: file.url.clone(),
            default_branch: file.default_branch.clone(),
            language: file.language.id().into(),
            mode: file.mode.clone(),
            max_concurrent_jobs: file.max_concurrent_jobs.to_string(),
            connection: if file.settings.app.is_some() {
                "app".into()
            } else {
                token_path()
            },
            fork_platform: String::new(),
            fork_owner: String::new(),
            fork_name: String::new(),
            token_env: String::new(),
            conventions: file.settings.conventions.clone().unwrap_or_default(),
            settings: toml::to_string(&rest).map_err(|e| e.to_string())?,
        })
    }
}

/// `settings` as TOML, without what is empty (it reads back the same).
fn settings_table(settings: &RepoSettings) -> Result<toml::Table, String> {
    let value = toml::Value::try_from(settings).map_err(|e| e.to_string())?;
    let toml::Value::Table(mut table) = value else {
        return Err("settings are not a table".into());
    };
    // `[app]` says the connection even when empty (ADR 0017).
    table.retain(|k, v| match v {
        toml::Value::Table(t) => k == "app" || !t.is_empty(),
        toml::Value::Array(a) => !a.is_empty(),
        _ => true,
    });
    Ok(table)
}

/// `file` as the TOML repo file `aegis-server repo add|update` reads.
pub fn file_toml(file: &RepoFile) -> Result<String, String> {
    let mut table = toml::Table::new();
    table.insert("slug".into(), toml::Value::String(file.slug.clone()));
    table.insert("url".into(), toml::Value::String(file.url.clone()));
    table.insert(
        "default_branch".into(),
        toml::Value::String(file.default_branch.clone()),
    );
    table.insert(
        "language".into(),
        toml::Value::String(file.language.id().into()),
    );
    table.insert("mode".into(), toml::Value::String(file.mode.clone()));
    table.insert(
        "max_concurrent_jobs".into(),
        toml::Value::Integer(i64::from(file.max_concurrent_jobs)),
    );
    table.insert(
        "settings".into(),
        toml::Value::Table(settings_table(&file.settings)?),
    );
    toml::to_string(&table).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> RepoForm {
        RepoForm {
            slug: "demo".into(),
            url: "https://github.com/acme/demo".into(),
            language: "go".into(),
            fork_owner: "aegis-bot".into(),
            fork_name: "demo".into(),
            token_env: "AEGIS_GITHUB_TOKEN".into(),
            ..RepoForm::blank()
        }
    }

    #[test]
    fn a_form_is_a_repo_file_and_back() {
        let mut f = form();
        f.conventions = "Run make check.".into();
        f.settings =
            "allowed_packages = [\"serde\"]\n\n[fork]\napi = \"https://gitlab.example/api/v4\"\n"
                .into();
        f.fork_platform = "gitlab".into();
        let file = f.to_file(None).unwrap();
        assert_eq!(
            file.settings.fork.as_ref().unwrap().token_env,
            "AEGIS_GITHUB_TOKEN"
        );
        assert_eq!(
            file.settings.fork.as_ref().unwrap().api,
            "https://gitlab.example/api/v4"
        );
        assert_eq!(file.settings.allowed_packages, ["serde"]);
        assert_eq!(
            file.settings.conventions.as_deref(),
            Some("Run make check.")
        );
        let back = RepoForm::of(&file).unwrap();
        assert_eq!(back.to_file(None).unwrap(), file);
        assert!(!back.settings.contains("token_env"), "{}", back.settings);
        assert!(file_toml(&file).unwrap().contains("slug = \"demo\""));
    }

    #[test]
    fn the_app_connection_has_no_fork_and_reads_back() {
        let mut f = RepoForm {
            connection: "app".into(),
            fork_owner: String::new(),
            fork_name: String::new(),
            token_env: String::new(),
            ..form()
        };
        let file = f.to_file(None).unwrap();
        assert!(file.settings.fork.is_none());
        assert_eq!(file.settings.app, Some(Default::default()));
        let back = RepoForm::of(&file).unwrap();
        assert_eq!(back.connection, "app");
        assert_eq!(back.to_file(None).unwrap(), file);
        assert!(file_toml(&file).unwrap().contains("[settings.app]"));

        f.settings = "[app]\napi = \"https://ghe.acme.test/api/v3\"\n".into();
        let ghes = f.to_file(None).unwrap();
        assert_eq!(
            ghes.settings.app.as_ref().unwrap().api.as_deref(),
            Some("https://ghe.acme.test/api/v3")
        );
        assert_eq!(RepoForm::of(&ghes).unwrap().to_file(None).unwrap(), ghes);

        f.settings = String::new();
        f.token_env = "AEGIS_GITHUB_TOKEN".into();
        assert!(
            f.to_file(None)
                .unwrap_err()
                .contains("leave its fields empty")
        );
        f.token_env = String::new();
        f.settings = "[fork]\napi = \"x\"\n".into();
        assert!(f.to_file(None).unwrap_err().contains("token connection"));
        let mut token = form();
        token.settings = "[app]\n".into();
        assert!(token.to_file(None).unwrap_err().contains("app connection"));
        token.settings = String::new();
        token.connection = "ssh".into();
        assert!(
            token
                .to_file(None)
                .unwrap_err()
                .contains("unknown connection")
        );
    }

    #[test]
    fn fields_are_not_set_twice_and_unknown_keys_are_refused() {
        let mut f = form();
        f.settings = "[fork]\ntoken_env = \"OTHER\"\n".into();
        assert!(f.to_file(None).unwrap_err().contains("token_env"));
        f.settings = "conventions = \"x\"\n".into();
        assert!(f.to_file(None).unwrap_err().contains("conventions"));
        f.settings = "token = \"ghp_xxx\"\n".into();
        assert!(f.to_file(None).unwrap_err().contains("token"));
        f.settings = String::new();
        f.max_concurrent_jobs = "two".into();
        assert!(f.to_file(None).unwrap_err().contains("whole number"));
    }
}
