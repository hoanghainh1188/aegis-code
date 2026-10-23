//! Resolved Java dependency trees (#80): what `mvn dependency:tree` and
//! `gradle dependencies` print, read as [`LockedDependency`] so the gate
//! compares them like a lockfile.
//!
//! Maven has no lockfile and Gradle may not lock, so `pom.xml` alone shows
//! only direct dependencies: a patch bumping an allowed dependency can pull
//! in transitive artifacts nobody listed. The profile's `dependencies.tree`
//! command writes the resolved tree on the base and on the patch; both run
//! in the fetch container (#66). Modules of the build itself are code in
//! the repo and left out. Coordinates carry no repository, so every one
//! gets Maven Central: extra repositories are already refused in `pom.xml`
//! (see `maven_lock`). Fails closed on any line it cannot read.

use std::collections::BTreeSet;

use crate::maven_lock::MAVEN_CENTRAL;
use crate::{LangError, LockedDependency};

pub(crate) const MAVEN_TREE: &str = "maven-dependency-tree.txt";
pub(crate) const GRADLE_TREE: &str = "gradle-dependencies.txt";

const MAVEN_SCOPES: &[&str] = &["compile", "provided", "runtime", "test", "system", "import"];

fn invalid(file: &str, message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: file.to_owned(),
        message: message.into(),
    }
}

fn plain(part: &str) -> bool {
    !part.is_empty() && !part.contains(char::is_whitespace)
}

fn locked(deps: BTreeSet<(String, String)>) -> Vec<LockedDependency> {
    deps.into_iter()
        .map(|(name, version)| LockedDependency {
            name,
            version,
            source: MAVEN_CENTRAL.to_owned(),
        })
        .collect()
}

/// Splits the drawing in front of a tree entry: `indent` blocks of
/// `width` characters, then the entry's own marker.
fn tree_entry<'a>(
    line: &'a str,
    width: usize,
    indents: &[&str],
    markers: &[&str],
) -> Option<&'a str> {
    let mut rest = line;
    loop {
        if let Some(entry) = markers.iter().find_map(|m| rest.strip_prefix(m)) {
            return Some(entry);
        }
        let block = rest.get(..width)?;
        if !indents.contains(&block) {
            return None;
        }
        rest = &rest[width..];
    }
}

/// `dependency:tree` output, one tree per module (`-DappendOutput=true`):
/// a module's `group:artifact:packaging[:classifier]:version` at the left
/// margin, then its dependencies as
/// `+- group:artifact:type[:classifier]:version:scope [(optional)]`.
pub(crate) fn maven_tree(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let mut modules = BTreeSet::new();
    let mut deps = BTreeSet::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let err = || invalid(MAVEN_TREE, format!("line {}: cannot read `{line}`", i + 1));
        let Some(entry) = tree_entry(line, 3, &["|  ", "   "], &["+- ", "\\- "]) else {
            // A module of the build.
            let parts: Vec<&str> = line.split(':').collect();
            if !(4..=5).contains(&parts.len()) || !parts.iter().all(|p| plain(p)) {
                return Err(err());
            }
            modules.insert(format!("{}:{}", parts[0], parts[1]));
            continue;
        };
        if modules.is_empty() {
            return Err(err());
        }
        let coordinate = entry.strip_suffix(" (optional)").unwrap_or(entry);
        let parts: Vec<&str> = coordinate.split(':').collect();
        let (version, scope) = match parts.as_slice() {
            [_, _, _, version, scope] | [_, _, _, _, version, scope] => (version, scope),
            _ => return Err(err()),
        };
        if !parts.iter().all(|p| plain(p)) || !MAVEN_SCOPES.contains(scope) {
            return Err(err());
        }
        deps.insert((format!("{}:{}", parts[0], parts[1]), (*version).to_owned()));
    }
    if modules.is_empty() {
        return Err(invalid(MAVEN_TREE, "Maven wrote no dependency tree"));
    }
    deps.retain(|(name, _)| !modules.contains(name));
    Ok(locked(deps))
}

/// `gradle dependencies` output of one or more projects: entries as
/// `+--- group:artifact:version`, `group:artifact[:requested] -> version`
/// once Gradle picks another version, `project ':core'` for the build's
/// own projects, `(c)` for constraints and `(*)` / `(n)` for repeated and
/// unresolved entries. A `FAILED` entry is an error: the tree is unknown.
pub(crate) fn gradle_dependencies(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let mut projects = 0;
    let mut deps = BTreeSet::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.starts_with("Root project ") || line.starts_with("Project ") {
            projects += 1;
            continue;
        }
        if !line.starts_with(['+', '\\', '|', ' ']) || line.trim().is_empty() {
            // Configuration names, `No dependencies`, the legend.
            continue;
        }
        let err = || invalid(GRADLE_TREE, format!("line {}: cannot read `{line}`", i + 1));
        let entry =
            tree_entry(line, 5, &["|    ", "     "], &["+--- ", "\\--- "]).ok_or_else(err)?;
        if let Some(dep) = gradle_entry(entry).ok_or_else(err)? {
            deps.insert(dep);
        }
    }
    if projects == 0 {
        return Err(invalid(GRADLE_TREE, "Gradle reported no project"));
    }
    Ok(locked(deps))
}

