//! `run_tests` (design §3.1, #137): the profile's `gate.repro_test` command
//! for one test, on the tree as the agent left it, in the gate's sandbox
//! (no network, the profile's image and caches) or on the machine when the
//! gate runs there. The agent sees whether its test passes before the gate
//! does; it is not a gate run and changes nothing the gate decides.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use aegis_gate::{CancelToken, Exit, Runner, Sandbox, failing_tests_with, out_of_memory, tail};
use aegis_lang::{Placeholder, ResolvedProfile};

use crate::AciError;
use crate::workspace::Workspace;

/// Longest a run may take. Kept below the orchestrator's call timeout, so
/// the agent gets the result (or the timeout) instead of a dropped call.
pub const RUN_TESTS_TIMEOUT: Duration = Duration::from_secs(240);

/// Failing tests listed; the rest are counted.
const MAX_LISTED: usize = 20;

/// Where and how long the tests run.
#[derive(Debug, Clone)]
pub struct TestSettings {
    /// The gate's container; `None` runs on the machine, as the gate does
    /// then.
    pub sandbox: Option<Sandbox>,
    /// Added to the command's environment (the run's `sandbox_env`).
    pub env: Vec<(String, String)>,
    /// Capped at [`RUN_TESTS_TIMEOUT`].
    pub timeout: Duration,
    /// Logs and the command's `TMPDIR`; outside the repo, so a run leaves
    /// nothing in the patch.
    pub work_dir: PathBuf,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self {
            sandbox: None,
            env: Vec::new(),
            timeout: RUN_TESTS_TIMEOUT,
            work_dir: std::env::temp_dir().join(format!("aegis-aci-tests-{}", std::process::id())),
        }
    }
}

#[derive(Debug)]
pub(crate) struct TestRunner {
    settings: TestSettings,
    cancel: CancelToken,
    runs: u32,
}

/// What one `run_tests` call shows the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRun {
    pub test: String,
    pub exit: Exit,
    pub duration: Duration,
    /// Failing tests read from the output, when the profile says how.
    pub failing: BTreeSet<String>,
    /// The runner exited 0 but ran no test: the id selects none (#235).
    pub ran_nothing: bool,
    /// The last lines of the output.
    pub tail: String,
}

impl TestRunner {
    pub(crate) fn new(settings: TestSettings) -> Self {
        Self {
            settings,
            cancel: CancelToken::new(),
            runs: 0,
        }
    }

    /// Stops a run in progress, now and for every later call.
    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    pub(crate) fn run(&mut self, workspace: &Workspace, test: &str) -> Result<TestRun, AciError> {
        let test = test.trim();
        if test.is_empty() {
            return Err(AciError::InvalidInput("`test` is empty".into()));
        }
        // A test inside a part's directory runs with that part's profile,
        // in that directory (#71).
        let (part, name) = workspace.part_of_test(test);
        let profile: &ResolvedProfile = &part.profile;
        let Some(command) = profile.gate.repro_test.command() else {
            return Err(AciError::InvalidInput(
                "this repo has no command to run one test".into(),
            ));
        };
        let argv = command.render(&[(Placeholder::Test, name)])?;
        self.runs += 1;
        let dir = &self.settings.work_dir;
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| AciError::io(&tmp, &e))?;
        let log = dir.join(format!("run_tests-{}.log", self.runs));
        let runner = Runner {
            env: self.settings.env.clone(),
            timeout: self.settings.timeout.min(RUN_TESTS_TIMEOUT),
            cancel: self.cancel.clone(),
            sandbox: self.settings.sandbox.clone().map(|s| {
                // Locked down as the part's formatter is (#320).
                let hardening = workspace
                    .format_container_of(part)
                    .map_or_else(|| s.hardening.clone(), |c| c.hardening().clone());
                match part.dir.is_empty() {
                    true => Sandbox { hardening, ..s },
                    // The part's image and dependency dirs (#198).
                    false => Sandbox {
                        image: part.profile.image.clone(),
                        mounts: workspace
                            .part_mounts(&part.dir)
                            .iter()
                            .map(|(host, inside)| (host.into(), inside.clone()))
                            .collect(),
                        hardening,
                        ..s
                    },
                }
            }),
        };
        let tmp = tmp.display().to_string();
        let ran = runner
            .run(
                &argv,
                &workspace.root().join(&part.dir),
                &log,
                &[("TMPDIR", &tmp)],
            )
            .map_err(|e| AciError::Tests(e.to_string()))?;
        let output =
            String::from_utf8_lossy(&std::fs::read(&ran.log).unwrap_or_default()).into_owned();
        let failing = profile
            .gate
            .test_output
            .map(|format| failing_tests_with(format, profile.gate.harness.as_ref(), &output))
            .unwrap_or_default();
        let ran_nothing = ran.exit == Exit::Code(0) && profile.gate.repro_ran_nothing(&output);
        Ok(TestRun {
            test: test.to_owned(),
            exit: ran.exit,
            duration: ran.duration,
            failing,
            ran_nothing,
            tail: tail(&ran.log),
        })
    }
}

