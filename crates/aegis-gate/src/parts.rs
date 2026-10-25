//! A repo of more than one language (ADR 0004 §1, #71): each directory
//! with its own profile is a part, the request's profile is the rest of the
//! repo. The gate checks only the parts the patch changes, one after the
//! other, each in its own directory; one report per step comes out of all
//! of them.

use aegis_lang::{ResolvedProfile, deepest, in_dir, join_dir};

use crate::build_cache::BuildCache;
use crate::gate::{FetchStep, ReferenceGroup, Repro};
use crate::git::Change;
use crate::report::{Skip, StepName, StepReport, StepStatus};
use crate::sandbox::Sandbox;

/// A directory of the repo that another profile owns, with that profile's
/// container, fetch and build cache (paths inside it are relative to it).
#[derive(Debug, Clone)]
pub struct GatePart<'a> {
    /// Relative to the repo root, `/` separators, e.g. `web`.
    pub dir: String,
    pub profile: &'a ResolvedProfile,
    /// Mounts this directory as the container's work directory.
    pub sandbox: Option<Sandbox>,
    pub fetch: Option<FetchStep>,
    pub build_cache: Option<BuildCache>,
}

/// `path` relative to part `i` of `dirs`, when part `i` is the deepest
/// one holding it.
pub(crate) fn scoped<'p>(dirs: &[String], i: usize, path: &'p str) -> Option<&'p str> {
    let owner = deepest(dirs.iter().map(String::as_str), path)?;
    (owner == i).then(|| in_dir(&dirs[i], path)).flatten()
}

/// The changes part `i` owns, relative to its directory.
pub(crate) fn scoped_changes(dirs: &[String], i: usize, changes: &[Change]) -> Vec<Change> {
    changes
        .iter()
        .filter_map(|c| {
            scoped(dirs, i, &c.path).map(|path| Change {
                path: path.to_owned(),
                gitlink: c.gitlink,
            })
        })
        .collect()
}

/// Ignored paths part `i` owns, relative to its directory; a directory
/// keeps its trailing `/`.
pub(crate) fn scoped_paths(dirs: &[String], i: usize, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|p| {
            let (path, slash) = match p.strip_suffix('/') {
                Some(dir) => (dir, "/"),
                None => (p.as_str(), ""),
            };
            scoped(dirs, i, path).map(|s| format!("{s}{slash}"))
        })
        .collect()
}

/// The part that runs the reproduction test: the one holding all its
/// files, the test named relative to it. Files in different parts stay
/// with the root, as the agent named them.
pub(crate) fn repro_part(dirs: &[String], repro: &Repro) -> (usize, Repro) {
    let owners: Vec<usize> = repro
        .files
        .iter()
        .filter_map(|f| deepest(dirs.iter().map(String::as_str), f))
        .collect();
    let Some(&i) = owners.first() else {
        return (0, repro.clone());
    };
    if i == 0 || owners.iter().any(|o| *o != i) || owners.len() != repro.files.len() {
        return (0, repro.clone());
    }
    let dir = &dirs[i];
    let files = repro
        .files
        .iter()
        .filter_map(|f| in_dir(dir, f).map(str::to_owned))
        .collect();
    let test = in_dir(dir, &repro.test).unwrap_or(&repro.test).to_owned();
    (i, Repro { test, files })
}

/// Test names as the part's harness prints them: one named by a path in
/// the part's directory loses that directory (`web/t/a.test.ts` →
/// `t/a.test.ts`); any other name is kept.
pub(crate) fn scoped_tests(dir: &str, tests: &[String]) -> Vec<String> {
    tests
        .iter()
        .map(|t| in_dir(dir, t).unwrap_or(t).to_owned())
        .collect()
}

/// The groups whose directory is in part `i`, relative to it.
pub(crate) fn scoped_groups(
    dirs: &[String],
    i: usize,
    groups: &[ReferenceGroup],
) -> Vec<ReferenceGroup> {
    groups
        .iter()
        .filter_map(|g| {
            let dir = g.dir.trim_end_matches('/');
            scoped(dirs, i, dir).map(|d| ReferenceGroup {
                dir: d.to_owned(),
                tests: g.tests.clone(),
            })
        })
        .collect()
}

/// `path` of part `dir` as the agent sees it, from the repo root.
pub(crate) fn shown(dir: &str, path: &str) -> String {
    join_dir(dir, path)
}

/// Which status of a step stands for all parts: a failure, then a stop,
/// then a pass; a skip only when every part skipped.
fn rank(status: &StepStatus) -> u8 {
    match status {
        StepStatus::Failed => 5,
        StepStatus::Skipped(Skip::Cancelled) => 4,
        StepStatus::Skipped(Skip::NotRun) => 3,
        StepStatus::Passed => 2,
        StepStatus::Skipped(_) => 1,
    }
}

fn status_text(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Passed => "passed",
        StepStatus::Failed => "FAILED",
        StepStatus::Skipped(Skip::NotRun | Skip::Cancelled) => "not run",
        StepStatus::Skipped(_) => "skipped",
    }
}

