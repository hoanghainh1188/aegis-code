//! The gate run of design §6.1, on the machine for now (#11; Docker comes
//! with #5). Steps run in order and the first failure stops the rest. The
//! base revision is written out next to the patch only when a step needs
//! it: to see the reproduction test fail without the fix, and to tell new
//! test failures from ones the base already has.
//!
//! On the machine, the patch's code runs as the gate's user with the same
//! `HOME`, so a hostile patch can still reach the host and the base run.
//! Each run gets its own `TMPDIR`; full isolation of the patched and base
//! runs comes with the sandbox (#5).

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use aegis_lang::{Placeholder, ResolvedProfile, SkipReason, Step};

use crate::command::{CancelToken, Exit, Ran, Runner};
use crate::failures::failing_tests;
use crate::git::{self, BaseTree, Change};
use crate::integrity::Snapshot;
use crate::report::{GateReport, Skip, StepName, StepReport, StepStatus, tail};
use crate::sandbox::Sandbox;
use crate::{GateError, deps};

/// Failing tests listed for the agent; the rest are counted.
const MAX_LISTED_TESTS: usize = 20;

/// The test that shows the issue, and the files that hold it. The files
/// are copied onto the base revision to see the test fail there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repro {
    pub test: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GateRequest<'a> {
    /// Git checkout of the patched repo; the gate runs commands in it.
    pub repo: &'a Path,
    /// Revision the patch is compared with (the main branch).
    pub base: &'a str,
    pub profile: &'a ResolvedProfile,
    /// Packages a patch may add (`policies/allowed-*.toml`).
    pub allowed: &'a BTreeSet<String>,
    pub repro: Option<&'a Repro>,
    /// Logs and the base checkout go here; must be outside `repo`.
    pub work_dir: &'a Path,
    /// Added to the small inherited environment of every command.
    pub env: Vec<(String, String)>,
    pub step_timeout: Duration,
    pub cancel: CancelToken,
    /// Run the steps in containers (#5); `None` runs them on the machine.
    pub sandbox: Option<Sandbox>,
}

struct Run<'a> {
    req: &'a GateRequest<'a>,
    runner: Runner,
    base: String,
    base_tree: Option<BaseTree>,
    work: PathBuf,
    logs: PathBuf,
    changes: Vec<Change>,
    ignored: Vec<String>,
    snapshot: Snapshot,
}

/// Whether `id`, as read from test output, is the test named `test`.
fn is_test(id: &str, test: &str) -> bool {
    id == test || id.ends_with(&format!("::{test}"))
}

fn skip_of(reason: SkipReason) -> Skip {
    match reason {
        SkipReason::NotInProfile => Skip::NotInProfile,
        SkipReason::DisabledByRepo => Skip::DisabledByRepo,
    }
}

fn exit_text(exit: Exit) -> String {
    match exit {
        Exit::Code(c) => format!("exit code {c}"),
        Exit::Signal => "killed by a signal".into(),
        Exit::TimedOut => "timed out".into(),
        Exit::Cancelled => "cancelled".into(),
    }
}

fn list(tests: &BTreeSet<String>) -> String {
    let mut lines: Vec<String> = tests
        .iter()
        .take(MAX_LISTED_TESTS)
        .map(|t| format!("  {t}"))
        .collect();
    if tests.len() > MAX_LISTED_TESTS {
        lines.push(format!("  … and {} more", tests.len() - MAX_LISTED_TESTS));
    }
    lines.join("\n")
}

fn report(name: StepName, status: StepStatus, ran: &[&Ran], detail: String) -> StepReport {
    StepReport {
        name,
        status,
        duration: ran.iter().map(|r| r.duration).sum(),
        detail,
        logs: ran.iter().map(|r| r.log.clone()).collect(),
    }
}

/// `repo/path` when it is a plain relative path to a regular file that
/// stays inside `repo`: no `..`, no symlink anywhere on the way.
fn regular_file_in(repo: &Path, path: &str) -> Result<PathBuf, GateError> {
    let invalid = || {
        GateError::InvalidRequest(format!(
            "repro file `{path}` must be a regular file inside the repo, not a link"
        ))
    };
    let p = Path::new(path);
    if p.is_absolute() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(invalid());
    }
    let full = repo.join(p);
    let meta = std::fs::symlink_metadata(&full).map_err(|_| invalid())?;
    let real = full.canonicalize().map_err(|_| invalid())?;
    if !meta.is_file() || real != full {
        return Err(invalid());
    }
    Ok(full)
}

