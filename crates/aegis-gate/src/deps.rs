//! Step 0 of design §6.1: a patch that touches dependencies must only add
//! allowed packages from the official source. Fails closed: a protected
//! build-config path, a lockfile the reader cannot read, or a manifest
//! changed without its lockfile all fail the step. A manifest the resolved
//! dependency tree covers (#80) may change alone: the gate compares the
//! trees next.

use std::collections::BTreeSet;
use std::path::Path;

use aegis_lang::{Profile, glob_match, new_dependency_violations, read_lockfile};

use crate::GateError;
use crate::git::{self, Change};

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(crate) fn matches_any(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, file_name(path)))
}

/// Whether the build would read `path`: a manifest, a lockfile or a
/// protected path of the profile.
fn matters(profile: &Profile, path: &str) -> bool {
    let deps = &profile.dependencies;
    matches_any(&deps.manifests, path)
        || matches_any(&deps.lockfiles, path)
        || !profile.protected_changes(&[path]).is_empty()
}

/// `Ok(None)` when no manifest, lockfile or protected path changed;
/// `Ok(Some(Err(reason)))` when the step fails. `ignored` are the paths
/// git ignores in the patched tree. `tree` are the `on_change` patterns of
/// the dependency tree the gate resolves after this check, if it does.
pub fn check(
    profile: &Profile,
    repo: &Path,
    base: &str,
    changes: &[Change],
    ignored: &[String],
    allowed: &BTreeSet<String>,
    tree: &[String],
) -> Result<Option<Result<(), String>>, GateError> {
    let nested: Vec<&str> = changes
        .iter()
        .filter(|c| c.gitlink)
        .map(|c| c.path.as_str())
        .collect();
    if !nested.is_empty() {
        return Ok(Some(Err(format!(
            "the patch adds or changes a submodule or nested repository: {}",
            nested.join(", ")
        ))));
    }
    // An ignored lockfile is invisible to the diff but not to the build.
    let hidden: Vec<&str> = ignored
        .iter()
        .filter(|p| !p.ends_with('/') && matters(profile, p))
        .map(String::as_str)
        .collect();
    if !hidden.is_empty() {
        return Ok(Some(Err(format!(
            "git ignores files the build reads: {}",
            hidden.join(", ")
        ))));
    }
    let changed: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let changed = changed.as_slice();
    let protected = profile.protected_changes(changed);
    if !protected.is_empty() {
        return Ok(Some(Err(format!(
            "the patch changes protected build configuration: {}",
            protected.join(", ")
        ))));
    }
    let deps = &profile.dependencies;
    let lockfiles: Vec<&String> = changed
        .iter()
        .filter(|p| matches_any(&deps.lockfiles, p))
        .collect();
    let manifests: Vec<&String> = changed
        .iter()
        .filter(|p| matches_any(&deps.manifests, p) && !matches_any(&deps.lockfiles, p))
        .collect();
    if lockfiles.is_empty() && manifests.is_empty() {
        return Ok(None);
    }
    if lockfiles.is_empty() && manifests.iter().all(|p| matches_any(tree, p)) {
        return Ok(Some(Ok(())));
    }
    if lockfiles.is_empty() {
        return Ok(Some(Err(format!(
            "{} changed but no lockfile did, so the new dependencies cannot be checked",
            manifests
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))));
    }
    let mut problems = Vec::new();
    for path in lockfiles {
        let name = file_name(path);
        let Ok(after) = std::fs::read_to_string(repo.join(path)) else {
            problems.push(format!("{path}: removed or unreadable"));
            continue;
        };
        let read = |text: &str| read_lockfile(profile.language, name, text);
        // A lockfile the base does not have locks nothing yet.
        let before = match git::show(repo, base, path)? {
            Some(text) => read(&text),
            None => Ok(vec![]),
        };
        let (before, after) = match (before, read(&after)) {
            (Ok(b), Ok(a)) => (b, a),
            (Err(e), _) | (_, Err(e)) => {
                problems.push(format!("{path}: {e}"));
                continue;
            }
        };
        problems.extend(
            new_dependency_violations(&before, &after, allowed, &deps.official_source)
                .into_iter()
                .map(|v| format!("{path}: {v}")),
        );
    }
    Ok(Some(if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }))
}
