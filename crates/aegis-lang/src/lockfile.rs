//! Lockfile readers and the allow-list (ADR 0004 §3). Moved here from
//! `aegis-gate` (#12) so every profile goes through the same check.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::maven_tree::{self, GRADLE_TREE, MAVEN_TREE};
use crate::profile::glob_match;
use crate::{DependencyViolation, LangError, Language, LockedDependency, Profile};
use crate::{go_lock, maven_lock, npm_lock, nuget_lock, python_lock};
use crate::{pnpm_lock, yarn_lock};

/// MSBuild files that can add packages or package sources.
const MSBUILD_FILES: &[&str] = &[
    "*.csproj",
    "*.fsproj",
    "*.vbproj",
    "*.proj",
    "*.props",
    "*.targets",
];

/// Dependencies a patch may add, e.g. `policies/allowed-crates.toml`.
/// Dependencies already locked before the patch need no entry.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowList {
    allowed: BTreeSet<String>,
}

impl AllowList {
    pub fn from_toml(text: &str) -> Result<Self, LangError> {
        toml::from_str(text).map_err(|e| LangError::InvalidAllowList(e.to_string()))
    }

    pub fn names(&self) -> &BTreeSet<String> {
        &self.allowed
    }
}

/// External dependencies locked in `text`, the content of `file_name`.
pub fn read_lockfile(
    language: Language,
    file_name: &str,
    text: &str,
) -> Result<Vec<LockedDependency>, LangError> {
    match (language, file_name) {
        (Language::Rust, "Cargo.lock") => cargo_lock(text),
        (Language::Python, "uv.lock") => python_lock::uv_lock(text),
        (Language::Python, "poetry.lock") => python_lock::poetry_lock(text),
        (Language::Go, "go.mod") => go_lock::go_mod(text),
        (Language::TypeScript, "package-lock.json") => npm_lock::package_lock(text),
        (Language::TypeScript, "pnpm-lock.yaml") => pnpm_lock::pnpm_lock(text),
        (Language::TypeScript, "yarn.lock") => yarn_lock::yarn_lock(text),
        (Language::Java, "pom.xml") => maven_lock::pom_xml(text),
        (Language::Java, "extensions.xml") => maven_lock::maven_extensions(text),
        (Language::CSharp, "packages.lock.json") => nuget_lock::packages_lock(text),
        (Language::CSharp, name) if name.eq_ignore_ascii_case("nuget.config") => {
            nuget_lock::nuget_config(name, text)
        }
        (Language::CSharp, "global.json") => nuget_lock::global_json(text),
        (Language::CSharp, "dotnet-tools.json") => nuget_lock::dotnet_tools(text),
        (Language::CSharp, name) if MSBUILD_FILES.iter().any(|p| glob_match(p, name)) => {
            nuget_lock::msbuild_project(name, text)
        }
        (Language::Java, name @ ("gradle.lockfile" | "buildscript-gradle.lockfile")) => {
            maven_lock::gradle_lockfile(name, text)
        }
        (Language::Java, MAVEN_TREE) => maven_tree::maven_tree(text),
        (Language::Java, GRADLE_TREE) => maven_tree::gradle_dependencies(text),
        (Language::Python, name) if glob_match("requirements*.txt", name) => {
            python_lock::requirements(name, text)
        }
        _ => Err(LangError::UnsupportedLockfile {
            language,
            file: file_name.to_owned(),
        }),
    }
}

#[derive(Deserialize)]
struct CargoLock {
    #[serde(default)]
    package: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    /// `None` for workspace members and path dependencies.
    source: Option<String>,
}

fn cargo_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let lock: CargoLock = toml::from_str(text).map_err(|e| LangError::InvalidLockfile {
        file: "Cargo.lock".into(),
        message: e.to_string(),
    })?;
    Ok(lock
        .package
        .into_iter()
        .filter_map(|p| {
            Some(LockedDependency {
                source: p.source?,
                name: p.name,
                version: p.version,
            })
        })
        .collect())
}

