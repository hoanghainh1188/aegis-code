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

use aegis_lang::{
    DependencyTree, Placeholder, ReferenceUpdate, ResolvedProfile, SkipReason, Step,
    check_advisory, check_license, default_licenses, deny_config, in_dir, join_dir,
    new_dependency_violations, osv_config, part_label, read_lockfile,
};

use crate::build_cache::BuildCache;
use crate::command::{CancelToken, Exit, Ran, Runner};
use crate::egress::{Egress, EgressSession};
use crate::failures::{failing_tests_with, ran_tests};
use crate::findings::new_findings;
use crate::git::{self, BaseTree, Change};
use crate::integrity::Snapshot;
use crate::parts::{self, GatePart};
use crate::report::{GateReport, Skip, StepName, StepReport, StepStatus, UpdatedReferences, tail};
use crate::sandbox::Sandbox;
use crate::sonar::{self, SonarStep};
use crate::{GateError, deps};

/// Where the fetch container writes the resolved dependency tree (#80).
const TREE_DIR: &str = "/aegis-tree";

/// Where the audit container reads the repo's policy files (#159).
const AUDIT_DIR: &str = "/aegis-audit";

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
    /// Fetch the dependencies of a patch that changed them, once they pass
    /// the check (#66). Needs `sandbox`.
    pub fetch: Option<FetchStep>,
    /// Where the repo and the base copy start their build directories
    /// from; only the base copy refreshes it.
    pub build_cache: Option<BuildCache>,
    /// Tests the agent declares as allowed changes (their expected result
    /// had to change): the profile's `update_references` may rewrite their
    /// reference output, and only theirs (#105).
    pub allowed_tests: Vec<String>,
    /// Groups of tests the agent declares, when too many changed to name
    /// one by one (#147): their references may be rewritten too, only
    /// inside the group's directory.
    pub reference_groups: Vec<ReferenceGroup>,
    /// The Sonar server and project (#91); `None` skips the step as not
    /// configured.
    pub sonar: Option<SonarStep>,
    /// The repo's own policy for the profile's `dependencies.audit` (#159).
    pub audit: AuditPolicy,
    /// Directories another profile owns (#71); `profile`, `sandbox`,
    /// `fetch` and `build_cache` are the rest of the repo's. Only the parts
    /// the patch changes are checked, the rest of the repo first.
    pub parts: Vec<GatePart<'a>>,
}

/// A repo's licenses and ignored advisories, on top of `policies/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditPolicy {
    /// In place of `policies/licenses.toml` for `{licenses}`, and of
    /// `licenses.allow` in `policies/deny.toml` for `{deny_config}`.
    pub licenses: Option<Vec<String>>,
    /// Advisories the audit lets through.
    pub ignored_advisories: Vec<String>,
}

impl AuditPolicy {
    /// Licenses and advisories are plain identifiers, and a list of
    /// licenses is not empty: they end up in the audit's arguments.
    pub fn check_ids(&self) -> Result<(), String> {
        if self.licenses.as_ref().is_some_and(Vec::is_empty) {
            return Err("repo.licenses must not be empty when set".into());
        }
        let licenses = self.licenses.as_deref().unwrap_or_default();
        licenses
            .iter()
            .try_for_each(|l| check_license(l).map_err(|e| format!("repo.licenses: {e}")))?;
        self.ignored_advisories.iter().try_for_each(|a| {
            check_advisory(a).map_err(|e| format!("repo.ignored_advisories: {e}"))
        })
    }
}

/// Tests whose reference output changed together, e.g. every HTML hash of
/// typst's suite (#147). Only references: the tests still run after the
/// update and must then fail only as on the base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceGroup {
    /// A reference directory inside the profile's `reference_dirs`; the
    /// update may write only there.
    pub dir: String,
    /// Test names, `*` matching any run of characters.
    pub tests: String,
}

impl ReferenceGroup {
    fn covers(&self, id: &str) -> bool {
        glob(&self.tests, without_params(id)) || glob(&self.tests, harness_name(id))
    }

    /// `dir` without a trailing `/`, if it is a plain relative path inside
    /// one of `dirs`.
    fn dir_in(&self, dirs: &[String]) -> Option<&str> {
        let dir = self.dir.trim_end_matches('/');
        let plain = !dir.is_empty()
            && !dir.starts_with('/')
            && dir.split('/').all(|c| !matches!(c, "" | "." | ".."));
        (plain && dirs.iter().any(|d| dir == d || is_inside(dir, d))).then_some(dir)
    }
}

/// The profile's fetch command and where it may go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchStep {
    pub egress: Egress,
    pub command: Vec<String>,
    /// Where the audit may go (the profile's `audit_hosts`), through a
    /// proxy of its own: the fetch command never reaches these.
    pub audit_hosts: Vec<String>,
}

/// The part of the repo the steps are checking now: its profile, where its
/// commands run, and the patch's changes in it, relative to its directory.
struct Part<'a> {
    dir: String,
    profile: &'a ResolvedProfile,
    sandbox: Option<Sandbox>,
    fetch: Option<FetchStep>,
    build_cache: Option<BuildCache>,
    runner: Runner,
    /// The patched repo's directory of the part.
    repo: PathBuf,
    changes: Vec<Change>,
    ignored: Vec<String>,
    repro: Option<Repro>,
    /// The request's allowed tests, named as the part's harness does.
    allowed_tests: Vec<String>,
    reference_groups: Vec<ReferenceGroup>,
    logs: PathBuf,
    /// The base copy's directory of the part has its build cache.
    base_seeded: bool,
}

impl Part<'_> {
    fn label(&self) -> String {
        part_label(&self.dir, self.profile)
    }

    /// `path` of the part, from the repo root.
    fn shown(&self, path: &str) -> String {
        parts::shown(&self.dir, path)
    }
}