impl fmt::Display for TestRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.duration.as_secs();
        match self.exit {
            Exit::Code(0) if self.ran_nothing => write!(
                f,
                "RAN NO TEST `{}` ({secs}s): no test has that id; give it in the form \
                 the conventions say",
                self.test
            )?,
            Exit::Code(0) => write!(f, "PASSED `{}` ({secs}s)", self.test)?,
            Exit::Code(c) => write!(f, "FAILED `{}` (exit code {c}, {secs}s)", self.test)?,
            Exit::Signal => write!(f, "FAILED `{}` (killed by a signal, {secs}s)", self.test)?,
            Exit::TimedOut => write!(
                f,
                "TIMED OUT `{}` after {secs}s; run a narrower test",
                self.test
            )?,
            Exit::Cancelled => write!(f, "CANCELLED `{}`", self.test)?,
            Exit::OutOfMemory { limit_mb } => write!(
                f,
                "FAILED `{}` ({}, {secs}s)",
                self.test,
                out_of_memory(limit_mb)
            )?,
        }
        if !self.failing.is_empty() {
            f.write_str("\nfailing tests:")?;
            for name in self.failing.iter().take(MAX_LISTED) {
                write!(f, "\n  {name}")?;
            }
            if self.failing.len() > MAX_LISTED {
                write!(f, "\n  … and {} more", self.failing.len() - MAX_LISTED)?;
            }
        }
        if !self.tail.is_empty() {
            write!(f, "\n--- end of output ---\n{}", self.tail)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(exit: Exit, failing: &[&str]) -> TestRun {
        TestRun {
            test: "tests/test_x.py::test_y".into(),
            exit,
            duration: Duration::from_secs(3),
            failing: failing.iter().map(|s| (*s).to_owned()).collect(),
            ran_nothing: false,
            tail: "1 failed in 0.1s".into(),
        }
    }

    #[test]
    fn a_run_that_selected_no_test_does_not_say_passed() {
        let nothing = TestRun {
            test: "textutil_test.go::TestX".into(),
            ran_nothing: true,
            ..run(Exit::Code(0), &[])
        };
        assert!(
            nothing
                .to_string()
                .starts_with("RAN NO TEST `textutil_test.go::TestX` (3s): no test has that id")
        );
    }

    #[test]
    fn the_result_says_pass_or_fail_first_then_the_failing_tests_and_the_output() {
        assert_eq!(
            run(Exit::Code(0), &[]).to_string(),
            "PASSED `tests/test_x.py::test_y` (3s)\n--- end of output ---\n1 failed in 0.1s"
        );
        assert_eq!(
            run(Exit::Code(1), &["tests/test_x.py::test_y"]).to_string(),
            "FAILED `tests/test_x.py::test_y` (exit code 1, 3s)\nfailing tests:\n  \
             tests/test_x.py::test_y\n--- end of output ---\n1 failed in 0.1s"
        );
        assert!(
            run(Exit::TimedOut, &[])
                .to_string()
                .starts_with("TIMED OUT `tests/test_x.py::test_y` after 3s")
        );
    }

    #[test]
    fn long_failure_lists_are_cut() {
        let names: Vec<String> = (0..25).map(|i| format!("t{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let text = run(Exit::Code(1), &refs).to_string();
        assert!(text.contains("\n  t19\n  … and 5 more\n"), "{text}");
        assert!(!text.contains("t20"), "{text}");
    }
}
