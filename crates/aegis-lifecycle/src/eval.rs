//! The eval set of `eval/issues/` (#4) as jobs (#98): each issue runs from
//! its own base commit under a fresh run id, and its fix's tests are kept
//! for after the gate.

use std::path::Path;

use aegis_lang::Language;
use aegis_ledger::{IssueId, LedgerStore};
use serde::Deserialize;

use crate::issue::HiddenTests;
use crate::{IssueInput, LifecycleError, RunConfig};

/// One `eval/issues/<id>.toml`; see `eval/README.md`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EvalIssue {
    pub id: String,
    pub language: Language,
    pub repo: String,
    pub issue: u64,
    pub url: String,
    pub title: String,
    pub base_commit: String,
    pub fix_commit: String,
    /// What the fix changes besides tests; the dry run (#104) writes them.
    #[serde(default)]
    pub source_files: Vec<String>,
    #[serde(default)]
    pub test_files: Vec<String>,
    pub hidden_tests: Vec<String>,
    /// Hidden tests that fail in the sandbox with the upstream fix too,
    /// as the dry run found them (#104).
    #[serde(default)]
    pub hidden_known_failures: Vec<String>,
    /// Existing tests whose reference output the fix changes
    /// (`scripts/eval/changed_tests.py`, #105).
    #[serde(default)]
    pub changed_tests: Vec<String>,
    #[serde(default)]
    pub body: String,
}

/// Every issue file under `dir`, by id. Files that do not parse are an
/// error: a set must not silently lose an issue.
pub fn load_set(dir: &Path) -> Result<Vec<EvalIssue>, LifecycleError> {
    let entries = std::fs::read_dir(dir).map_err(|e| LifecycleError::io(dir, &e))?;
    let mut issues = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| LifecycleError::io(dir, &e))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| LifecycleError::io(&path, &e))?;
        let issue: EvalIssue = toml::from_str(&text)
            .map_err(|e| LifecycleError::Issue(format!("{}: {e}", path.display())))?;
        if issue.hidden_tests.is_empty() {
            return Err(LifecycleError::Issue(format!(
                "{}: no hidden tests",
                path.display()
            )));
        }
        issues.push(issue);
    }
    issues.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(issues)
}

/// `template` with `{tests}` as one argument per test and `{tests_regex}`
/// as `^(a|b)$`, special characters escaped.
pub fn hidden_command(template: &[String], tests: &[String]) -> Vec<String> {
    let escape = |t: &str| {
        t.chars()
            .flat_map(|c| {
                let special = "\\.+*?()|[]{}^$".contains(c);
                special.then_some('\\').into_iter().chain([c])
            })
            .collect::<String>()
    };
    let regex = format!(
        "^({})$",
        tests
            .iter()
            .map(|t| escape(t))
            .collect::<Vec<_>>()
            .join("|")
    );
    template
        .iter()
        .flat_map(|arg| match arg.as_str() {
            "{tests}" => tests.to_vec(),
            "{tests_regex}" => vec![regex.clone()],
            _ => vec![arg.clone()],
        })
        .collect()
}

/// The first `<id>-<mark><n>` the ledger has not seen, so a rerun never
/// reuses a finished record. Runs with a model are `r`, dry runs `dry`
/// (#104), so their job dirs never meet.
pub async fn next_run_id<S: LedgerStore>(
    store: &S,
    id: &str,
    mark: &str,
) -> Result<String, LifecycleError> {
    for n in 1.. {
        let run = format!("{id}-{mark}{n}");
        let issue = IssueId::parse(run.clone())?;
        if store.events(&issue).await?.is_empty() {
            return Ok(run);
        }
    }
    unreachable!("ran out of run numbers")
}

impl EvalIssue {
    /// The job for run `run_id`: the issue's text and base, with its
    /// hidden tests run by the repo's `hidden_test` command.
    pub fn job(&self, cfg: &RunConfig, run_id: &str) -> Result<IssueInput, LifecycleError> {
        if self.language != cfg.repo.language {
            return Err(LifecycleError::Config(format!(
                "{} is a {} issue but the run configuration is for {}",
                self.id, self.language, cfg.repo.language
            )));
        }
        let template = cfg.repo.hidden_test.as_ref().ok_or_else(|| {
            LifecycleError::Config("repo.hidden_test is needed to run an eval set".into())
        })?;
        Ok(IssueInput {
            id: run_id.to_owned(),
            title: self.title.clone(),
            body: self.body.clone(),
            url: self.url.clone(),
            base: Some(self.base_commit.clone()),
            hidden: Some(HiddenTests {
                fix_commit: self.fix_commit.clone(),
                test_files: self.test_files.clone(),
                command: hidden_command(template, &self.hidden_tests),
                known_failures: self.hidden_known_failures.clone(),
            }),
            kind: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_test_commands() {
        let typst: Vec<String> = ["cargo", "test", "--test", "tests", "--", "{tests_regex}"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            hidden_command(&typst, &["math-lr".into(), "a.b".into()])
                .last()
                .unwrap(),
            r"^(math-lr|a\.b)$"
        );
        let pytest: Vec<String> = ["python", "-m", "pytest", "{tests}"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            hidden_command(&pytest, &["tests/a.py".into(), "tests/b.py".into()]),
            ["python", "-m", "pytest", "tests/a.py", "tests/b.py"]
        );
    }

    #[test]
    fn the_shipped_set_loads() {
        let set =
            load_set(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/issues")).unwrap();
        assert_eq!(set.len(), 32);
        assert_eq!(
            set.iter().filter(|i| i.language == Language::Rust).count(),
            18
        );
        assert!(
            set.iter()
                .all(|i| i.base_commit.len() == 40 && i.fix_commit.len() == 40)
        );
    }
}
