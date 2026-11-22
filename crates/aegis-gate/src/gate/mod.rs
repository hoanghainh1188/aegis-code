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

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use aegis_lang::{
    DependencyTree, Placeholder, Profile, ReferenceUpdate, ResolvedProfile, SkipReason, Step,
    TestOutput, check_advisory, check_license, default_licenses, deny_config, in_dir, join_dir,
    osv_config, part_label, read_lockfile,
};

use crate::build_cache::BuildCache;
use crate::command::{CancelToken, Exit, Ran, Runner, out_of_memory};
use crate::dependency_policy::DependencyPolicy;
use crate::egress::{Egress, EgressSession};
use crate::failures::{failing_tests_with, missed, names_test, ran_tests, selector};
use crate::findings::new_findings;
use crate::git::{self, BaseTree, Change};
use crate::integrity::Snapshot;
use crate::parts::{self, GatePart};
use crate::report::{GateReport, Skip, StepName, StepReport, StepStatus, UpdatedReferences, tail};
use crate::sandbox::Sandbox;
use crate::scope::{ScopePolicy, analyze_scope};
use crate::secret_scan::{SecretPolicy, scan_patch};
use crate::sidecar::{SidecarSession, Sidecars, check_sidecars};
use crate::sonar::{self, SonarStep};
use crate::{GateError, deps};

mod base;
mod checks;
mod dependencies;
mod helpers;
mod repro;
mod test_step;

use helpers::*;
pub(crate) use helpers::{glob, is_in};

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
    /// Patch scope limit and enforcement policy (#359).
    pub scope: ScopePolicy,
    /// Secrets the repo lets the `secrets` step pass (#509).
    pub secrets: SecretPolicy,
    /// Directories another profile owns (#71); `profile`, `sandbox`,
    /// `fetch` and `build_cache` are the rest of the repo's. Only the parts
    /// the patch changes are checked, the rest of the repo first.
    pub parts: Vec<GatePart<'a>>,
    /// Services the test steps reach (#490, ADR 0013). Needs `sandbox`.
    pub sidecars: Option<Sidecars>,
    /// What the patch may change in the dependencies (#500): `Locked`
    /// for bug-fixes and review rounds, `Upgrade` for a CVE job.
    pub dependency_policy: DependencyPolicy,
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
    /// The request's sidecars once a test step needed them (#490); one
    /// set for the whole run, reset before each test command but the first.
    sidecars: RefCell<Option<SidecarSession>>,
    /// The step now running reaches the sidecars: `repro_test`, `test`,
    /// `coverage`.
    with_sidecars: Cell<bool>,
    /// Runs Sonar, on the whole repo.
    root_runner: Runner,
    base: String,
    base_tree: Option<BaseTree>,
    work: PathBuf,
    /// Every change of the patch, from the repo root.
    all_changes: Vec<Change>,
    /// The parts' directories, the rest of the repo (`""`) first.
    dirs: Vec<String>,
    /// The parts' profiles, in the order of `dirs`.
    profiles: Vec<&'a Profile>,
    /// The coverage report of every part, which their steps write.
    coverage_files: Vec<String>,
    snapshot: Snapshot,
    index: PathBuf,
    /// What is checked; written again after a reference update.
    tree: String,
    references: UpdatedReferences,
    /// Build caches to refresh from the base copy, with their part's dir.
    seeded: Vec<(BuildCache, String)>,
    /// Secrets the `secrets` step found in the patch (#509).
    secrets_found: usize,
}

/// The reruns of suspected new failures, and those that failed in every
/// one (#306).
type Reruns = (Vec<Ran>, BTreeSet<String>);

impl<'a> Run<'a> {
    fn part_index(&self) -> usize {
        self.dirs
            .iter()
            .position(|d| *d == self.part.dir)
            .unwrap_or(0)
    }

    fn step(&mut self, name: StepName) -> Result<StepReport, GateError> {
        self.with_sidecars.set(matches!(
            name,
            StepName::ReproTest | StepName::Test | StepName::Coverage
        ));
        let gate = &self.part.profile.gate;
        match name {
            StepName::Scope => self.scope(),
            StepName::Secrets => self.secrets(),
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
    if let Some(sidecars) = &req.sidecars {
        if req.sandbox.is_none() && !sidecars.specs.is_empty() {
            return Err(GateError::InvalidRequest(
                "sidecars need the sandbox: the test steps reach them on a Docker network".into(),
            ));
        }
        check_sidecars(&sidecars.specs).map_err(GateError::InvalidRequest)?;
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
        sidecars: RefCell::new(None),
        with_sidecars: Cell::new(false),
        root_runner: runner(&req.sandbox),
        base,
        base_tree: None,
        work,
        all_changes: changes.clone(),
        dirs: dirs.clone(),
        profiles: all.iter().map(|p| &p.profile.profile).collect(),
        coverage_files,
        snapshot,
        index,
        tree,
        references: UpdatedReferences::default(),
        seeded: vec![],
        secrets_found: 0,
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
                // The secrets step scans the whole patch once (#509).
                None if name == StepName::Secrets && n > 0 => {
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
                StepName::Sonar | StepName::Secrets => "the whole repo".to_owned(),
                _ => label.clone(),
            };
            reports[k].push((label, step));
        }
    }
    let steps: Vec<StepReport> = StepName::ALL
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
    let sidecars = run
        .sidecars
        .borrow_mut()
        .take()
        .map(SidecarSession::teardown);
    let (tree, base) = (run.tree.clone(), run.base.clone());
    let scope_warnings = match steps.iter().find(|s| s.name == StepName::Scope) {
        Some(s) if !s.detail.is_empty() => vec![s.detail.clone()],
        _ => vec![],
    };
    Ok(GateReport {
        steps,
        tree,
        base,
        references: run.references,
        parts: touched.iter().map(|&i| dirs[i].clone()).collect(),
        scope_warnings,
        secrets_found: run.secrets_found,
        sidecars,
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