struct Run<'a> {
    req: &'a GateRequest<'a>,
    part: Part<'a>,
    /// Runs Sonar, on the whole repo.
    root_runner: Runner,
    base: String,
    base_tree: Option<BaseTree>,
    work: PathBuf,
    /// Every change of the patch, from the repo root.
    all_changes: Vec<Change>,
    /// The parts' directories, the rest of the repo (`""`) first.
    dirs: Vec<String>,
    /// The coverage report of every part, which their steps write.
    coverage_files: Vec<String>,
    snapshot: Snapshot,
    index: PathBuf,
    /// What is checked; written again after a reference update.
    tree: String,
    references: UpdatedReferences,
    /// Build caches to refresh from the base copy, with their part's dir.
    seeded: Vec<(BuildCache, String)>,
}

/// `^(a|b)$` matching exactly `names`, regex characters escaped.
fn exact_regex(names: &[&str]) -> String {
    let escaped: Vec<String> = names
        .iter()
        .map(|n| {
            n.chars()
                .flat_map(|c| {
                    let special = "\\.+*?()|[]{}^$#&-~".contains(c);
                    special.then_some('\\').into_iter().chain([c])
                })
                .collect()
        })
        .collect();
    format!("^({})$", escaped.join("|"))
}

/// Whether `id`, as read from test output, is the test named `test`. A
/// parametrized pytest case `<test>[<params>]` is one of its cases (#133).
fn is_test(id: &str, test: &str) -> bool {
    let id = without_params(id);
    id == test || id.ends_with(&format!("::{test}"))
}

/// Whether `id` is the test named `test` or one inside it, when `test` names
/// a file or a class (#136).
fn is_in(id: &str, test: &str) -> bool {
    let id = without_params(id);
    is_test(id, test) || id.starts_with(&format!("{test}::")) || id.contains(&format!("::{test}::"))
}

/// The test's own name, without the binary or file before `::`: what the
/// harness's update command selects.
fn harness_name(id: &str) -> &str {
    let id = without_params(id);
    id.rsplit("::").next().unwrap_or(id)
}

