//! Blocks patches that add crates outside the allow-list (#12, design §7).
//!
//! Compares `Cargo.lock` before and after the patch. Crates already locked
//! before stay allowed, version bumps included; licenses and advisories of
//! the whole graph are `cargo deny`'s job with `policies/deny.toml`.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

/// `source` of a crates.io package in `Cargo.lock`.
pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Debug, Error)]
pub enum CratePolicyError {
    #[error("invalid allow-list: {0}")]
    AllowList(String),
    #[error("invalid Cargo.lock ({which}): {message}")]
    Lockfile {
        which: &'static str,
        message: String,
    },
}

/// `policies/allowed-crates.toml`: crates a patch may add.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedCrates {
    allowed: BTreeSet<String>,
}

impl AllowedCrates {
    pub fn from_toml(text: &str) -> Result<Self, CratePolicyError> {
        toml::from_str(text).map_err(|e| CratePolicyError::AllowList(e.to_string()))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.allowed.contains(name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// New crates.io crate not in the allow-list.
    NotAllowed,
    /// New crate from a git repository or another registry; never allowed.
    UnknownSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateViolation {
    pub name: String,
    pub version: String,
    pub source: String,
    pub kind: ViolationKind,
}

impl fmt::Display for CrateViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            name,
            version,
            source,
            kind,
        } = self;
        match kind {
            ViolationKind::NotAllowed => {
                write!(f, "new crate `{name}@{version}` is not in the allow-list")
            }
            ViolationKind::UnknownSource => {
                write!(f, "new crate `{name}@{version}` comes from `{source}`")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct Lockfile {
    #[serde(default)]
    package: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    /// `None` for workspace members and path dependencies: code in the
    /// repo itself, reviewed as part of the patch.
    source: Option<String>,
}

/// Packages that come from outside the repo (`source` set).
fn external_packages(
    text: &str,
    which: &'static str,
) -> Result<Vec<(LockedPackage, String)>, CratePolicyError> {
    let lock: Lockfile = toml::from_str(text).map_err(|e| CratePolicyError::Lockfile {
        which,
        message: e.to_string(),
    })?;
    Ok(lock
        .package
        .into_iter()
        .filter_map(|p| {
            let source = p.source.clone()?;
            Some((p, source))
        })
        .collect())
}

/// Crates added by a patch that the gate must reject. `before` is the
/// lockfile on the base branch; `None` when the repo had no `Cargo.lock`.
///
/// A crate is new when its name was not locked from the same source
/// before, so pointing an existing crate at a git fork is caught too.
pub fn new_crate_violations(
    before: Option<&str>,
    after: &str,
    allowed: &AllowedCrates,
) -> Result<Vec<CrateViolation>, CratePolicyError> {
    let known: BTreeSet<(String, String)> = match before {
        Some(text) => external_packages(text, "before")?
            .into_iter()
            .map(|(p, source)| (p.name, source))
            .collect(),
        None => BTreeSet::new(),
    };
    Ok(external_packages(after, "after")?
        .into_iter()
        .filter(|(p, source)| !known.contains(&(p.name.clone(), source.clone())))
        .filter_map(|(p, source)| {
            let kind = if source != CRATES_IO {
                ViolationKind::UnknownSource
            } else if allowed.contains(&p.name) {
                return None;
            } else {
                ViolationKind::NotAllowed
            };
            Some(CrateViolation {
                name: p.name,
                version: p.version,
                source,
                kind,
            })
        })
        .collect())
}

/// Whether the patch changes dependencies, so the gate must run
/// `cargo deny` and [`new_crate_violations`] (design §7).
pub fn touches_cargo_files<P: AsRef<Path>>(changed: &[P]) -> bool {
    changed.iter().any(|path| {
        path.as_ref()
            .file_name()
            .is_some_and(|name| name == "Cargo.toml" || name == "Cargo.lock")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED_ALLOW_LIST: &str = include_str!("../../../policies/allowed-crates.toml");

    fn lock(packages: &[(&str, &str, Option<&str>)]) -> String {
        let body: String = packages
            .iter()
            .map(|(name, version, source)| {
                let source = source.map_or(String::new(), |s| format!("source = \"{s}\"\n"));
                format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n{source}\n")
            })
            .collect();
        format!("version = 4\n\n{body}")
    }

    fn base() -> String {
        lock(&[
            ("target-app", "0.1.0", None),
            ("serde", "1.0.200", Some(CRATES_IO)),
            ("anyhow", "1.0.80", Some(CRATES_IO)),
        ])
    }

    fn allow(names: &[&str]) -> AllowedCrates {
        AllowedCrates {
            allowed: names.iter().map(|n| n.to_string()).collect(),
        }
    }

    #[test]
    fn shipped_allow_list_parses_and_is_empty() {
        assert_eq!(
            AllowedCrates::from_toml(SHIPPED_ALLOW_LIST).unwrap(),
            AllowedCrates::default()
        );
    }

    // "Xong khi" of #12: a patch adding an unknown crate is blocked.
    #[test]
    fn blocks_a_patch_adding_an_unknown_crate() {
        let after = lock(&[
            ("target-app", "0.1.0", None),
            ("serde", "1.0.200", Some(CRATES_IO)),
            ("anyhow", "1.0.80", Some(CRATES_IO)),
            ("left-pad", "0.3.1", Some(CRATES_IO)),
        ]);
        let violations =
            new_crate_violations(Some(&base()), &after, &AllowedCrates::default()).unwrap();
        assert_eq!(
            violations,
            vec![CrateViolation {
                name: "left-pad".into(),
                version: "0.3.1".into(),
                source: CRATES_IO.into(),
                kind: ViolationKind::NotAllowed,
            }]
        );
        assert_eq!(
            violations[0].to_string(),
            "new crate `left-pad@0.3.1` is not in the allow-list"
        );
    }

    #[test]
    fn allows_new_crates_in_the_allow_list() {
        let after = lock(&[
            ("serde", "1.0.200", Some(CRATES_IO)),
            ("anyhow", "1.0.80", Some(CRATES_IO)),
            ("thiserror", "2.0.0", Some(CRATES_IO)),
        ]);
        let violations =
            new_crate_violations(Some(&base()), &after, &allow(&["thiserror"])).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn version_bumps_and_removals_are_not_violations() {
        let after = lock(&[
            ("target-app", "0.1.0", None),
            ("serde", "1.0.229", Some(CRATES_IO)),
            ("serde", "1.0.200", Some(CRATES_IO)),
        ]);
        let violations =
            new_crate_violations(Some(&base()), &after, &AllowedCrates::default()).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn new_workspace_crates_and_path_dependencies_are_not_violations() {
        let after = format!(
            "{}{}",
            base(),
            "[[package]]\nname = \"target-utils\"\nversion = \"0.1.0\"\n"
        );
        let violations =
            new_crate_violations(Some(&base()), &after, &AllowedCrates::default()).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn git_and_other_registries_are_blocked_even_if_listed() {
        let git = "git+https://github.com/someone/fork?rev=abc#abc";
        let other = "registry+https://example.com/index";
        let after = lock(&[
            ("serde", "1.0.200", Some(CRATES_IO)),
            ("anyhow", "1.0.80", Some(CRATES_IO)),
            ("forked", "0.1.0", Some(git)),
            ("private", "1.0.0", Some(other)),
        ]);
        let violations =
            new_crate_violations(Some(&base()), &after, &allow(&["forked", "private"])).unwrap();
        assert_eq!(violations.len(), 2);
        assert!(
            violations
                .iter()
                .all(|v| v.kind == ViolationKind::UnknownSource)
        );
        assert!(
            violations[0]
                .to_string()
                .contains("git+https://github.com/someone/fork")
        );
    }

    #[test]
    fn redirecting_an_existing_crate_to_git_is_blocked() {
        let fork = "git+https://github.com/someone/serde?rev=bad#bad";
        let after = lock(&[
            ("serde", "1.0.200", Some(fork)),
            ("anyhow", "1.0.80", Some(CRATES_IO)),
        ]);
        let violations = new_crate_violations(Some(&base()), &after, &allow(&["serde"])).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].name, "serde");
        assert_eq!(violations[0].kind, ViolationKind::UnknownSource);
    }

    #[test]
    fn repo_without_lockfile_checks_every_crate() {
        let after = lock(&[("serde", "1.0.200", Some(CRATES_IO))]);
        let blocked = new_crate_violations(None, &after, &AllowedCrates::default()).unwrap();
        assert_eq!(blocked.len(), 1);
        assert!(
            new_crate_violations(None, &after, &allow(&["serde"]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reports_bad_inputs() {
        assert!(matches!(
            new_crate_violations(Some("not = [toml"), &base(), &AllowedCrates::default()),
            Err(CratePolicyError::Lockfile {
                which: "before",
                ..
            })
        ));
        assert!(matches!(
            new_crate_violations(
                Some(&base()),
                "[[package]]\nname = 1\n",
                &AllowedCrates::default()
            ),
            Err(CratePolicyError::Lockfile { which: "after", .. })
        ));
        assert!(AllowedCrates::from_toml("allow = []").is_err());
    }

    #[test]
    fn reads_this_workspace_lockfile() {
        let ours = include_str!("../../../Cargo.lock");
        assert!(
            new_crate_violations(Some(ours), ours, &AllowedCrates::default())
                .unwrap()
                .is_empty()
        );
        let added = new_crate_violations(None, ours, &AllowedCrates::default()).unwrap();
        assert!(added.iter().any(|v| v.name == "serde_json"));
        assert!(added.iter().all(|v| !v.name.starts_with("aegis-")));
    }

    #[test]
    fn detects_dependency_changes() {
        assert!(touches_cargo_files(&["crates/core/Cargo.toml"]));
        assert!(touches_cargo_files(&["src/lib.rs", "Cargo.lock"]));
        assert!(!touches_cargo_files(&[
            "src/cargo.rs",
            "docs/Cargo.toml.md"
        ]));
        assert!(!touches_cargo_files::<&str>(&[]));
    }
}
