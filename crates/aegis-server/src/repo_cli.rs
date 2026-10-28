//! Registered repos from the server's command line (#234), audited as
//! `cli`:
//!
//! ```text
//! aegis-server --config <file> repo <command>
//!   add <repo-file>      # registers it and clones it
//!   update <repo-file>   # replaces all but its slug; fetches its clone
//!   show <slug>          # prints it as a repo file (JSON)
//!   list
//! ```
//!
//! A repo file is TOML, or JSON when its name ends in `.json`:
//!
//! ```toml
//! slug = "typst"                      # in paths and branch names
//! url = "https://github.com/typst/typst"
//! default_branch = "main"             # the default
//! language = "rust"
//! mode = "poc"                        # or "customer-jp"; poc by default
//! max_concurrent_jobs = 1             # the default
//!
//! [settings]                          # aegis_lifecycle::RepoSettings
//! fork = { owner = "aegis-bot", name = "typst", token_env = "AEGIS_GITHUB_TOKEN" }
//! ```
//!
//! A repo is checked against its mode's configuration (`[modes]`) before
//! anything is written, as a worker will read it. Its clone is made at
//! `<state_dir>/repos/<slug>` of that mode, with the fork's token when
//! the secret store has it (a private repo); workers fetch its main branch
//! before each job.

use std::path::Path;

use aegis_lang::Language;
use aegis_lifecycle::{Modes, RepoSettings, RepoSpec};
use aegis_secrets::Secrets;
use aegis_store::{Actor, Page, Repo, RepoMode, Store};
use serde::{Deserialize, Serialize};

pub const USAGE: &str = "\
aegis-server --config <file> repo <command>, the command one of:
  add <repo-file>
  update <repo-file>
  show <slug>
  list";

/// Whether `first`, the first argument, is this module's command.
pub fn is_command(first: &str) -> bool {
    first == "repo"
}

/// What a repo file holds; `repo show` prints the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoFile {
    pub slug: String,
    pub url: String,
    #[serde(default = "main_branch")]
    pub default_branch: String,
    pub language: Language,
    #[serde(default = "poc")]
    pub mode: String,
    #[serde(default = "one")]
    pub max_concurrent_jobs: u32,
    pub settings: RepoSettings,
}

fn main_branch() -> String {
    "main".into()
}

fn poc() -> String {
    RepoMode::Poc.as_str().into()
}

fn one() -> u32 {
    1
}

impl RepoFile {
    /// Read from `path`: JSON when it ends in `.json`, TOML otherwise.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let json = path.extension().is_some_and(|e| e == "json");
        let file: Self = if json {
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
        } else {
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
        };
        Ok(file)
    }

    pub fn spec(&self) -> Result<RepoSpec, String> {
        let mode = RepoMode::parse(&self.mode)
            .ok_or_else(|| format!("unknown mode `{}`: poc, customer-jp", self.mode))?;
        Ok(RepoSpec {
            slug: self.slug.clone(),
            url: self.url.clone(),
            default_branch: self.default_branch.clone(),
            language: self.language,
            mode,
            max_concurrent_jobs: self.max_concurrent_jobs,
            settings: self.settings.clone(),
        })
    }

    /// `repo` as a file `update` takes back.
    pub fn of(repo: &Repo) -> Result<Self, String> {
        let bad = |e: String| format!("repo `{}`: {e}", repo.slug);
        Ok(Self {
            slug: repo.slug.clone(),
            url: repo.url.clone(),
            default_branch: repo.default_branch.clone(),
            language: serde_json::from_value(serde_json::Value::String(repo.language.clone()))
                .map_err(|e| bad(e.to_string()))?,
            mode: repo.mode.as_str().into(),
            max_concurrent_jobs: repo.max_concurrent_jobs,
            settings: serde_json::from_value(repo.config.clone())
                .map_err(|e| bad(e.to_string()))?,
        })
    }
}