/// Whether `text` matches `pattern`, where `*` matches any run of
/// characters.
fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(mut rest) = parts.next().and_then(|first| text.strip_prefix(first)) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// Whether `path` is under the directory `dir`.
fn is_inside(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

fn without_params(id: &str) -> &str {
    match id.split_once('[') {
        Some((name, _)) if id.ends_with(']') => name,
        _ => id,
    }
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

/// `` `a`, `b` ``, or `none` for no paths.
fn quoted<S: AsRef<str>>(paths: &[S]) -> String {
    if paths.is_empty() {
        return "none".into();
    }
    paths
        .iter()
        .map(|p| format!("`{}`", p.as_ref()))
        .collect::<Vec<_>>()
        .join(", ")
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

impl<'a> Run<'a> {
    fn log(&self, name: &str) -> PathBuf {
        self.part.logs.join(format!("{name}.log"))
    }

    fn base_tree(&mut self) -> Result<&BaseTree, GateError> {
        if self.base_tree.is_none() {
            let path = self.work.join("base");
            self.base_tree = Some(BaseTree::create(self.req.repo, &self.base, &path)?);
        }
        let tree = self.base_tree.as_ref().expect("just created");
        if !self.part.base_seeded {
            if let Some(cache) = &self.part.build_cache {
                cache.seed(&tree.path().join(&self.part.dir))?;
                self.seeded.push((cache.clone(), self.part.dir.clone()));
            }
            self.part.base_seeded = true;
        }
        Ok(tree)
    }

    /// The base copy's directory of the part, once the copy exists.
    fn base_dir(&mut self) -> Result<PathBuf, GateError> {
        let dir = self.part.dir.clone();
        Ok(self.base_tree()?.path().join(dir))
    }

    /// Runs in the part's directory of the patched repo, or of the base
    /// copy when `on_base`; each side has its own `TMPDIR`.
    fn run_in(&self, argv: &[String], on_base: bool, log: &str) -> Result<Ran, GateError> {
        let (dir, tmp) = match (on_base, &self.base_tree) {
            (true, Some(tree)) => (tree.path().join(&self.part.dir), self.work.join("tmp-base")),
            _ => (self.part.repo.clone(), self.work.join("tmp")),
        };
        std::fs::create_dir_all(&tmp).map_err(|e| GateError::io(&tmp, &e))?;
        let tmp = tmp.display().to_string();
        self.part
            .runner
            .run(argv, &dir, &self.log(log), &[("TMPDIR", &tmp)])
    }

    /// Format, lint, typecheck: the command exits 0, or it fails on the
    /// base revision too and the patch adds no finding in the files it
    /// changed ("no new warnings", design §6.1; #104).
    fn simple(&mut self, name: StepName, step: &Step) -> Result<StepReport, GateError> {
        let command = match step {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c,
        };
        let argv = command.render(&[])?;
        let ran = self.run_in(&argv, false, name.id())?;
        let failed = |ran: &[&Ran], why: String| report(name, StepStatus::Failed, ran, why);
        match ran.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => {
                return Ok(report(name, StepStatus::Passed, &[&ran], String::new()));
            }
            Exit::Code(_) => {}
            e => {
                return Ok(failed(
                    &[&ran],
                    format!("{}:\n{}", exit_text(e), tail(&ran.log)),
                ));
            }
        }
        self.base_tree()?;
        let base = self.run_in(&argv, true, &format!("{}-base", name.id()))?;
        let both = [&ran, &base];
        match base.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            Exit::Code(c) if c != 0 => {}
            _ => {
                let why = format!("{}:\n{}", exit_text(ran.exit), tail(&ran.log));
                return Ok(failed(&both, why));
            }
        }
        let changed: Vec<String> = self.part.changes.iter().map(|c| c.path.clone()).collect();
        let new = new_findings(&read_log(&ran.log), &read_log(&base.log), &changed);
        if new.is_empty() {
            return Ok(report(name, StepStatus::Passed, &both, String::new()));
        }
        let why = format!(
            "new findings in the changed files (the base revision fails too, without these):\n{}\n\n{}",
            new.join("\n"),
            tail(&ran.log)
        );
        Ok(failed(&both, why))
    }

    /// The dependency tree to resolve for this patch: the profile has one,
    /// the patch changes a file of its `on_change`, and the gate has the
    /// fetch container to resolve it in.
    fn tree(&self) -> Option<&'a DependencyTree> {
        let profile: &'a ResolvedProfile = self.part.profile;
        let tree = profile.tree.as_ref()?;
        (self.part.fetch.is_some() && self.part.sandbox.is_some())
            .then_some(tree)
            .filter(|t| {
                self.part
                    .changes
                    .iter()
                    .any(|c| deps::matches_any(&t.on_change, &c.path))
            })
    }

    fn dependencies(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::Dependencies;
        let tree = self.tree();
        let checked = deps::check(
            &self.part.profile.profile,
            self.req.repo,
            &self.part.dir,
            &self.base,
            &self.part.changes,
            &self.part.ignored,
            self.req.allowed,
            tree.map_or(&[], |t| t.on_change.as_slice()),
        )?;
        Ok(match checked {
            None => StepReport::skipped(name, Skip::NoDependencyChange),
            Some(Ok(())) => self.fetch_checked(tree)?,
            Some(Err(why)) => report(name, StepStatus::Failed, &[], why),
        })
    }

    /// The patch's dependencies passed the check: compare the resolved
    /// trees when there is one (#80), fetch them through the egress proxy
    /// into the caches the later steps read (#66), then audit the changed
    /// lockfiles (#159). Without a sandbox there is nothing to fetch into,
    /// and the audit runs on the machine.
    fn fetch_checked(&mut self, tree: Option<&DependencyTree>) -> Result<StepReport, GateError> {
        let name = StepName::Dependencies;
        let mut ran = Vec::new();
        if let Some(failed) = self.fetch(tree, &mut ran)? {
            return Ok(failed);
        }
        if let Some(failed) = self.audit(&mut ran)? {
            return Ok(failed);
        }
        let all: Vec<&Ran> = ran.iter().collect();
        Ok(report(name, StepStatus::Passed, &all, String::new()))
    }

    /// `Some` is the step's report when fetching does not pass.
    fn fetch(
        &mut self,
        tree: Option<&DependencyTree>,
        ran: &mut Vec<Ran>,
    ) -> Result<Option<StepReport>, GateError> {
        let name = StepName::Dependencies;
        let req: &GateRequest<'_> = self.req;
        let (Some(fetch), Some(sandbox)) = (self.part.fetch.clone(), self.part.sandbox.clone())
        else {
            return Ok(None);
        };
        let session = fetch.egress.start()?;
        if let Some(tree) = tree
            && let Some(failed) = self.compare_trees(tree, &session, &sandbox, ran)?
        {
            return Ok(Some(failed));
        }
        let fetched = session.fetch(
            &sandbox,
            &fetch.command,
            &self.part.repo,
            &self.log("fetch"),
            req.step_timeout,
            req.cancel.clone(),
        )?;
        ran.push(fetched);
        let all: Vec<&Ran> = ran.iter().collect();
        let fetched = ran.last().expect("just pushed");
        Ok(match fetched.exit {
            Exit::Cancelled => Some(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => None,
            e => Some(report(
                name,
                StepStatus::Failed,
                &all,
                format!(
                    "fetching the new dependencies failed ({}):\n{}\n\nproxy:\n{}",
                    exit_text(e),
                    tail(&fetched.log),
                    session.log()
                ),
            )),
        })
    }

    /// Lockfiles of the part's profile the patch changed and kept.
    fn changed_lockfiles(&self) -> Vec<String> {
        let lockfiles = &self.part.profile.profile.dependencies.lockfiles;
        self.part
            .changes
            .iter()
            .filter(|c| deps::matches_any(lockfiles, &c.path))
            .filter(|c| self.part.repo.join(&c.path).is_file())
            .map(|c| c.path.clone())
            .collect()
    }

    /// The profile's `dependencies.audit` on each changed lockfile, with
    /// the repo's policy (#159). In a sandbox it runs in a container whose
    /// only way out is a proxy to `audit_hosts`. `Some` is the step's
    /// report when an audit does not pass.
    fn audit(&mut self, ran: &mut Vec<Ran>) -> Result<Option<StepReport>, GateError> {
        let name = StepName::Dependencies;
        let req: &GateRequest<'_> = self.req;
        let Step::Run(command) = &self.part.profile.audit else {
            return Ok(None);
        };
        let lockfiles = self.changed_lockfiles();
        if lockfiles.is_empty() {
            return Ok(None);
        }
        req.audit.check_ids().map_err(GateError::InvalidRequest)?;
        let dir = self.work.join("audit");
        std::fs::create_dir_all(&dir).map_err(|e| GateError::io(&dir, &e))?;
        let policy = &req.audit;
        for (file, text) in [
            (
                "deny.toml",
                deny_config(policy.licenses.as_deref(), &policy.ignored_advisories)?,
            ),
            ("osv-scanner.toml", osv_config(&policy.ignored_advisories)?),
        ] {
            let path = dir.join(file);
            std::fs::write(&path, text).map_err(|e| GateError::io(&path, &e))?;
        }
        let licenses = policy
            .licenses
            .clone()
            .unwrap_or_else(default_licenses)
            .join(",");
        let contained = match (&self.part.fetch, &self.part.sandbox) {
            (Some(fetch), Some(sandbox)) => {
                let egress = Egress {
                    allow: fetch.audit_hosts.clone(),
                    ..fetch.egress.clone()
                };
                let mut auditor = sandbox.clone();
                auditor.mounts.push((dir.clone(), AUDIT_DIR.to_owned()));
                Some((egress.start()?, auditor))
            }
            _ => None,
        };
        let policy_dir = match contained {
            Some(_) => AUDIT_DIR.to_owned(),
            None => dir.display().to_string(),
        };
        let (deny, osv) = (
            format!("{policy_dir}/deny.toml"),
            format!("{policy_dir}/osv-scanner.toml"),
        );
        for (i, lockfile) in lockfiles.iter().enumerate() {
            let lockfile_dir = match lockfile.rsplit_once('/') {
                Some((parent, _)) => parent,
                None => ".",
            };
            let argv = command.render(&[
                (Placeholder::Lockfile, lockfile),
                (Placeholder::LockfileDir, lockfile_dir),
                (Placeholder::Licenses, &licenses),
                (Placeholder::DenyConfig, &deny),
                (Placeholder::OsvConfig, &osv),
            ])?;
            let log = format!("audit-{i}");
            let audited = match &contained {
                Some((session, auditor)) => session.fetch(
                    auditor,
                    &argv,
                    &self.part.repo,
                    &self.log(&log),
                    req.step_timeout,
                    req.cancel.clone(),
                )?,
                None => self.run_in(&argv, false, &log)?,
            };
            let exit = audited.exit;
            ran.push(audited);
            match exit {
                Exit::Cancelled => return Ok(Some(StepReport::skipped(name, Skip::Cancelled))),
                e if e.success() => {}
                e => {
                    let last = ran.last().expect("just pushed");
                    let mut why = format!(
                        "auditing {} failed ({}): an advisory or a license outside \
                         the repo's policy, or the audit could not run:\n{}",
                        self.part.shown(lockfile),
                        exit_text(e),
                        tail(&last.log)
                    );
                    if let Some((session, _)) = &contained {
                        why.push_str(&format!("\n\nproxy:\n{}", session.log()));
                    }
                    let all: Vec<&Ran> = ran.iter().collect();
                    return Ok(Some(report(name, StepStatus::Failed, &all, why)));
                }
            }
        }
        Ok(None)
    }

    /// Resolves the dependency tree on the base and on the patch, in the
    /// fetch container, and rejects what the patch adds like a lockfile
    /// change. Resolving downloads only what the tree needs to be known
    /// (poms for Maven), not the new dependencies themselves. `Some` is
    /// the step's report when it does not pass.
    fn compare_trees(
        &mut self,
        tree: &DependencyTree,
        session: &EgressSession,
        sandbox: &Sandbox,
        ran: &mut Vec<Ran>,
    ) -> Result<Option<StepReport>, GateError> {
        let name = StepName::Dependencies;
        let base_dir = self.base_dir()?;
        let patch_dir = self.part.repo.clone();
        let mut trees = Vec::new();
        for (side, dir) in [("base", base_dir.as_path()), ("patch", patch_dir.as_path())] {
            let out = self.work.join(format!("tree-{side}"));
            // A fresh directory: the command may append to the file.
            if out.exists() {
                std::fs::remove_dir_all(&out).map_err(|e| GateError::io(&out, &e))?;
            }
            std::fs::create_dir_all(&out).map_err(|e| GateError::io(&out, &e))?;
            let mut resolver = sandbox.clone();
            resolver.mounts.push((out.clone(), TREE_DIR.to_owned()));
            let inside = format!("{TREE_DIR}/{}", tree.file);
            let argv = tree.command.render(&[(Placeholder::Output, &inside)])?;
            let r = session.fetch(
                &resolver,
                &argv,
                dir,
                &self.log(&format!("tree-{side}")),
                self.req.step_timeout,
                self.req.cancel.clone(),
            )?;
            let exit = r.exit;
            ran.push(r);
            let all: Vec<&Ran> = ran.iter().collect();
            let which = if side == "base" {
                "on the base revision"
            } else {
                "with the patch"
            };
            match exit {
                Exit::Cancelled => return Ok(Some(StepReport::skipped(name, Skip::Cancelled))),
                e if !e.success() => {
                    let last = ran.last().expect("just pushed");
                    let why = format!(
                        "resolving the dependency tree {which} failed ({}):\n{}\n\nproxy:\n{}",
                        exit_text(e),
                        tail(&last.log),
                        session.log()
                    );
                    return Ok(Some(report(name, StepStatus::Failed, &all, why)));
                }
                _ => {}
            }
            let file = out.join(&tree.file);
            let read = std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    read_lockfile(self.part.profile.profile.language, &tree.file, &text)
                        .map_err(|e| e.to_string())
                });
            match read {
                Ok(deps) => trees.push(deps),
                Err(e) => {
                    let why = format!("the dependency tree {which} cannot be read: {e}");
                    return Ok(Some(report(name, StepStatus::Failed, &all, why)));
                }
            }
        }
        let violations = new_dependency_violations(
            &trees[0],
            &trees[1],
            self.req.allowed,
            &self.part.profile.profile.dependencies.official_source,
        );
        if violations.is_empty() {
            return Ok(None);
        }
        let why = violations
            .iter()
            .map(|v| format!("dependency tree: {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        let all: Vec<&Ran> = ran.iter().collect();
        Ok(Some(report(name, StepStatus::Failed, &all, why)))
    }

    /// The reproduction test passes with the patch and fails on the base
    /// revision with only its own files copied in.
    fn repro(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::ReproTest;
        let command = match &self.part.profile.gate.repro_test {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c.clone(),
        };
        let Some(repro) = self.part.repro.clone() else {
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
            .part
            .repo
            .canonicalize()
            .map_err(|e| GateError::io(&self.part.repo, &e))?;
        let sources = repro
            .files
            .iter()
            .map(|f| regular_file_in(&repo, f))
            .collect::<Result<Vec<_>, _>>()?;
        let base_dir = self.base_dir()?;
        // Files the base has and the patch changes: the copy brings every
        // change of the patch in them, a fix there too (#140).
        let carried: Vec<&str> = repro
            .files
            .iter()
            .filter(|f| base_dir.join(f).exists())
            .filter(|f| self.part.changes.iter().any(|c| &c.path == *f))
            .map(String::as_str)
            .collect();
        for (file, from) in repro.files.iter().zip(&sources) {
            let to = base_dir.join(file);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| GateError::io(parent, &e))?;
            }
            std::fs::copy(from, &to).map_err(|e| GateError::io(from, &e))?;
        }
        let base = self.run_in(&argv, true, "repro_test-base");
        if let Some(tree) = &self.base_tree {
            let files: Vec<String> = repro.files.iter().map(|f| self.part.shown(f)).collect();
            tree.restore(self.req.repo, &self.base, &files)?;
        }
        let base = base?;
        let ran = [&patched, &base];
        let harness = self.part.profile.gate.harness.as_ref();
        let failed_on_base = match (base.exit, self.part.profile.gate.test_output) {
            (Exit::Cancelled, _) => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            (Exit::TimedOut, _) => {
                let why = format!("`{}` timed out on the base revision", repro.test);
                return Ok(report(name, StepStatus::Failed, &ran, why));
            }
            (e, _) if e.success() => false,
            // The test itself must fail there, not the build around it.
            (_, Some(format)) => failing_tests_with(format, harness, &read_log(&base.log))
                .iter()
                .any(|id| is_in(id, &repro.test)),
            (e, None) => matches!(e, Exit::Code(_)),
        };
        if failed_on_base {
            return Ok(report(name, StepStatus::Passed, &ran, String::new()));
        }
        let reported = |r: &Ran| {
            let format = self.part.profile.gate.test_output?;
            let ids = ran_tests(format, &read_log(&r.log))?;
            Some(ids.iter().any(|id| is_in(id, &repro.test)))
        };
        let why = if base.exit.success()
            && reported(&patched) == Some(true)
            && reported(&base) == Some(false)
        {
            format!(
                "`{}` did not run on the base revision: no test by that name is there. Only the \
                 files in `repro_test.files` ({}) are copied onto the base; a test written inside \
                 a source file the fix changes is not there. Put the reproduction test in a file \
                 that holds only tests and list that file.",
                repro.test,
                quoted(&repro.files)
            )
        } else if base.exit.success() {
            let mut why = format!(
                "`{}` also passes on the base revision, without the fix: it does not reproduce \
                 the issue",
                repro.test
            );
            if !carried.is_empty() {
                why.push_str(&format!(
                    ". The patch also changes {}, copied onto the base with the test: if the fix \
                     is in there, it was copied too. Put the reproduction test in a file that \
                     holds only tests.",
                    quoted(&carried)
                ));
            }
            why
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
        let argv = self.part.profile.gate.test.render(&[])?;
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
        let Some(format) = self.part.profile.gate.test_output else {
            return fail(
                &[&patched],
                format!(
                    "the tests fail ({}):\n{}",
                    exit_text(patched.exit),
                    tail(&patched.log)
                ),
            );
        };
        let harness = self.part.profile.gate.harness.as_ref();
        let read = |ran: &Ran| failing_tests_with(format, harness, &read_log(&ran.log));
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
        // A test that fails once and passes on the next run is flaky, not
        // broken by the patch (#104): only failures of both runs count.
        let again = self.run_in(&argv, false, "test-rerun")?;
        if again.exit == Exit::Cancelled {
            return Ok(StepReport::skipped(name, Skip::Cancelled));
        }
        let failing_again = if again.exit.success() {
            BTreeSet::new()
        } else {
            read(&again)
        };
        let (broken, flaky): (BTreeSet<String>, BTreeSet<String>) =
            new.into_iter().partition(|t| failing_again.contains(t));
        let ran = [&patched, &base, &again];
        if broken.is_empty() {
            let detail = format!(
                "{} tests failed once and passed when run again (flaky):\n{}",
                flaky.len(),
                list(&flaky)
            );
            return Ok(report(name, StepStatus::Passed, &ran, detail));
        }
        let Some(update) = self.part.profile.gate.update_references.clone() else {
            return fail(
                &ran,
                format!(
                    "{} tests fail that pass on the base revision:\n{}\n\n{}",
                    broken.len(),
                    list(&broken),
                    tail(&again.log)
                ),
            );
        };
        // Every new failure is a test the agent says had to change, by name
        // or in a group: its reference output is rewritten, then all the
        // tests run again.
        let allowed_tests = self.part.allowed_tests.clone();
        let declared = |id: &String| allowed_tests.iter().any(|t| is_test(id, t));
        let reference_groups = self.part.reference_groups.clone();
        let (groups, ignored): (Vec<&ReferenceGroup>, Vec<&ReferenceGroup>) = reference_groups
            .iter()
            .partition(|g| g.dir_in(&update.dirs).is_some());
        let grouped = |id: &String| groups.iter().any(|g| g.covers(id));
        if broken.iter().all(|id| declared(id) || grouped(id)) {
            let mut names: Vec<String> = allowed_tests
                .iter()
                .filter(|t| broken.iter().any(|id| is_test(id, t)))
                .cloned()
                .collect();
            let in_groups: Vec<&String> = broken.iter().filter(|id| !declared(id)).collect();
            // A group's update may write only in its directory; with tests
            // declared by name too, in the profile's (#105).
            let dirs: Vec<String> = if names.is_empty() {
                groups
                    .iter()
                    .filter(|g| in_groups.iter().any(|id| g.covers(id)))
                    .filter_map(|g| g.dir_in(&update.dirs))
                    .map(str::to_owned)
                    .collect()
            } else {
                update.dirs.clone()
            };
            for id in in_groups {
                let name = harness_name(id).to_owned();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            return self.update_references(&update, &names, &dirs, &argv, &on_base, &ran);
        }
        let mut hint = String::from(
            "If the fix had to change their reference output, declare each in \
             `allowed_test_changes`, or groups of them in `allowed_reference_changes`.",
        );
        if !ignored.is_empty() {
            let dirs: Vec<&str> = ignored.iter().map(|g| g.dir.as_str()).collect();
            hint.push_str(&format!(
                " Groups ignored, their directory is not inside {}: {}.",
                quoted(&update.dirs),
                quoted(&dirs)
            ));
        }
        fail(
            &ran,
            format!(
                "{} tests fail that pass on the base revision:\n{}\n\n{hint}\n\n{}",
                broken.len(),
                list(&broken),
                tail(&again.log)
            ),
        )
    }

    /// Rewrites the reference output of `names` with the profile's command,
    /// checks it wrote nothing outside `dirs`, and runs the tests once more:
    /// they must now fail only as on the base (#105).
    fn update_references(
        &mut self,
        update: &ReferenceUpdate,
        names: &[String],
        dirs: &[String],
        test_argv: &[String],
        on_base: &BTreeSet<String>,
        earlier: &[&Ran],
    ) -> Result<StepReport, GateError> {
        let name = StepName::Test;
        let mut ran: Vec<&Ran> = earlier.to_vec();
        let pattern = exact_regex(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let argv = update.command.render(&[(Placeholder::Test, &pattern)])?;
        let updated = self.run_in(&argv, false, "update-references")?;
        match updated.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            Exit::TimedOut | Exit::Signal => {
                let why = format!(
                    "updating the reference outputs {}:\n{}",
                    exit_text(updated.exit),
                    tail(&updated.log)
                );
                ran.push(&updated);
                return Ok(report(name, StepStatus::Failed, &ran, why));
            }
            // A harness may exit non-zero after it updated what failed.
            Exit::Code(_) => {}
        }
        // Paths from the repo root; `dirs` are inside the part.
        let part_dir = self.part.dir.clone();
        let inside = |path: &str| {
            in_dir(&part_dir, path).is_some_and(|p| dirs.iter().any(|d| is_inside(p, d)))
        };
        let before: BTreeSet<String> = self.all_changes.iter().map(|c| c.path.clone()).collect();
        let changes = git::changes(self.req.repo, &self.base, &self.index)?;
        let mut written: BTreeSet<String> = self
            .snapshot
            .changed_since(self.req.repo)
            .into_iter()
            .collect();
        written.extend(
            changes
                .iter()
                .map(|c| c.path.clone())
                .filter(|p| !before.contains(p)),
        );
        let outside: BTreeSet<String> = written.iter().filter(|p| !inside(p)).cloned().collect();
        if !outside.is_empty() {
            let shown: Vec<String> = dirs.iter().map(|d| self.part.shown(d)).collect();
            let why = format!(
                "updating the reference outputs wrote outside {}:\n{}",
                shown.join(", "),
                list(&outside)
            );
            ran.push(&updated);
            return Ok(report(name, StepStatus::Failed, &ran, why));
        }
        // What is checked, and sent, now includes the new references.
        self.part.changes = parts::scoped_changes(&self.dirs, self.part_index(), &changes);
        self.all_changes = changes;
        self.snapshot = Snapshot::take(
            self.req.repo,
            &git::files(self.req.repo, &self.index)?,
            &self.coverage_files,
        );
        self.tree = git::write_tree(self.req.repo, &self.index)?;

        let after = self.run_in(test_argv, false, "test-after-update")?;
        if after.exit == Exit::Cancelled {
            return Ok(StepReport::skipped(name, Skip::Cancelled));
        }
        let format = self.part.profile.gate.test_output.expect("compared above");
        let harness = self.part.profile.gate.harness.as_ref();
        let failing = if after.exit.success() {
            BTreeSet::new()
        } else {
            failing_tests_with(format, harness, &read_log(&after.log))
        };
        let still: BTreeSet<String> = failing.difference(on_base).cloned().collect();
        let detail = format!(
            "updated the reference outputs of {} declared tests:\n{}\nfiles:\n{}",
            names.len(),
            list(&names.iter().cloned().collect()),
            list(&written)
        );
        ran.push(&updated);
        ran.push(&after);
        if still.is_empty() {
            self.references.tests.extend(names.iter().cloned());
            self.references.files.extend(written);
            return Ok(report(name, StepStatus::Passed, &ran, detail));
        }
        Ok(report(
            name,
            StepStatus::Failed,
            &ran,
            format!(
                "{detail}\n\n{} tests still fail that pass on the base revision:\n{}\n\n{}",
                still.len(),
                list(&still),
                tail(&after.log)
            ),
        ))
    }

    fn coverage(&self) -> Result<StepReport, GateError> {
        let name = StepName::Coverage;
        let command = match &self.part.profile.gate.coverage {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c,
        };
        let path = self
            .part
            .profile
            .coverage
            .as_ref()
            .map(|c| self.part.repo.join(&c.path));
        if let Some(p) = path.as_ref().filter(|p| p.exists()) {
            std::fs::remove_file(p).map_err(|e| GateError::io(p, &e))?;
        }
        let ran = self.run_in(&command.render(&[])?, false, name.id())?;
        let failed = |why: String| report(name, StepStatus::Failed, &[&ran], why);
        Ok(match (ran.exit, &path) {
            (Exit::Cancelled, _) => StepReport::skipped(name, Skip::Cancelled),
            (e @ (Exit::TimedOut | Exit::Signal), _) => {
                failed(format!("{}:\n{}", exit_text(e), tail(&ran.log)))
            }
            (e, Some(p)) if !p.exists() => failed(format!(
                "no coverage report at `{}` ({}):\n{}",
                p.display(),
                exit_text(e),
                tail(&ran.log)
            )),
            (e, None) if !e.success() => failed(format!("{}:\n{}", exit_text(e), tail(&ran.log))),
            // The coverage run also runs the tests; their failures were
            // judged against the base by the test step (#104).
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

    fn sonar(&self) -> Result<StepReport, GateError> {
        let Some(step) = &self.req.sonar else {
            return Ok(StepReport::skipped(StepName::Sonar, Skip::NotConfigured));
        };
        sonar::run(
            step,
            &sonar::Context {
                repo: self.req.repo,
                base: self.req.base,
                tree: &self.tree,
                profile: &self.req.profile.profile,
                properties: sonar::repo_properties(
                    all_parts(self.req)
                        .iter()
                        .map(|p| (p.dir.as_str(), &p.profile.profile)),
                ),
                runner: &self.root_runner,
                logs: &self.work.join("logs"),
            },
        )
    }

    fn part_index(&self) -> usize {
        self.dirs
            .iter()
            .position(|d| *d == self.part.dir)
            .unwrap_or(0)
    }

    fn step(&mut self, name: StepName) -> Result<StepReport, GateError> {
        let gate = &self.part.profile.gate;
        match name {
            StepName::Integrity => Ok(self.integrity()),
            StepName::Dependencies => self.dependencies(),
            StepName::Format => self.simple(name, &gate.format.clone()),
            StepName::Lint => self.simple(name, &gate.lint.clone()),
            StepName::Typecheck => self.simple(name, &gate.typecheck.clone()),
            StepName::ReproTest => self.repro(),
            StepName::Test => self.test(),
            StepName::Coverage => self.coverage(),
            StepName::Sonar => self.sonar(),
        }
    }
}

/// The parts of the request, the rest of the repo first.
fn all_parts<'a>(req: &GateRequest<'a>) -> Vec<GatePart<'a>> {
    let rest = GatePart {
        dir: String::new(),
        profile: req.profile,
        sandbox: req.sandbox.clone(),
        fetch: req.fetch.clone(),
        build_cache: req.build_cache.clone(),
    };
    std::iter::once(rest)
        .chain(req.parts.iter().cloned())
        .collect()
}

/// Every part's directory is a plain one inside the repo, named once.
fn check_parts(parts: &[GatePart<'_>]) -> Result<(), GateError> {
    for (i, part) in parts.iter().enumerate().skip(1) {
        aegis_lang::check_dir(&part.dir)
            .map_err(|e| GateError::InvalidRequest(format!("part {e}")))?;
        if parts[..i].iter().any(|p| p.dir == part.dir) {
            return Err(GateError::InvalidRequest(format!(
                "part `{}` is given twice",
                part.dir
            )));
        }
        if let Some(sandbox) = &part.sandbox {
            sandbox.check()?;
        }
    }
    Ok(())
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
    if let Some(sandbox) = &req.sandbox {
        sandbox.check()?;
    }
    let all = all_parts(req);
    check_parts(&all)?;
    let dirs: Vec<String> = all.iter().map(|p| p.dir.clone()).collect();
    let logs = work.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| GateError::io(&logs, &e))?;
    let base = git::resolve(req.repo, req.base)?;
    let index = work.join("index");
    let changes = git::changes(req.repo, &base, &index)?;
    let mut caches: Vec<String> = all
        .iter()
        .flat_map(|p| p.profile.profile.sandbox.cache_dirs.iter())
        .filter(|d| !d.contains('/'))
        .cloned()
        .collect();
    caches.sort();
    caches.dedup();
    let ignored = git::ignored(req.repo, &index, &caches)?;
    let tree = git::write_tree(req.repo, &index)?;
    let coverage_files: Vec<String> = all
        .iter()
        .filter_map(|p| Some(join_dir(&p.dir, &p.profile.coverage.as_ref()?.path)))
        .collect();
    let snapshot = Snapshot::take(req.repo, &git::files(req.repo, &index)?, &coverage_files);

    let repro = req.repro.map(|r| parts::repro_part(&dirs, r));
    let mut touched: Vec<usize> = (0..all.len())
        .filter(|&i| !parts::scoped_changes(&dirs, i, &changes).is_empty())
        .collect();
    if let Some((i, _)) = &repro
        && !touched.contains(i)
    {
        touched.push(*i);
        touched.sort_unstable();
    }
    if touched.is_empty() {
        touched.push(0);
    }

    let runner = |sandbox: &Option<Sandbox>| Runner {
        env: req.env.clone(),
        timeout: req.step_timeout,
        cancel: req.cancel.clone(),
        sandbox: sandbox.clone(),
    };
    let part = |i: usize| -> Result<Part<'_>, GateError> {
        let p = &all[i];
        let logs = match i {
            0 => logs.clone(),
            _ => logs.join(p.dir.replace('/', "_")),
        };
        std::fs::create_dir_all(&logs).map_err(|e| GateError::io(&logs, &e))?;
        Ok(Part {
            dir: p.dir.clone(),
            profile: p.profile,
            sandbox: p.sandbox.clone(),
            fetch: p.fetch.clone(),
            build_cache: p.build_cache.clone(),
            runner: runner(&p.sandbox),
            repo: repo.join(&p.dir),
            changes: parts::scoped_changes(&dirs, i, &changes),
            ignored: parts::scoped_paths(&dirs, i, &ignored),
            repro: repro
                .as_ref()
                .filter(|(r, _)| *r == i)
                .map(|(_, r)| r.clone()),
            allowed_tests: parts::scoped_tests(&p.dir, &req.allowed_tests),
            reference_groups: parts::scoped_groups(&dirs, i, &req.reference_groups),
            logs,
            base_seeded: false,
        })
    };
    let mut run = Run {
        req,
        part: part(touched[0])?,
        root_runner: runner(&req.sandbox),
        base,
        base_tree: None,
        work,
        all_changes: changes.clone(),
        dirs: dirs.clone(),
        coverage_files,
        snapshot,
        index,
        tree,
        references: UpdatedReferences::default(),
        seeded: vec![],
    };
    let mut reports: Vec<Vec<(String, StepReport)>> = vec![vec![]; StepName::ALL.len()];
    let mut stop: Option<Skip> = None;
    for (n, &i) in touched.iter().enumerate() {
        if n > 0 {
            run.part = part(i)?;
        }
        if let Some(cache) = &run.part.build_cache {
            cache.seed(&run.part.repo)?;
        }
        let label = run.part.label();
        for (k, name) in StepName::ALL.into_iter().enumerate() {
            if stop.is_none() && req.cancel.is_cancelled() {
                stop = Some(Skip::Cancelled);
            }
            let step = match stop {
                Some(skip) => StepReport::skipped(name, skip),
                // Sonar analyses the whole repo once, after every part's
                // coverage step wrote its report.
                None if name == StepName::Sonar && n + 1 < touched.len() => {
                    StepReport::skipped(name, Skip::NotConfigured)
                }
                None => run.step(name)?,
            };
            stop = stop.or(match step.status {
                StepStatus::Failed => Some(Skip::NotRun),
                StepStatus::Skipped(Skip::Cancelled) => Some(Skip::Cancelled),
                _ => None,
            });
            let label = match name {
                StepName::Sonar => "the whole repo".to_owned(),
                _ => label.clone(),
            };
            reports[k].push((label, step));
        }
    }
    let steps = StepName::ALL
        .into_iter()
        .zip(reports)
        .map(|(name, reports)| parts::merge(name, reports))
        .collect();
    if let Some(base) = &run.base_tree {
        for (cache, dir) in &run.seeded {
            // Upstream code only; a failed refresh costs time, not correctness.
            let _ = cache.refresh_from(&base.path().join(dir));
        }
    }
    let (tree, base) = (run.tree.clone(), run.base.clone());
    Ok(GateReport {
        steps,
        tree,
        base,
        references: run.references,
        parts: touched.iter().map(|&i| dirs[i].clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_is_matched_by_its_full_name_or_its_last_parts() {
        let test = "tests/test_x.py::test_y";
        assert!(is_test("tests/test_x.py::test_y", test));
        assert!(is_test("tests/test_x.py::test_y", "test_y"));
        assert!(is_test("tests/test_x.py::TestX::test_y", "TestX::test_y"));
        assert!(!is_test("tests/test_x.py::test_y2", test));
        assert!(!is_test("tests/test_x.py::test_y2", "test_y"));
    }

    #[test]
    fn a_parametrized_case_is_one_of_its_cases() {
        let test = "tests/test_x.py::test_y";
        assert!(is_test(
            "tests/test_x.py::test_y[http://u:80@h:80/p-x]",
            test
        ));
        assert!(is_test("tests/test_x.py::test_y[a::b]", "test_y"));
        assert!(!is_test("tests/test_x.py::test_y2[1]", test));
        // Brackets that do not end the id are part of the name.
        assert!(!is_test("tests/test_x.py::test_y[1]::more", test));
    }

    #[test]
    fn a_file_or_class_holds_the_tests_inside_it() {
        let id = "tests/test_x.py::TestX::test_y[a::b]";
        assert!(is_in(id, "tests/test_x.py::TestX"));
        assert!(is_in(id, "TestX"));
        assert!(is_in(id, "tests/test_x.py"));
        assert!(is_in(id, "tests/test_x.py::TestX::test_y"));
        assert!(!is_in(id, "tests/test_x.py::Test"));
        assert!(!is_in(
            "tests/test_x.py::TestX2::test_y",
            "tests/test_x.py::TestX"
        ));
        assert!(!is_in("tests/test_x.py::TestX2::test_y", "TestX"));
        assert!(!is_in("tests/test_x.pyc::test_y", "tests/test_x.py"));
        // Only the reproduction test may name a node; a test allowed to
        // change is matched by its own name.
        assert!(!is_test(id, "tests/test_x.py::TestX"));
    }

    #[test]
    fn a_star_matches_any_run_of_characters() {
        assert!(glob("*", "tests::math-mat-html"));
        assert!(glob("*", ""));
        assert!(glob("math-*", "math-mat-html"));
        assert!(glob("*-html", "math-mat-html"));
        assert!(glob("math-*-html", "math-mat-html"));
        assert!(glob("m*a*t", "mat"));
        assert!(!glob("math-*", "tests::math-mat"));
        assert!(!glob("*-html", "math-mat-html2"));
        assert!(!glob("a*a", "a"));
        assert!(!glob("*x*x", "x"));
        assert!(!glob("math", "math-mat"));
        assert!(!glob("", "math"));
    }

    #[test]
    fn a_group_covers_tests_by_their_own_name_or_full_id() {
        let group = |tests: &str| ReferenceGroup {
            dir: "tests/ref/html".into(),
            tests: tests.into(),
        };
        assert!(group("math-*").covers("tests::math-mat-html"));
        assert!(group("tests::math-*").covers("tests::math-mat-html"));
        assert!(group("test_*").covers("tests/test_x.py::test_y[1]"));
        assert!(!group("math-*").covers("tests::basic-table"));
        assert_eq!(harness_name("tests/test_x.py::test_y[a::b]"), "test_y");
        assert_eq!(harness_name("shows-value"), "shows-value");
    }

    #[test]
    fn a_group_s_directory_must_be_a_reference_dir_or_inside_one() {
        let dirs = vec!["tests/ref".to_owned()];
        let dir_in = |dir: &str| {
            ReferenceGroup {
                dir: dir.into(),
                tests: "*".into(),
            }
            .dir_in(&dirs)
            .map(str::to_owned)
        };
        assert_eq!(dir_in("tests/ref/html/").as_deref(), Some("tests/ref/html"));
        assert_eq!(dir_in("tests/ref").as_deref(), Some("tests/ref"));
        for outside in [
            "tests",
            "tests/reference",
            "tests/ref/../suite",
            "tests/ref/./html",
            "tests/ref//html",
            "/tests/ref/html",
            "",
        ] {
            assert_eq!(dir_in(outside), None, "{outside}");
        }
    }
}
