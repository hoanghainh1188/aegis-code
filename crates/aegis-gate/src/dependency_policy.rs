//! What a patch may change in the dependencies (#500, #455). A bug-fix or
//! a review round keeps every locked version: a new package only from the
//! allow-list and the official source, as before, and no package bumped,
//! downgraded or removed. Dependency upgrades go through a CVE job, where
//! the platform itself raises one package (#501) and the patch must keep
//! the files it wrote.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DependencyPolicy {
    /// Bug-fix jobs and review rounds.
    #[default]
    Locked,
    /// A CVE job.
    Upgrade(Upgrade),
}

/// The platform's upgrade of one package for a CVE job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub package: String,
    pub version: String,
    /// Every manifest and lockfile the upgrade wrote, from the repo root,
    /// with the exact content it gave them.
    pub files: BTreeMap<String, String>,
}

/// Package names as the ecosystems compare them: case and `-`, `_`, `.`
/// do not matter (PEP 503; Cargo's `-`/`_`).
pub fn same_package(a: &str, b: &str) -> bool {
    let fold = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '_' | '.' => '-',
                c => c.to_ascii_lowercase(),
            })
            .collect()
    };
    fold(a) == fold(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_compare_as_the_registries_do() {
        assert!(same_package("Requests", "requests"));
        assert!(same_package("typing_extensions", "typing-extensions"));
        assert!(same_package("zope.interface", "zope-interface"));
        assert!(!same_package("requests", "request"));
    }
}
