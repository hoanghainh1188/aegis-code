//! Dependencies a patch adds, for every ecosystem (ADR 0004 §3). Lockfile
//! readers turn each ecosystem's lockfile into [`LockedDependency`] (#64 for
//! `Cargo.lock`, #65 for Python); this module decides what to reject.

use std::collections::BTreeSet;
use std::fmt;

/// One external package locked by the repo. Workspace members and path
/// dependencies are code in the repo and are left out by the readers.
///
/// Readers normalize before comparison: `name` the way the ecosystem
/// compares names (PEP 503 for Python: lower-case, runs of `-_.` become
/// `-`), `source` to one spelling per registry (no trailing `/`).
/// Comparison here is exact and fails closed: an unnormalized official
/// source is reported as unofficial.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockedDependency {
    pub name: String,
    pub version: String,
    /// Where it comes from, as the reader normalizes it.
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// New package from the official registry, not in the allow-list.
    NotAllowed,
    /// New package from a git repository or another registry; never allowed.
    UnofficialSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyViolation {
    pub dependency: LockedDependency,
    pub kind: ViolationKind,
}

impl fmt::Display for DependencyViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let LockedDependency {
            name,
            version,
            source,
        } = &self.dependency;
        match self.kind {
            ViolationKind::NotAllowed => {
                write!(
                    f,
                    "new dependency `{name}@{version}` is not in the allow-list"
                )
            }
            ViolationKind::UnofficialSource => {
                write!(f, "new dependency `{name}@{version}` comes from `{source}`")
            }
        }
    }
}

/// Dependencies in `after` that the gate must reject. A dependency is new
/// when its name was not locked from the same source in `before`, so
/// pointing an existing package at a fork is caught; version bumps are not.
pub fn new_dependency_violations(
    before: &[LockedDependency],
    after: &[LockedDependency],
    allowed: &BTreeSet<String>,
    official_source: &str,
) -> Vec<DependencyViolation> {
    let known: BTreeSet<(&str, &str)> = before
        .iter()
        .map(|d| (d.name.as_str(), d.source.as_str()))
        .collect();
    after
        .iter()
        .filter(|d| !known.contains(&(d.name.as_str(), d.source.as_str())))
        .filter_map(|d| {
            let kind = if d.source != official_source {
                ViolationKind::UnofficialSource
            } else if allowed.contains(&d.name) {
                return None;
            } else {
                ViolationKind::NotAllowed
            };
            Some(DependencyViolation {
                dependency: d.clone(),
                kind,
            })
        })
        .collect()
}

/// A package already locked whose versions changed: bumped, downgraded,
/// or removed (`after` empty). Bug-fix patches change none (#500).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionChange {
    pub name: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

impl fmt::Display for VersionChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name;
        let before = self.before.join(", ");
        if self.after.is_empty() {
            write!(f, "`{name}` {before} is removed")
        } else {
            write!(f, "`{name}` {before} → {}", self.after.join(", "))
        }
    }
}

/// The packages of `before` whose set of locked versions differs in
/// `after`, by name, in name order. New packages are
/// [`new_dependency_violations`]' to judge.
pub fn version_changes(
    before: &[LockedDependency],
    after: &[LockedDependency],
) -> Vec<VersionChange> {
    let versions = |deps: &[LockedDependency], name: &str| -> Vec<String> {
        let set: BTreeSet<&str> = deps
            .iter()
            .filter(|d| d.name == name)
            .map(|d| d.version.as_str())
            .collect();
        set.into_iter().map(str::to_owned).collect()
    };
    let names: BTreeSet<&str> = before.iter().map(|d| d.name.as_str()).collect();
    names
        .into_iter()
        .filter_map(|name| {
            let (b, a) = (versions(before, name), versions(after, name));
            (b != a).then(|| VersionChange {
                name: name.to_owned(),
                before: b,
                after: a,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_changes_are_bumps_downgrades_and_removals_of_known_packages() {
        let before = [
            dep("requests", "2.32.0", PYPI),
            dep("idna", "3.7", PYPI),
            dep("serde", "1.0.1", PYPI),
            dep("serde", "1.0.2", PYPI),
            dep("gone", "1.0", PYPI),
        ];
        let after = [
            dep("requests", "2.32.4", PYPI),
            dep("idna", "3.7", PYPI),
            dep("serde", "1.0.2", PYPI),
            dep("serde", "1.0.1", PYPI),
            dep("new-one", "0.1", PYPI),
        ];
        let changes = version_changes(&before, &after);
        let text: Vec<String> = changes.iter().map(ToString::to_string).collect();
        assert_eq!(
            text,
            ["`gone` 1.0 is removed", "`requests` 2.32.0 → 2.32.4"]
        );
        assert!(version_changes(&before, &before).is_empty());
        // Two versions of one crate, one of them bumped.
        let bumped = [dep("serde", "1.0.1", PYPI), dep("serde", "1.0.3", PYPI)];
        assert_eq!(
            version_changes(&before[2..4], &bumped)[0].to_string(),
            "`serde` 1.0.1, 1.0.2 → 1.0.1, 1.0.3"
        );
    }

    const PYPI: &str = "https://pypi.org/simple";

    fn dep(name: &str, version: &str, source: &str) -> LockedDependency {
        LockedDependency {
            name: name.into(),
            version: version.into(),
            source: source.into(),
        }
    }

    fn base() -> Vec<LockedDependency> {
        vec![
            dep("requests", "2.32.0", PYPI),
            dep("urllib3", "2.2.0", PYPI),
        ]
    }

    fn allow(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn blocks_new_package_not_in_allow_list() {
        let after = [base(), vec![dep("leftpad", "1.0.0", PYPI)]].concat();
        let v = new_dependency_violations(&base(), &after, &allow(&[]), PYPI);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::NotAllowed);
        assert_eq!(
            v[0].to_string(),
            "new dependency `leftpad@1.0.0` is not in the allow-list"
        );
    }

    #[test]
    fn allows_listed_packages_and_version_bumps() {
        let after = vec![
            dep("requests", "2.33.0", PYPI),
            dep("httpx", "0.27.0", PYPI),
        ];
        assert!(new_dependency_violations(&base(), &after, &allow(&["httpx"]), PYPI).is_empty());
    }

    #[test]
    fn blocks_unofficial_sources_even_if_listed() {
        let fork = "git+https://github.com/someone/requests@bad";
        let after = vec![
            dep("requests", "2.32.0", fork),
            dep("urllib3", "2.2.0", PYPI),
        ];
        let v = new_dependency_violations(&base(), &after, &allow(&["requests"]), PYPI);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::UnofficialSource);
        assert!(v[0].to_string().contains(fork));
    }
}
