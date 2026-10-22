//! Per-repo overrides on top of a profile (ADR 0004 §1). The repo's own CI
//! commands win; a step the repo does not have is skipped and reported,
//! never invented.

use serde::Deserialize;

use crate::profile::{check_audit, check_gate, check_image, check_repo_path};
use crate::{
    CommandTemplate, Coverage, GateSteps, LangError, Language, Placeholder, Profile, TestOutput,
};

/// A command from the repo, or `false` to turn the step off.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum StepOverride {
    Run(CommandTemplate),
    Enabled(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateOverrides {
    pub format: Option<StepOverride>,
    pub lint: Option<StepOverride>,
    pub typecheck: Option<StepOverride>,
    pub repro_test: Option<StepOverride>,
    pub test: Option<StepOverride>,
    pub coverage: Option<StepOverride>,
    /// Output format of an overridden `test` command.
    pub test_output: Option<TestOutput>,
    /// A test binary with its own harness (typst's `tests`): a regex whose
    /// one group is a failing test's name, read on top of `test_output`.
    pub test_failure: Option<String>,
    /// The line such a harness ends with, so its binary counts as finished
    /// without libtest's `test result:`. Needs `test_failure`.
    pub test_finished: Option<String>,
}

/// How a test binary with its own harness reports (#104), checked when the
/// profile is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomHarness {
    pub failure: String,
    pub finished: Option<String>,
}

/// The language part of `RepoConfig` (#16, #24).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOverrides {
    #[serde(default)]
    pub gate: GateOverrides,
    pub edit_format: Option<StepOverride>,
    pub fetch: Option<StepOverride>,
    /// Hosts added to the profile's `fetch_hosts`, e.g. `github.com` for a
    /// repo with git dependencies.
    #[serde(default)]
    pub fetch_hosts: Vec<String>,
    pub audit: Option<StepOverride>,
    /// Required when the coverage command is overridden.
    pub coverage: Option<Coverage>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    NotInProfile,
    DisabledByRepo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Run(CommandTemplate),
    Skipped(SkipReason),
}

