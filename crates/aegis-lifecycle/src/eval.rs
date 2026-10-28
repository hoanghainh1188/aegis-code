//! The eval set of `eval/issues/` (#4) as jobs (#98): each issue runs from
//! its own base commit under a fresh run id, and its fix's tests are kept
//! for after the gate.

use std::path::Path;

use aegis_lang::{Language, deepest, in_dir};
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
    /// hidden tests run by the `hidden_test` command of the part of the
    /// repo that holds their files (#199), in that part's directory.
    pub fn job(&self, cfg: &RunConfig, run_id: &str) -> Result<IssueInput, LifecycleError> {
        let (dir, language, template) = self.hidden_part(cfg)?;
        let where_ = match dir {
            "" => format!("the repo root ({language})"),
            dir => format!("`{dir}/` ({language})"),
        };
        if self.language != language {
            return Err(LifecycleError::Config(format!(
                "{} is a {} issue but its hidden tests are in {where_}",
                self.id, self.language
            )));
        }
        let template = template.ok_or_else(|| {
            LifecycleError::Config(format!(
                "{where_} needs a hidden_test command to run an eval set"
            ))
        })?;
        // Named from the repo root or from the part's directory, where
        // the command runs.
        let tests: Vec<String> = self
            .hidden_tests
            .iter()
            .map(|t| in_dir(dir, t).unwrap_or(t).to_owned())
            .collect();
        Ok(IssueInput {
            id: run_id.to_owned(),
            title: self.title.clone(),
            body: self.body.clone(),
            url: self.url.clone(),
            base: Some(self.base_commit.clone()),
            hidden: Some(HiddenTests {
                fix_commit: self.fix_commit.clone(),
                test_files: self.test_files.clone(),
                dir: dir.to_owned(),
                command: hidden_command(template, &tests),
                known_failures: self.hidden_known_failures.clone(),
            }),
            kind: None,
        })
    }

    /// The directory, language and `hidden_test` of the one part of the
    /// repo holding the hidden tests' files; the root when there are none.
    fn hidden_part<'c>(
        &self,
        cfg: &'c RunConfig,
    ) -> Result<(&'c str, Language, Option<&'c Vec<String>>), LifecycleError> {
        let repo = &cfg.repo;
        let dirs = || std::iter::once("").chain(repo.dirs.iter().map(|d| d.path.as_str()));
        let mut held: Vec<usize> = self
            .test_files
            .iter()
            .map(|f| deepest(dirs(), f).unwrap_or(0))
            .collect();
        held.sort_unstable();
        held.dedup();
        match held.as_slice() {
            [] | [0] => Ok(("", repo.language, repo.hidden_test.as_ref())),
            [i] => {
                let d = &repo.dirs[i - 1];
                Ok((d.path.as_str(), d.language, d.hidden_test.as_ref()))
            }
            _ => Err(LifecycleError::Config(format!(
                "{}: the hidden tests' files are in more than one part of the repo",
                self.id
            ))),
        }
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

    fn issue(language: Language, test_files: &[&str], tests: &[&str]) -> EvalIssue {
        EvalIssue {
            id: "x-1".into(),
            language,
            repo: "me/x".into(),
            issue: 1,
            url: "https://github.com/me/x/issues/1".into(),
            title: "t".into(),
            base_commit: "b".repeat(40),
            fix_commit: "f".repeat(40),
            source_files: vec![],
            test_files: test_files.iter().map(|f| (*f).to_owned()).collect(),
            hidden_tests: tests.iter().map(|t| (*t).to_owned()).collect(),
            hidden_known_failures: vec![],
            changed_tests: vec![],
            body: String::new(),
        }
    }

    /// The PoC's Python repo with a Rust part in `rs/` and another Python
    /// one in `tools/`, each with its own hidden test command.
    fn monorepo() -> RunConfig {
        let poc = include_str!("../../../config/poc.toml");
        RunConfig::from_toml(&format!(
            r#"{poc}
[[repo.dirs]]
path = "rs"
language = "rust"
hidden_test = ["cargo", "test", "--", "{{tests_regex}}"]
[[repo.dirs]]
path = "tools"
language = "python"
hidden_test = ["pytest", "{{tests}}"]
"#
        ))
        .unwrap()
    }

    #[test]
    fn hidden_tests_run_in_the_part_that_holds_their_files() {
        let cfg = monorepo();
        let rust = issue(Language::Rust, &["rs/tests/cart.rs"], &["fourth"]);
        let hidden = rust.job(&cfg, "x-1-r1").unwrap().hidden.unwrap();
        assert_eq!(hidden.dir, "rs");
        assert_eq!(hidden.command, ["cargo", "test", "--", "^(fourth)$"]);

        // Tests named from the repo root are named from the part's dir.
        let tools = issue(
            Language::Python,
            &["tools/tests/test_a.py"],
            &["tools/tests/test_a.py::test_x"],
        );
        let hidden = tools.job(&cfg, "x-1-r1").unwrap().hidden.unwrap();
        assert_eq!(hidden.dir, "tools");
        assert_eq!(hidden.command, ["pytest", "tests/test_a.py::test_x"]);

        let root = issue(Language::Python, &["tests/test_b.py"], &["tests/test_b.py"]);
        let hidden = root.job(&cfg, "x-1-r1").unwrap().hidden.unwrap();
        assert_eq!(hidden.dir, "");
        assert_eq!(hidden.command.last().unwrap(), "tests/test_b.py");
    }

    #[test]
    fn hidden_tests_must_be_of_one_part_of_the_issue_s_language() {
        let cfg = monorepo();
        let wrong = issue(Language::Rust, &["tests/test_b.py"], &["t"]);
        let err = wrong.job(&cfg, "x-1-r1").unwrap_err().to_string();
        assert!(err.contains("the repo root (python)"), "{err}");
        let split = issue(
            Language::Python,
            &["tests/test_b.py", "tools/tests/test_a.py"],
            &["t"],
        );
        let err = split.job(&cfg, "x-1-r1").unwrap_err().to_string();
        assert!(err.contains("more than one part"), "{err}");
    }

    #[test]
    fn the_shipped_set_loads() {
        let set =
            load_set(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/issues")).unwrap();
        assert_eq!(set.len(), 52);
        assert_eq!(
            set.iter().filter(|i| i.language == Language::Rust).count(),
            18
        );
        assert_eq!(
            set.iter()
                .filter(|i| i.language == Language::Python)
                .count(),
            14
        );
        assert_eq!(
            set.iter()
                .filter(|i| i.language == Language::TypeScript)
                .count(),
            5
        );
        assert_eq!(set.iter().filter(|i| i.language == Language::Go).count(), 5);
        assert_eq!(
            set.iter().filter(|i| i.language == Language::Java).count(),
            5
        );
        assert_eq!(
            set.iter()
                .filter(|i| i.language == Language::CSharp)
                .count(),
            5
        );
        assert!(
            set.iter()
                .all(|i| i.base_commit.len() == 40 && i.fix_commit.len() == 40)
        );
    }

    /// Every shipped issue went through `scripts/eval/verify.py` (#220):
    /// its hidden tests fail at the base and pass at the fix.
    #[test]
    fn every_shipped_issue_is_verified() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/issues");
        let unverified: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "toml"))
            .filter(|path| {
                let text = std::fs::read_to_string(path).unwrap();
                let table: toml::Table = text.parse().unwrap();
                !table.get("verified").is_some_and(|v| {
                    v.get("base").and_then(|b| b.as_str()) == Some("fail")
                        && v.get("fix").and_then(|f| f.as_str()) == Some("pass")
                })
            })
            .collect();
        assert!(unverified.is_empty(), "not verified: {unverified:?}");
    }
}
