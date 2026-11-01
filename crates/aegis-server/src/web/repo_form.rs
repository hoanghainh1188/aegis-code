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
    pub fork_platform: String,
    pub fork_owner: String,
    pub fork_name: String,
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
        let mut fork = match settings.remove("fork") {
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
        settings.insert("fork".into(), toml::Value::Table(fork));
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
            settings,
        })
    }

    /// The form that changes `file`'s repo.
    pub fn of(file: &RepoFile) -> Result<Self, String> {
        let fork = &file.settings.fork;
        let mut rest = settings_table(&file.settings)?;
        for key in SETTINGS_FIELDS {
            rest.remove(key);
        }
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
            slug: file.slug.clone(),
            url: file.url.clone(),
            default_branch: file.default_branch.clone(),
            language: file.language.id().into(),
            mode: file.mode.clone(),
            max_concurrent_jobs: file.max_concurrent_jobs.to_string(),
            fork_platform: match fork.platform {
                aegis_lifecycle::ForkPlatform::GitHub => "github".into(),
                aegis_lifecycle::ForkPlatform::GitLab => "gitlab".into(),
            },
            fork_owner: fork.owner.clone(),
            fork_name: fork.name.clone(),
            token_env: fork.token_env.clone(),
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
    table.retain(|_, v| match v {
        toml::Value::Table(t) => !t.is_empty(),
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
        assert_eq!(file.settings.fork.token_env, "AEGIS_GITHUB_TOKEN");
        assert_eq!(file.settings.fork.api, "https://gitlab.example/api/v4");
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