impl Step {
    pub fn command(&self) -> Option<&CommandTemplate> {
        match self {
            Self::Run(c) => Some(c),
            Self::Skipped(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatePlan {
    pub format: Step,
    pub lint: Step,
    pub typecheck: Step,
    pub repro_test: Step,
    /// Never skipped: a gate without tests proves nothing.
    pub test: CommandTemplate,
    pub coverage: Step,
    pub test_output: Option<TestOutput>,
    pub harness: Option<CustomHarness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub profile: Profile,
    pub gate: GatePlan,
    pub edit_format: Step,
    pub fetch: Step,
    /// The profile's `fetch_hosts` and the repo's, in that order.
    pub fetch_hosts: Vec<String>,
    pub audit: Step,
    pub coverage: Option<Coverage>,
    pub image: String,
}

impl Profile {
    pub fn resolve(&self, overrides: &ProfileOverrides) -> Result<ResolvedProfile, LangError> {
        let invalid = |message: String| LangError::InvalidOverrides {
            language: self.language,
            message,
        };
        let o = &overrides.gate;
        let gate = GatePlan {
            format: step(&self.gate.format, &o.format).map_err(invalid)?,
            lint: step(&self.gate.lint, &o.lint).map_err(invalid)?,
            typecheck: step(&self.gate.typecheck, &o.typecheck).map_err(invalid)?,
            repro_test: step(&self.gate.repro_test, &o.repro_test).map_err(invalid)?,
            test: match step(&Some(self.gate.test.clone()), &o.test).map_err(invalid)? {
                Step::Run(c) => c,
                Step::Skipped(_) => return Err(invalid("gate.test cannot be turned off".into())),
            },
            coverage: step(&self.gate.coverage, &o.coverage).map_err(invalid)?,
            // A repo's own test command may print differently: its format
            // is used only when the repo names it.
            test_output: match &o.test {
                Some(StepOverride::Run(_)) => o.test_output,
                _ => o.test_output.or(self.gate.test_output),
            },
            harness: harness(o).map_err(invalid)?,
        };
        if gate.harness.is_some() && gate.test_output.is_none() {
            return Err(invalid(
                "gate.test_failure reads on top of a test_output".into(),
            ));
        }
        check_gate(&GateSteps {
            format: gate.format.command().cloned(),
            lint: gate.lint.command().cloned(),
            typecheck: gate.typecheck.command().cloned(),
            repro_test: gate.repro_test.command().cloned(),
            test: gate.test.clone(),
            coverage: gate.coverage.command().cloned(),
            test_output: gate.test_output,
        })
        .map_err(invalid)?;

        let coverage_overridden = matches!(o.coverage, Some(StepOverride::Run(_)));
        if let Some(c) = &overrides.coverage {
            check_repo_path(&c.path).map_err(|e| invalid(format!("coverage.path: {e}")))?;
        }
        let coverage = match (&gate.coverage, &overrides.coverage) {
            (Step::Skipped(_), _) => None,
            (Step::Run(_), Some(c)) => Some(c.clone()),
            (Step::Run(_), None) if coverage_overridden => {
                return Err(invalid(
                    "a coverage command needs [coverage] format and path".into(),
                ));
            }
            (Step::Run(_), None) => self.coverage.clone(),
        };

        let edit_format = step(&self.edit.format, &overrides.edit_format).map_err(invalid)?;
        if let Some(c) = edit_format.command() {
            c.check_placeholders(&[Placeholder::File], &[Placeholder::File])
                .map_err(|e| invalid(format!("edit_format: {e}")))?;
        }
        let fetch = step(&self.dependencies.fetch, &overrides.fetch).map_err(invalid)?;
        let mut fetch_hosts = self.dependencies.fetch_hosts.clone();
        for host in &overrides.fetch_hosts {
            if !plain_host(host) {
                return Err(invalid(format!("fetch_hosts: `{host}` is not a host name")));
            }
            if !fetch_hosts.contains(host) {
                fetch_hosts.push(host.clone());
            }
        }
        if let Some(c) = fetch.command() {
            c.check_placeholders(&[], &[])
                .map_err(|e| invalid(format!("fetch: {e}")))?;
        }
        let audit = step(&self.dependencies.audit, &overrides.audit).map_err(invalid)?;
        if let Some(c) = audit.command() {
            check_audit(c).map_err(|e| invalid(format!("audit: {e}")))?;
        }
        let image = match &overrides.image {
            Some(i) => {
                check_image(i).map_err(|e| invalid(format!("image {e}")))?;
                i.clone()
            }
            None => self.sandbox.image.clone(),
        };
        Ok(ResolvedProfile {
            profile: self.clone(),
            gate,
            edit_format,
            fetch,
            fetch_hosts,
            audit,
            coverage,
            image,
        })
    }
}

fn harness(o: &GateOverrides) -> Result<Option<CustomHarness>, String> {
    let Some(failure) = &o.test_failure else {
        return match o.test_finished {
            Some(_) => Err("gate.test_finished needs gate.test_failure".into()),
            None => Ok(None),
        };
    };
    let groups = regex::Regex::new(failure)
        .map_err(|e| format!("gate.test_failure: {e}"))?
        .captures_len();
    if groups != 2 {
        return Err("gate.test_failure needs exactly one group: the test name".into());
    }
    if let Some(finished) = &o.test_finished {
        regex::Regex::new(finished).map_err(|e| format!("gate.test_finished: {e}"))?;
    }
    Ok(Some(CustomHarness {
        failure: failure.clone(),
        finished: o.test_finished.clone(),
    }))
}

/// A DNS name the proxy can match exactly: letters, digits, `-` and `.`.
fn plain_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with(['.', '-'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn step(default: &Option<CommandTemplate>, o: &Option<StepOverride>) -> Result<Step, String> {
    match (o, default) {
        (Some(StepOverride::Run(c)), _) => Ok(Step::Run(c.clone())),
        (Some(StepOverride::Enabled(false)), _) => Ok(Step::Skipped(SkipReason::DisabledByRepo)),
        (Some(StepOverride::Enabled(true)), _) => {
            Err("`true` is not a command; give the command or `false`".into())
        }
        (None, Some(c)) => Ok(Step::Run(c.clone())),
        (None, None) => Ok(Step::Skipped(SkipReason::NotInProfile)),
    }
}

impl ResolvedProfile {
    pub fn language(&self) -> Language {
        self.profile.language
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoverageFormat, Language, Profiles, TestOutput};

    fn python() -> Profile {
        Profiles::builtin()
            .unwrap()
            .get(Language::Python)
            .unwrap()
            .clone()
    }

    fn overrides(text: &str) -> ProfileOverrides {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn no_overrides_keeps_profile_defaults() {
        let r = python().resolve(&ProfileOverrides::default()).unwrap();
        assert_eq!(r.gate.test.argv(), ["python", "-m", "pytest"]);
        assert_eq!(
            r.gate.lint.command().unwrap().argv(),
            ["ruff", "check", "."]
        );
        assert_eq!(r.image, "aegis-sandbox-python");
        assert_eq!(
            r.coverage.as_ref().unwrap().format,
            CoverageFormat::Cobertura
        );
        assert_eq!(r.language(), Language::Python);
    }

    #[test]
    fn repo_commands_and_switches_win() {
        let r = python()
            .resolve(&overrides(
                r#"
                image = "registry.local/app-ci:3.12"
                fetch = ["pip", "install", "-r", "requirements.txt"]
                [gate]
                lint = ["make", "lint"]
                format = false
                coverage = false
                "#,
            ))
            .unwrap();
        assert_eq!(r.gate.lint.command().unwrap().argv(), ["make", "lint"]);
        assert_eq!(r.gate.format, Step::Skipped(SkipReason::DisabledByRepo));
        assert_eq!(r.gate.coverage, Step::Skipped(SkipReason::DisabledByRepo));
        assert_eq!(r.coverage, None);
        assert_eq!(r.image, "registry.local/app-ci:3.12");
        assert_eq!(r.fetch.command().unwrap().argv()[0], "pip");
    }

    #[test]
    fn repo_fetch_hosts_add_to_the_profiles() {
        let r = python()
            .resolve(&overrides(r#"fetch_hosts = ["github.com", "pypi.org"]"#))
            .unwrap();
        assert_eq!(
            r.fetch_hosts,
            ["pypi.org", "files.pythonhosted.org", "github.com"]
        );
        for bad in ["", "*.github.com", "github.com:443", "-x.org", "a b"] {
            let text = format!("fetch_hosts = [{bad:?}]");
            let err = python().resolve(&overrides(&text)).unwrap_err();
            assert!(err.to_string().contains("not a host name"), "{err}");
        }
    }

    #[test]
    fn a_custom_harness_is_read_on_top_of_libtest() {
        let rust = Profiles::builtin()
            .unwrap()
            .get(Language::Rust)
            .unwrap()
            .clone();
        let r = rust
            .resolve(&overrides(
                r#"
                [gate]
                test_failure = '^❌ (\S+) \('
                test_finished = '^\d+ passed, \d+ failed'
                "#,
            ))
            .unwrap();
        assert_eq!(r.gate.test_output, Some(TestOutput::Libtest));
        assert_eq!(r.gate.harness.unwrap().failure, r"^❌ (\S+) \(");
        for bad in [
            "[gate]\ntest_failure = '^❌ \\S+'",
            "[gate]\ntest_failure = '('",
            "[gate]\ntest_finished = 'x'",
        ] {
            assert!(rust.resolve(&overrides(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn steps_missing_from_profile_are_reported_as_skipped() {
        let mut p = python();
        p.gate.lint = None;
        let r = p.resolve(&ProfileOverrides::default()).unwrap();
        assert_eq!(r.gate.lint, Step::Skipped(SkipReason::NotInProfile));
    }

    #[test]
    fn audit_renders_lockfile_and_license_policy() {
        let r = python().resolve(&ProfileOverrides::default()).unwrap();
        let policy =
            crate::AllowList::from_toml(include_str!("../../../policies/licenses.toml")).unwrap();
        let licenses = policy.names().iter().cloned().collect::<Vec<_>>().join(",");
        let argv = r
            .audit
            .command()
            .unwrap()
            .render(&[
                (Placeholder::Lockfile, "uv.lock"),
                (Placeholder::Licenses, &licenses),
            ])
            .unwrap();
        assert_eq!(argv[..4], ["osv-scanner", "scan", "source", "--lockfile"]);
        assert_eq!(argv[4], "uv.lock");
        assert!(
            argv[5].starts_with("--licenses=0BSD,Apache-2.0,"),
            "{}",
            argv[5]
        );

        let rust = Profiles::builtin()
            .unwrap()
            .get(Language::Rust)
            .unwrap()
            .clone();
        let r = rust.resolve(&ProfileOverrides::default()).unwrap();
        assert_eq!(
            r.audit,
            Step::Skipped(SkipReason::NotInProfile),
            "Rust uses cargo deny"
        );
        let err = python()
            .resolve(&overrides("audit = [\"osv-scanner\", \"scan\"]\n"))
            .unwrap_err();
        assert!(
            err.to_string().contains("must contain `{lockfile}`"),
            "{err}"
        );
    }

    #[test]
    fn coverage_override_needs_its_report() {
        let o = overrides("[gate]\ncoverage = [\"make\", \"cov\"]\n");
        assert!(
            python()
                .resolve(&o)
                .unwrap_err()
                .to_string()
                .contains("[coverage]")
        );
        let o = overrides(
            "[gate]\ncoverage = [\"make\", \"cov\"]\n[coverage]\nformat = \"lcov\"\npath = \"out/lcov.info\"\n",
        );
        assert_eq!(
            python().resolve(&o).unwrap().coverage.unwrap().path,
            "out/lcov.info"
        );
    }

    #[test]
    fn rejects_unsafe_or_meaningless_overrides() {
        let cases = [
            ("[gate]\ntest = false\n", "cannot be turned off"),
            ("[gate]\nlint = true\n", "not a command"),
            (
                "[gate]\nrepro_test = [\"pytest\"]\n",
                "must contain `{test}`",
            ),
            ("[gate]\nlint = [\"ruff\", \"{test}\"]\n", "not allowed"),
            ("edit_format = [\"black\"]\n", "must contain `{file}`"),
            ("image = \" \"\n", "image must not contain whitespace"),
            (
                "image = \"--privileged\"\n",
                "image must not start with `-`",
            ),
            (
                "[gate]\ncoverage = [\"make\", \"cov\"]\n[coverage]\nformat = \"lcov\"\npath = \"../x\"\n",
                "must not contain `..`",
            ),
        ];
        for (text, needle) in cases {
            let err = python().resolve(&overrides(text)).unwrap_err().to_string();
            assert!(err.contains(needle), "{text}: {err}");
        }
        assert!(toml::from_str::<ProfileOverrides>("[gate]\ntypo = false\n").is_err());
        assert!(toml::from_str::<ProfileOverrides>("[gate]\nlint = []\n").is_err());
    }
}