impl Profile {
    /// Whether a patch changes dependencies, so the gate must check them
    /// and run `cargo deny` (or the profile's equivalent) before fetching.
    pub fn touches_dependencies<P: AsRef<Path>>(&self, changed: &[P]) -> bool {
        let deps = &self.dependencies;
        changed.iter().any(|path| {
            path.as_ref()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| {
                    deps.manifests.iter().chain(&deps.lockfiles).any(|pattern| {
                        glob_match(&pattern.to_ascii_lowercase(), &name.to_ascii_lowercase())
                    })
                })
        })
    }

    /// Dependencies a patch adds that the gate must reject. `before` is the
    /// lockfile on the base branch; `None` when the repo had none.
    pub fn dependency_violations(
        &self,
        lockfile: &str,
        before: Option<&str>,
        after: &str,
        allowed: &AllowList,
    ) -> Result<Vec<DependencyViolation>, LangError> {
        let read = |text: &str, which: &str| {
            read_lockfile(self.language, lockfile, text).map_err(|e| match e {
                LangError::InvalidLockfile { file, message } => LangError::InvalidLockfile {
                    file: format!("{file} ({which} the patch)"),
                    message,
                },
                other => other,
            })
        };
        let before = match before {
            Some(text) => read(text, "before")?,
            None => Vec::new(),
        };
        let after = read(after, "after")?;
        // Lockfile names are normalized; the allow-list must be too.
        let allowed: BTreeSet<String> = match self.language {
            Language::Python => allowed
                .names()
                .iter()
                .map(|n| python_lock::normalize_name(n))
                .collect(),
            _ => allowed.names().clone(),
        };
        Ok(crate::new_dependency_violations(
            &before,
            &after,
            &allowed,
            &self.dependencies.official_source,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Profiles, ViolationKind};

    const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

    fn rust() -> Profile {
        Profiles::builtin()
            .unwrap()
            .get(Language::Rust)
            .unwrap()
            .clone()
    }

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

    fn allow(names: &[&str]) -> AllowList {
        AllowList {
            allowed: names.iter().map(|n| n.to_string()).collect(),
        }
    }

    fn check(before: Option<&str>, after: &str, allowed: &AllowList) -> Vec<DependencyViolation> {
        rust()
            .dependency_violations("Cargo.lock", before, after, allowed)
            .unwrap()
    }

    #[test]
    fn rust_profile_uses_crates_io_as_official_source() {
        assert_eq!(rust().dependencies.official_source, CRATES_IO);
    }

    #[test]
    fn shipped_allow_list_parses_and_is_empty() {
        let text = include_str!("../../../policies/allowed-crates.toml");
        assert_eq!(AllowList::from_toml(text).unwrap(), AllowList::default());
        assert!(AllowList::from_toml("allow = []").is_err());
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
        let v = check(Some(&base()), &after, &AllowList::default());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::NotAllowed);
        assert_eq!(
            v[0].to_string(),
            "new dependency `left-pad@0.3.1` is not in the allow-list"
        );
    }

    #[test]
    fn allows_listed_crates_bumps_removals_and_workspace_members() {
        let after = lock(&[
            ("target-app", "0.1.0", None),
            ("target-utils", "0.1.0", None),
            ("serde", "1.0.229", Some(CRATES_IO)),
            ("thiserror", "2.0.0", Some(CRATES_IO)),
        ]);
        assert!(check(Some(&base()), &after, &allow(&["thiserror"])).is_empty());
    }

    #[test]
    fn git_forks_and_other_registries_are_blocked_even_if_listed() {
        let fork = "git+https://github.com/someone/serde?rev=bad#bad";
        let other = "registry+https://example.com/index";
        let after = lock(&[
            ("serde", "1.0.200", Some(fork)),
            ("anyhow", "1.0.80", Some(CRATES_IO)),
            ("private", "1.0.0", Some(other)),
        ]);
        let v = check(Some(&base()), &after, &allow(&["serde", "private"]));
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|v| v.kind == ViolationKind::UnofficialSource));
    }

    #[test]
    fn repo_without_lockfile_checks_every_crate() {
        let after = lock(&[("serde", "1.0.200", Some(CRATES_IO))]);
        assert_eq!(check(None, &after, &AllowList::default()).len(), 1);
        assert!(check(None, &after, &allow(&["serde"])).is_empty());
    }

    #[test]
    fn reads_this_workspace_lockfile() {
        let ours = read_lockfile(
            Language::Rust,
            "Cargo.lock",
            include_str!("../../../Cargo.lock"),
        )
        .unwrap();
        assert!(ours.iter().any(|d| d.name == "serde_json"));
        assert!(ours.iter().all(|d| !d.name.starts_with("aegis-")));
    }

    #[test]
    fn reports_bad_or_unknown_lockfiles() {
        let r = rust();
        assert!(matches!(
            r.dependency_violations(
                "Cargo.lock",
                Some("not = [toml"),
                &base(),
                &AllowList::default()
            ),
            Err(LangError::InvalidLockfile { .. })
        ));
        assert!(matches!(
            read_lockfile(Language::Rust, "package-lock.json", "{}"),
            Err(LangError::UnsupportedLockfile { .. })
        ));
    }

    #[test]
    fn says_which_lockfile_is_malformed() {
        let r = rust();
        let err = r
            .dependency_violations(
                "Cargo.lock",
                Some(&base()),
                "[[package]]\nname = 1\n",
                &AllowList::default(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("after the patch"), "{err}");
        let err = r
            .dependency_violations(
                "Cargo.lock",
                Some("not = [toml"),
                &base(),
                &AllowList::default(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("before the patch"), "{err}");
    }

    #[test]
    fn python_lockfiles_go_through_the_same_check() {
        let py = Profiles::builtin()
            .unwrap()
            .get(Language::Python)
            .unwrap()
            .clone();
        let before = "requests==2.32.3\n";
        let after = "requests==2.33.0\nLeft_Pad==1.0\nhttpx==0.27\n";
        let allowed = AllowList::from_toml("allowed = [\"HTTPX\"]").unwrap();
        let v = py
            .dependency_violations("requirements-dev.txt", Some(before), after, &allowed)
            .unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].dependency.name, "left-pad");
        assert!(matches!(
            read_lockfile(Language::Python, "Pipfile.lock", "{}"),
            Err(LangError::UnsupportedLockfile { .. })
        ));
    }

    #[test]
    fn detects_dependency_changes_from_profile_files() {
        let r = rust();
        assert!(r.touches_dependencies(&["crates/core/Cargo.toml"]));
        assert!(r.touches_dependencies(&["src/lib.rs", "Cargo.lock"]));
        assert!(!r.touches_dependencies(&["src/cargo.rs", "docs/Cargo.toml.md"]));
        assert!(!r.touches_dependencies::<&str>(&[]));
        let py = Profiles::builtin()
            .unwrap()
            .get(Language::Python)
            .unwrap()
            .clone();
        assert!(py.touches_dependencies(&["requirements-dev.txt"]));
    }
}