/// `Some(None)` for entries that are no dependency of their own: a project
/// of the build, a constraint.
fn gradle_entry(entry: &str) -> Option<Option<(String, String)>> {
    let mut entry = entry;
    loop {
        if let Some(rest) = entry.strip_suffix(" (*)").or(entry.strip_suffix(" (n)")) {
            entry = rest;
        } else if entry.ends_with(" (c)") {
            return Some(None);
        } else {
            break;
        }
    }
    if entry.starts_with("project ") {
        return Some(None);
    }
    let (declared, selected) = match entry.split_once(" -> ") {
        Some((d, s)) => (d, Some(s)),
        None => (entry, None),
    };
    if selected.is_some_and(|s| s.starts_with("project ")) {
        return Some(None);
    }
    let mut parts = declared.splitn(3, ':');
    let (group, artifact) = (parts.next()?, parts.next()?);
    // `{strictly 1.0}` and other rich versions hold spaces.
    let version = match (parts.next(), selected) {
        (_, Some(v)) => v,
        (Some(v), None) if !v.is_empty() && !v.ends_with(" FAILED") => v,
        _ => return None,
    };
    (plain(group) && plain(artifact) && plain(version))
        .then(|| Some((format!("{group}:{artifact}"), version.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(deps: &[LockedDependency]) -> Vec<(&str, &str)> {
        deps.iter()
            .map(|d| (d.name.as_str(), d.version.as_str()))
            .collect()
    }

    // As Maven 3.9 with maven-dependency-plugin 3.8.1 writes it.
    const MULTI_MODULE: &str = "ex:parent:pom:1.0
ex:core:jar:1.0
+- com.google.guava:guava:jar:33.0.0-jre:compile (optional)
|  +- com.google.guava:failureaccess:jar:1.0.2:compile (optional)
|  \\- com.google.j2objc:j2objc-annotations:jar:2.8:compile (optional)
\\- net.java.dev.jna:jna:jar:jpms:5.14.0:compile
ex:app:jar:1.0
\\- ex:core:jar:1.0:compile
   \\- net.java.dev.jna:jna:jar:jpms:5.14.0:compile
";

    #[test]
    fn reads_every_module_and_leaves_out_the_builds_own() {
        let deps = maven_tree(MULTI_MODULE).unwrap();
        assert_eq!(
            pairs(&deps),
            vec![
                ("com.google.guava:failureaccess", "1.0.2"),
                ("com.google.guava:guava", "33.0.0-jre"),
                ("com.google.j2objc:j2objc-annotations", "2.8"),
                ("net.java.dev.jna:jna", "5.14.0"),
            ]
        );
        assert!(deps.iter().all(|d| d.source == MAVEN_CENTRAL));
    }

    #[test]
    fn a_module_without_dependencies_is_fine() {
        assert!(maven_tree("ex:parent:pom:1.0\n").unwrap().is_empty());
    }

    #[test]
    fn maven_tree_fails_closed() {
        let err = |t: &str| maven_tree(t).unwrap_err().to_string();
        assert!(err("").contains("no dependency tree"));
        assert!(
            err("+- a:b:jar:1:compile\n").contains("line 1"),
            "before any module"
        );
        let root = "ex:app:jar:1.0\n";
        for bad in [
            // Verbose output, which the profile does not ask for.
            "+- a:b:jar:1:compile (version managed from 2)",
            "+- (a:b:jar:1:compile - omitted for duplicate)",
            "+- a:b:1:compile",
            "+- a:b:jar:1:everywhere",
            "+- a::jar:1:compile",
            "-- a:b:jar:1:compile",
            "|- a:b:jar:1:compile",
            "[INFO] ex:app:jar:1.0",
        ] {
            assert!(
                err(&format!("{root}{bad}\n")).contains("line 2"),
                "{bad} must be refused"
            );
        }
    }

    // As Gradle 9 prints `gradle -q dependencies`.
    const GRADLE: &str = "
------------------------------------------------------------
Root project 'demo'
------------------------------------------------------------

compileClasspath - Compile classpath for source set 'main'.
No dependencies

testRuntimeClasspath - Runtime classpath of source set 'test'.
+--- project ':core'
|    \\--- org.checkerframework:checker-qual:3.30.0 -> 3.41.0
+--- com.google.guava:guava:33.0.0-jre
|    +--- com.google.code.findbugs:jsr305:3.0.2 -> 3.0.1
|    \\--- org.checkerframework:checker-qual:3.41.0 (*)
+--- org.slf4j:slf4j-api -> 2.0.16
+--- com.google.code.findbugs:jsr305:{strictly 3.0.1} -> 3.0.1
+--- org.slf4j:slf4j-api:2.0.16 (c)
+--- org.example:local:1.0 -> project :local
\\--- org.example:unresolved:1.2 (n)

(c) - A dependency constraint, not a dependency. The dependency affected by the constraint occurs elsewhere in the tree.
(*) - Indicates repeated occurrences of a transitive dependency subtree.
";

    #[test]
    fn reads_gradle_dependencies() {
        let deps = gradle_dependencies(GRADLE).unwrap();
        assert_eq!(
            pairs(&deps),
            vec![
                ("com.google.code.findbugs:jsr305", "3.0.1"),
                ("com.google.guava:guava", "33.0.0-jre"),
                ("org.checkerframework:checker-qual", "3.41.0"),
                ("org.example:unresolved", "1.2"),
                ("org.slf4j:slf4j-api", "2.0.16"),
            ]
        );
    }

    #[test]
    fn gradle_dependencies_fail_closed() {
        let err = |t: &str| gradle_dependencies(t).unwrap_err().to_string();
        assert!(err("No dependencies\n").contains("no project"));
        let head = "Root project 'demo'\n\nruntimeClasspath\n";
        for bad in [
            "\\--- nonexistent.group:nope:1.0 FAILED",
            "+--- org.slf4j:slf4j-api",
            "+--- guava",
            "+--- a:b:1 (selected by rule)",
            "|   \\--- a:b:1",
            "+-- a:b:1",
        ] {
            assert!(
                err(&format!("{head}{bad}\n")).contains("line 4"),
                "{bad} must be refused"
            );
        }
    }
}