/// A log as text (logs are capped, see `command::MAX_LOG_BYTES`).
fn read_log(path: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).into_owned()
}

impl Run<'_> {
    fn log(&self, name: &str) -> PathBuf {
        self.logs.join(format!("{name}.log"))
    }

    fn base_tree(&mut self) -> Result<&BaseTree, GateError> {
        if self.base_tree.is_none() {
            let path = self.work.join("base");
            self.base_tree = Some(BaseTree::create(self.req.repo, &self.base, &path)?);
        }
        Ok(self.base_tree.as_ref().expect("just created"))
    }

    /// Runs in the patched repo, or in the base copy when `on_base`; each
    /// side has its own `TMPDIR`.
    fn run_in(&self, argv: &[String], on_base: bool, log: &str) -> Result<Ran, GateError> {
        let (dir, tmp) = match (on_base, &self.base_tree) {
            (true, Some(tree)) => (tree.path(), self.work.join("tmp-base")),
            _ => (self.req.repo, self.work.join("tmp")),
        };
        std::fs::create_dir_all(&tmp).map_err(|e| GateError::io(&tmp, &e))?;
        let tmp = tmp.display().to_string();
        self.runner
            .run(argv, dir, &self.log(log), &[("TMPDIR", &tmp)])
    }

    /// Format, lint, typecheck: the command must exit 0.
    fn simple(&self, name: StepName, step: &Step) -> Result<StepReport, GateError> {
        let command = match step {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c,
        };
        let ran = self.run_in(&command.render(&[])?, false, name.id())?;
        Ok(match ran.exit {
            Exit::Cancelled => StepReport::skipped(name, Skip::Cancelled),
            e if e.success() => report(name, StepStatus::Passed, &[&ran], String::new()),
            e => report(
                name,
                StepStatus::Failed,
                &[&ran],
                format!("{}:\n{}", exit_text(e), tail(&ran.log)),
            ),
        })
    }

    fn dependencies(&self) -> Result<StepReport, GateError> {
        let name = StepName::Dependencies;
        let checked = deps::check(
            &self.req.profile.profile,
            self.req.repo,
            &self.base,
            &self.changes,
            &self.ignored,
            self.req.allowed,
        )?;
        Ok(match checked {
            None => StepReport::skipped(name, Skip::NoDependencyChange),
            Some(Ok(())) => report(name, StepStatus::Passed, &[], String::new()),
            Some(Err(why)) => report(name, StepStatus::Failed, &[], why),
        })
    }

    /// The reproduction test passes with the patch and fails on the base
    /// revision with only its own files copied in.
    fn repro(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::ReproTest;
        let command = match &self.req.profile.gate.repro_test {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c.clone(),
        };
        let Some(repro) = self.req.repro else {
            return Ok(StepReport::skipped(name, Skip::NoReproTest));
        };
        let argv = command.render(&[(Placeholder::Test, &repro.test)])?;
        let patched = self.run_in(&argv, false, "repro_test")?;
        match patched.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if !e.success() => {
                let detail = format!(
                    "`{}` does not pass with the patch ({}):\n{}",
                    repro.test,
                    exit_text(e),
                    tail(&patched.log)
                );
                return Ok(report(name, StepStatus::Failed, &[&patched], detail));
            }
            _ => {}
        }
        let repo = self
            .req
            .repo
            .canonicalize()
            .map_err(|e| GateError::io(self.req.repo, &e))?;
        let sources = repro
            .files
            .iter()
            .map(|f| regular_file_in(&repo, f))
            .collect::<Result<Vec<_>, _>>()?;
        let base_dir = self.base_tree()?.path().to_path_buf();
        for (file, from) in repro.files.iter().zip(&sources) {
            let to = base_dir.join(file);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| GateError::io(parent, &e))?;
            }
            std::fs::copy(from, &to).map_err(|e| GateError::io(from, &e))?;
        }
        let base = self.run_in(&argv, true, "repro_test-base");
        if let Some(tree) = &self.base_tree {
            tree.restore(self.req.repo, &self.base, &repro.files)?;
        }
        let base = base?;
        let ran = [&patched, &base];
        let failed_on_base = match (base.exit, self.req.profile.gate.test_output) {
            (Exit::Cancelled, _) => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            (Exit::TimedOut, _) => {
                let why = format!("`{}` timed out on the base revision", repro.test);
                return Ok(report(name, StepStatus::Failed, &ran, why));
            }
            (e, _) if e.success() => false,
            // The test itself must fail there, not the build around it.
            (_, Some(format)) => failing_tests(format, &read_log(&base.log))
                .iter()
                .any(|id| is_test(id, &repro.test)),
            (e, None) => matches!(e, Exit::Code(_)),
        };
        if failed_on_base {
            return Ok(report(name, StepStatus::Passed, &ran, String::new()));
        }
        let why = if base.exit.success() {
            format!(
                "`{}` also passes on the base revision, without the fix: it does not reproduce \
                 the issue",
                repro.test
            )
        } else {
            format!(
                "`{}` did not run as a failing test on the base revision ({}):\n{}",
                repro.test,
                exit_text(base.exit),
                tail(&base.log)
            )
        };
        Ok(report(name, StepStatus::Failed, &ran, why))
    }

    /// All tests; fails only on tests the base revision does not fail.
    fn test(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::Test;
        let argv = self.req.profile.gate.test.render(&[])?;
        let patched = self.run_in(&argv, false, "test")?;
        let fail = |ran: &[&Ran], why: String| Ok(report(name, StepStatus::Failed, ran, why));
        match patched.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => {
                return Ok(report(name, StepStatus::Passed, &[&patched], String::new()));
            }
            Exit::TimedOut => return fail(&[&patched], "the tests timed out".into()),
            Exit::Signal => {
                let why = format!("the tests were killed by a signal:\n{}", tail(&patched.log));
                return fail(&[&patched], why);
            }
            _ => {}
        }
        let Some(format) = self.req.profile.gate.test_output else {
            return fail(
                &[&patched],
                format!(
                    "the tests fail ({}):\n{}",
                    exit_text(patched.exit),
                    tail(&patched.log)
                ),
            );
        };
        let read = |ran: &Ran| failing_tests(format, &read_log(&ran.log));
        let failing = read(&patched);
        if failing.is_empty() {
            return fail(
                &[&patched],
                format!(
                    "the test command failed ({}) without a failing test, e.g. a build error:\n{}",
                    exit_text(patched.exit),
                    tail(&patched.log)
                ),
            );
        }
        self.base_tree()?;
        let base = self.run_in(&argv, true, "test-base")?;
        let on_base = match base.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => BTreeSet::new(),
            _ => {
                let on_base = read(&base);
                if on_base.is_empty() {
                    return fail(
                        &[&patched, &base],
                        format!(
                            "the tests also fail on the base revision ({}), and no failing test \
                             could be read to compare",
                            exit_text(base.exit)
                        ),
                    );
                }
                on_base
            }
        };
        let new: BTreeSet<String> = failing.difference(&on_base).cloned().collect();
        if new.is_empty() {
            let detail = format!(
                "{} tests fail, as on the base revision:\n{}",
                failing.len(),
                list(&failing)
            );
            return Ok(report(name, StepStatus::Passed, &[&patched, &base], detail));
        }
        fail(
            &[&patched, &base],
            format!(
                "{} tests fail that pass on the base revision:\n{}\n\n{}",
                new.len(),
                list(&new),
                tail(&patched.log)
            ),
        )
    }

    fn coverage(&self) -> Result<StepReport, GateError> {
        let name = StepName::Coverage;
        let command = match &self.req.profile.gate.coverage {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c,
        };
        let path = self
            .req
            .profile
            .coverage
            .as_ref()
            .map(|c| self.req.repo.join(&c.path));
        if let Some(p) = path.as_ref().filter(|p| p.exists()) {
            std::fs::remove_file(p).map_err(|e| GateError::io(p, &e))?;
        }
        let ran = self.run_in(&command.render(&[])?, false, name.id())?;
        Ok(match (ran.exit, &path) {
            (Exit::Cancelled, _) => StepReport::skipped(name, Skip::Cancelled),
            (e, _) if !e.success() => report(
                name,
                StepStatus::Failed,
                &[&ran],
                format!("{}:\n{}", exit_text(e), tail(&ran.log)),
            ),
            (_, Some(p)) if !p.exists() => report(
                name,
                StepStatus::Failed,
                &[&ran],
                format!("no coverage report at `{}`", p.display()),
            ),
            _ => report(name, StepStatus::Passed, &[&ran], String::new()),
        })
    }

    /// None of the steps rewrote a file of the patch.
    fn integrity(&self) -> StepReport {
        let name = StepName::Integrity;
        let changed = self.snapshot.changed_since(self.req.repo);
        if changed.is_empty() {
            return report(name, StepStatus::Passed, &[], String::new());
        }
        let changed: BTreeSet<String> = changed.into_iter().collect();
        let why = format!(
            "the steps changed files of the patch, so what was checked is not what would be \
             sent:\n{}",
            list(&changed)
        );
        report(name, StepStatus::Failed, &[], why)
    }

    fn step(&mut self, name: StepName) -> Result<StepReport, GateError> {
        let gate = &self.req.profile.gate;
        match name {
            StepName::Integrity => Ok(self.integrity()),
            StepName::Dependencies => self.dependencies(),
            StepName::Format => self.simple(name, &gate.format.clone()),
            StepName::Lint => self.simple(name, &gate.lint.clone()),
            StepName::Typecheck => self.simple(name, &gate.typecheck.clone()),
            StepName::ReproTest => self.repro(),
            StepName::Test => self.test(),
            StepName::Coverage => self.coverage(),
            StepName::Sonar => Ok(StepReport::skipped(name, Skip::NotConfigured)),
        }
    }
}