/// The fork's token for cloning a private repo, when the store has it.
async fn read_token(secrets: &Secrets, spec: &RepoSpec) -> Result<Option<String>, String> {
    let name = &spec.settings.fork.token_env;
    Ok(secrets
        .get_or_load(name)
        .await
        .map_err(|e| format!("{name}: {e}"))?
        .map(|t| t.expose().to_owned()))
}

/// Runs one command (`args` after `repo`); what to print.
pub async fn run(
    store: &Store,
    modes: &Modes,
    secrets: &Secrets,
    args: &[String],
) -> Result<String, String> {
    let actor = Actor::cli();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [verb @ ("add" | "update"), file] => {
            let spec = RepoFile::load(Path::new(file))?.spec()?;
            let token = read_token(secrets, &spec).await?;
            let token = token.as_deref();
            let repo = if *verb == "add" {
                modes.add(store, &spec, token, &actor).await
            } else {
                modes.update(store, &spec, token, &actor).await
            }
            .map_err(|e| e.to_string())?;
            let clone = modes
                .run_config(&repo)
                .map_err(|e| e.to_string())?
                .repo
                .path;
            let done = if *verb == "add" { "added" } else { "updated" };
            Ok(format!(
                "repo `{}` {done} (id {}), cloned at {}\n\
                 give people roles with `grant <email> {} maintainer|reviewer|viewer`",
                repo.slug,
                repo.id.0,
                clone.display(),
                repo.slug
            ))
        }
        ["show", slug] => {
            let repo = store
                .repo_by_slug(slug)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no repo `{slug}`"))?;
            serde_json::to_string_pretty(&RepoFile::of(&repo)?).map_err(|e| e.to_string())
        }
        ["list"] => {
            let mut lines = Vec::new();
            let mut page = Some(Page::first(Page::MAX_LIMIT));
            while let Some(p) = page {
                let found = store.list_repos(p).await.map_err(|e| e.to_string())?;
                lines.extend(found.items.iter().map(|r| {
                    format!(
                        "{}\t{}\t{}\t{}\t{}",
                        r.slug,
                        r.mode.as_str(),
                        r.language,
                        r.default_branch,
                        r.url
                    )
                }));
                page = found.next_page();
            }
            if lines.is_empty() {
                Ok("no repos".into())
            } else {
                Ok(lines.join("\n"))
            }
        }
        _ => Err(USAGE.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
slug = "typst"
url = "https://github.com/typst/typst"
language = "rust"

[settings]
fork = { owner = "aegis-bot", name = "typst", token_env = "AEGIS_GITHUB_TOKEN" }
"#;

    #[test]
    fn a_repo_file_has_defaults_and_refuses_unknown_keys() {
        let file: RepoFile = toml::from_str(FILE).unwrap();
        let spec = file.spec().unwrap();
        assert_eq!(spec.default_branch, "main");
        assert_eq!(spec.mode, RepoMode::Poc);
        assert_eq!(spec.max_concurrent_jobs, 1);
        assert_eq!(spec.language, Language::Rust);

        let typo = FILE.replace("language", "lang = \"rust\"\nlanguage");
        assert!(toml::from_str::<RepoFile>(&typo).is_err());
        let in_settings = FILE.replace("[settings]", "[settings]\nimag = \"x\"");
        assert!(toml::from_str::<RepoFile>(&in_settings).is_err());
        let mode = RepoFile {
            mode: "customer".into(),
            ..file
        };
        assert!(mode.spec().unwrap_err().contains("unknown mode"));
    }

    #[test]
    fn what_show_prints_is_a_file_update_takes() {
        let file: RepoFile = toml::from_str(FILE).unwrap();
        let repo = Repo {
            id: aegis_store::RepoId(3),
            slug: file.slug.clone(),
            url: file.url.clone(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::Poc,
            max_concurrent_jobs: 1,
            config: serde_json::to_value(&file.settings).unwrap(),
            created_at: Default::default(),
        };
        let shown = serde_json::to_string_pretty(&RepoFile::of(&repo).unwrap()).unwrap();
        let back: RepoFile = serde_json::from_str(&shown).unwrap();
        assert_eq!(back, file);
    }
}