/// One step's reports from the parts, each named by its label, as one.
/// A single part's report is kept as it is.
pub(crate) fn merge(name: StepName, mut reports: Vec<(String, StepReport)>) -> StepReport {
    if reports.len() == 1 {
        return reports.pop().expect("one report").1;
    }
    let status = reports
        .iter()
        .map(|(_, r)| r.status.clone())
        .max_by_key(rank)
        .unwrap_or(StepStatus::Skipped(Skip::NotRun));
    let ran: Vec<&(String, StepReport)> = reports
        .iter()
        .filter(|(_, r)| matches!(r.status, StepStatus::Passed | StepStatus::Failed))
        .collect();
    let detail = ran
        .iter()
        .map(|(label, r)| match r.detail.is_empty() {
            true => format!("{label}: {}", status_text(&r.status)),
            false => format!("{label}: {}\n{}", status_text(&r.status), r.detail),
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let failed = status == StepStatus::Failed;
    StepReport {
        name,
        status,
        duration: reports.iter().map(|(_, r)| r.duration).sum(),
        detail: if ran.len() > 1 || failed {
            detail
        } else {
            ran.first()
                .map(|(_, r)| r.detail.clone())
                .unwrap_or_default()
        },
        logs: reports.into_iter().flat_map(|(_, r)| r.logs).collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn dirs() -> Vec<String> {
        vec![String::new(), "web".into(), "web/admin".into()]
    }

    #[test]
    fn a_path_is_scoped_to_the_deepest_part_only() {
        let dirs = dirs();
        assert_eq!(scoped(&dirs, 0, "src/lib.rs"), Some("src/lib.rs"));
        assert_eq!(scoped(&dirs, 0, "web/app.ts"), None);
        assert_eq!(scoped(&dirs, 1, "web/app.ts"), Some("app.ts"));
        assert_eq!(scoped(&dirs, 1, "web/admin/a.ts"), None);
        assert_eq!(scoped(&dirs, 2, "web/admin/a.ts"), Some("a.ts"));
        assert_eq!(
            scoped_paths(&dirs, 1, &["web/node_modules/".into(), "x".into()]),
            ["node_modules/"]
        );
    }

    #[test]
    fn the_repro_goes_to_the_part_holding_all_its_files() {
        let dirs = dirs();
        let repro = |test: &str, files: &[&str]| Repro {
            test: test.into(),
            files: files.iter().map(|f| (*f).to_owned()).collect(),
        };
        let (i, r) = repro_part(&dirs, &repro("web/t/a.test.ts", &["web/t/a.test.ts"]));
        assert_eq!((i, r.test.as_str()), (1, "t/a.test.ts"));
        assert_eq!(r.files, ["t/a.test.ts"]);
        // A test name that is not a path is kept.
        let (_, r) = repro_part(&dirs, &repro("refuses_fourth", &["web/t/a.test.ts"]));
        assert_eq!(r.test, "refuses_fourth");
        // Files of two parts stay with the root, unchanged.
        let both = repro("t", &["web/a.ts", "src/a.rs"]);
        assert_eq!(repro_part(&dirs, &both), (0, both.clone()));
    }

    #[test]
    fn declared_tests_are_named_as_the_part_runs_them() {
        let tests = vec![
            "web/t/a.test.ts::adds".to_owned(),
            "refuses_fourth".to_owned(),
        ];
        assert_eq!(
            scoped_tests("web", &tests),
            ["t/a.test.ts::adds", "refuses_fourth"]
        );
        assert_eq!(scoped_tests("", &tests), tests);
    }

    fn step(status: StepStatus, detail: &str) -> StepReport {
        StepReport {
            name: StepName::Test,
            status,
            duration: Duration::from_secs(1),
            detail: detail.into(),
            logs: vec!["a.log".into()],
        }
    }

    #[test]
    fn merged_steps_fail_when_a_part_fails_and_name_it() {
        let merged = merge(
            StepName::Test,
            vec![
                ("the repo root (Rust)".into(), step(StepStatus::Passed, "")),
                (
                    "`web/` (TypeScript)".into(),
                    step(StepStatus::Failed, "boom"),
                ),
            ],
        );
        assert_eq!(merged.status, StepStatus::Failed);
        assert_eq!(merged.duration, Duration::from_secs(2));
        assert_eq!(merged.logs.len(), 2);
        assert!(merged.detail.contains("`web/` (TypeScript): FAILED\nboom"));
        assert!(merged.detail.contains("the repo root (Rust): passed"));

        let skipped = |s| step(StepStatus::Skipped(s), "");
        let merged = merge(
            StepName::Typecheck,
            vec![
                ("a".into(), skipped(Skip::NotInProfile)),
                ("b".into(), step(StepStatus::Passed, "")),
            ],
        );
        assert_eq!(merged.status, StepStatus::Passed);
        let merged = merge(
            StepName::Coverage,
            vec![
                ("a".into(), step(StepStatus::Passed, "")),
                ("b".into(), skipped(Skip::NotRun)),
            ],
        );
        assert_eq!(merged.status, StepStatus::Skipped(Skip::NotRun));
    }
}