pub fn run_gate(req: &GateRequest<'_>) -> Result<GateReport, GateError> {
    let repo = req
        .repo
        .canonicalize()
        .map_err(|e| GateError::io(req.repo, &e))?;
    std::fs::create_dir_all(req.work_dir).map_err(|e| GateError::io(req.work_dir, &e))?;
    let work = req
        .work_dir
        .canonicalize()
        .map_err(|e| GateError::io(req.work_dir, &e))?;
    if work.starts_with(&repo) {
        return Err(GateError::InvalidRequest(
            "work_dir must be outside the repo, or its files count as part of the patch".into(),
        ));
    }
    let logs = work.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| GateError::io(&logs, &e))?;
    let base = git::resolve(req.repo, req.base)?;
    let index = work.join("index");
    let changes = git::changes(req.repo, &base, &index)?;
    let caches: Vec<String> = req
        .profile
        .profile
        .sandbox
        .cache_dirs
        .iter()
        .filter(|d| !d.contains('/'))
        .cloned()
        .collect();
    let ignored = git::ignored(req.repo, &index, &caches)?;
    let tree = git::write_tree(req.repo, &index)?;
    let coverage = req.profile.coverage.as_ref().map(|c| c.path.as_str());
    let snapshot = Snapshot::take(req.repo, &git::files(req.repo, &index)?, coverage);
    let mut run = Run {
        req,
        runner: Runner {
            env: req.env.clone(),
            timeout: req.step_timeout,
            cancel: req.cancel.clone(),
            sandbox: req.sandbox.clone(),
        },
        base,
        base_tree: None,
        work,
        logs,
        changes,
        ignored,
        snapshot,
    };
    let mut steps = Vec::new();
    let mut stop: Option<Skip> = None;
    for name in StepName::ALL {
        if stop.is_none() && req.cancel.is_cancelled() {
            stop = Some(Skip::Cancelled);
        }
        if let Some(skip) = stop {
            steps.push(StepReport::skipped(name, skip));
            continue;
        }
        let step = run.step(name)?;
        stop = match step.status {
            StepStatus::Failed => Some(Skip::NotRun),
            StepStatus::Skipped(Skip::Cancelled) => Some(Skip::Cancelled),
            _ => None,
        };
        steps.push(step);
    }
    let base = run.base.clone();
    Ok(GateReport { steps, tree, base })
}
