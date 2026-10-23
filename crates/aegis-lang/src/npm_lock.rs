//! `package-lock.json` reader (#67), lockfile versions 2 and 3 (npm 7+).
//! Fails closed like the Python readers: the source is where npm really
//! downloads from (`resolved`), npm aliases report the real package, and a
//! registry tarball of another package is not the package it claims to be.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::python_lock::in_repo;
use crate::{LangError, LockedDependency};

/// The public npm registry.
pub(crate) const NPM: &str = "https://registry.npmjs.org";

#[derive(Deserialize)]
struct PackageLock {
    #[serde(rename = "lockfileVersion")]
    version: u32,
    /// Required: npm falls back to the legacy `dependencies` map when it is
    /// missing, which this reader does not read.
    packages: Option<BTreeMap<String, Entry>>,
}

#[derive(Deserialize)]
struct Entry {
    /// Real package name when the key is an npm alias.
    name: Option<String>,
    version: Option<String>,
    resolved: Option<String>,
    #[serde(default)]
    link: bool,
    /// Shipped inside its parent's tarball. Trusted only when the parent
    /// lists it in `bundleDependencies`; npm itself ignores the flag.
    #[serde(default, rename = "inBundle")]
    in_bundle: bool,
    #[serde(default, rename = "bundleDependencies")]
    bundle_dependencies: Option<serde_json::Value>,
}

fn invalid(message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: "package-lock.json".into(),
        message: message.into(),
    }
}

/// npm package names: optional `@scope/`, then lower-case URL-safe text.
pub(crate) fn valid_name(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-._~".contains(&b))
    };
    match name.strip_prefix('@').map(|rest| rest.split_once('/')) {
        Some(Some((scope, pkg))) => part(scope) && part(pkg),
        Some(None) => false,
        None => part(name),
    }
}

/// Key of the entry whose `node_modules` holds `key`'s package
/// (`""` for the root).
fn parent_key(key: &str) -> &str {
    key.rsplit_once("/node_modules/")
        .map_or("", |(parent, _)| parent)
}

fn bundles(parent: &Entry, name: &str) -> bool {
    match &parent.bundle_dependencies {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::Array(names)) => names.iter().any(|n| n.as_str() == Some(name)),
        _ => false,
    }
}

pub(crate) fn package_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let lock: PackageLock = serde_json::from_str(text).map_err(|e| invalid(e.to_string()))?;
    if !(2..=3).contains(&lock.version) {
        return Err(invalid(format!(
            "lockfileVersion {} is not supported; regenerate with npm 7 or later",
            lock.version
        )));
    }
    let packages = lock
        .packages
        .ok_or_else(|| invalid("no `packages` map; npm would read the legacy `dependencies`"))?;
    let mut out = Vec::new();
    for (key, e) in &packages {
        let err = |m: &str| invalid(format!("`{key}`: {m}"));
        if key.is_empty() {
            continue; // the root package
        }
        let Some((_, key_name)) = key.rsplit_once("node_modules/") else {
            // A workspace member, e.g. "packages/core": code in the repo,
            // as long as it does not come from somewhere else.
            let local = e.resolved.as_deref().is_none_or(in_repo);
            if in_repo(key) && local {
                continue;
            }
            return Err(err("workspace entry outside the repo"));
        };
        let name = e.name.clone().unwrap_or_else(|| key_name.to_owned());
        if !valid_name(&name) {
            return Err(err(&format!("`{name}` is not an npm package name")));
        }
        let version = e.version.clone().unwrap_or_default();
        if e.link {
            match e.resolved.as_deref() {
                Some(target) if in_repo(target) && !target.contains("node_modules") => continue,
                Some(target) => {
                    out.push(dep(name, version, format!("path+{target}")));
                    continue;
                }
                None => return Err(err("link without a target")),
            }
        }
        if e.in_bundle {
            match packages.get(parent_key(key)) {
                Some(parent) if bundles(parent, &name) => continue,
                _ => return Err(err("`inBundle` but the parent does not bundle it")),
            }
        }
        let resolved = e
            .resolved
            .as_deref()
            .ok_or_else(|| err("no `resolved` URL"))?;
        let source = source_of(&name, &version, resolved).map_err(|m| err(&m))?;
        let Some(source) = source else { continue };
        out.push(dep(name, version, source));
    }
    Ok(out)
}

pub(crate) fn dep(name: String, version: String, source: String) -> LockedDependency {
    LockedDependency {
        name,
        version,
        source,
    }
}

/// Whether `resolved` is exactly npm's tarball URL for `name@version`:
/// `https://registry.npmjs.org/<name>/-/<base>-<version>.tgz`, with no
/// encoding, dot segments, backslashes, query or fragment that a URL parser
/// would turn into another path.
pub(crate) fn is_npm_tarball(name: &str, version: &str, resolved: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    let expected = format!("{NPM}/{name}/-/{base}-{version}.tgz");
    !version.is_empty() && resolved == expected && !resolved.contains(['%', '\\', '?', '#'])
}

fn is_archive(path: &str) -> bool {
    [".tgz", ".tar.gz", ".tar", ".zip"]
        .iter()
        .any(|ext| path.to_ascii_lowercase().ends_with(ext))
}

/// `rel` resolved against `base`, both relative to the repo root, when the
/// result stays inside the repo (`"."` for the root itself).
pub(crate) fn join_in_repo(base: &str, rel: &str) -> Option<String> {
    let odd = |p: &str| p.starts_with(['/', '~']) || p.contains(['\\', ':']);
    if odd(base) || odd(rel) {
        return None;
    }
    let mut parts = Vec::new();
    for seg in base.split('/').chain(rel.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    })
}

/// Source of a local package at `path`, relative to the repo directory
/// `base`. `None` (repo code) for a directory inside the repo that is not
/// under `node_modules`; a packed tarball or anything outside the repo is a
/// third-party package.
pub(crate) fn local_source(base: Option<&str>, path: &str) -> Option<String> {
    match base.and_then(|b| join_in_repo(b, path)) {
        Some(p) if !is_archive(path) && !p.split('/').any(|s| s == "node_modules") => None,
        _ => Some(format!("path+{path}")),
    }
}

/// Splits `name@spec`, where a scoped name starts with `@`.
pub(crate) fn split_name(text: &str) -> Option<(&str, &str)> {
    let from = usize::from(text.starts_with('@'));
    let at = text[from..].find('@')? + from;
    Some((&text[..at], &text[at + 1..]))
}

/// `None` for a `file:` directory inside the repo.
fn source_of(name: &str, version: &str, resolved: &str) -> Result<Option<String>, String> {
    if let Some(path) = resolved.strip_prefix("file:") {
        // A packed tarball is a third-party package, even inside the repo.
        return Ok((is_archive(path) || !in_repo(path)).then(|| format!("path+{path}")));
    }
    if let Some(rest) = resolved.strip_prefix("git+") {
        return Ok(Some(format!("git+{rest}")));
    }
    if resolved.starts_with("git:") || resolved.starts_with("github:") {
        return Ok(Some(format!("git+{resolved}")));
    }
    if !resolved.contains("://") {
        return Err(format!("cannot read `resolved` = `{resolved}`"));
    }
    if is_npm_tarball(name, version, resolved) {
        return Ok(Some(NPM.to_owned()));
    }
    // Anything else, on npm or another host, is pinned by its full URL: a
    // different repo, commit or file on the same host is a different source.
    Ok(Some(format!("url+{resolved}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(packages: &str) -> String {
        format!(
            r#"{{"name": "app", "lockfileVersion": 3, "packages": {{"": {{"name": "app"}}{packages}}}}}"#
        )
    }

    fn deps(packages: &str) -> Vec<LockedDependency> {
        package_lock(&lock(packages)).unwrap()
    }

    #[test]
    fn reads_registry_packages() {
        let d = deps(
            r#",
            "node_modules/lodash": {"version": "4.17.21", "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz"},
            "node_modules/@types/node": {"version": "22.0.0", "resolved": "https://registry.npmjs.org/@types/node/-/node-22.0.0.tgz"},
            "node_modules/a/node_modules/b": {"version": "1.0.0", "resolved": "https://registry.npmjs.org/b/-/b-1.0.0.tgz"}"#,
        );
        let names: Vec<_> = d
            .iter()
            .map(|d| (d.name.as_str(), d.source.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("@types/node", NPM), ("b", NPM), ("lodash", NPM)]
        );
    }

    #[test]
    fn skips_root_workspaces_in_repo_links_and_bundled() {
        let d = deps(
            r#",
            "packages/core": {"name": "core", "version": "1.0.0"},
            "node_modules/core": {"resolved": "packages/core", "link": true},
            "node_modules/x": {"version": "1.0.0", "resolved": "https://registry.npmjs.org/x/-/x-1.0.0.tgz", "bundleDependencies": ["y"]},
            "node_modules/x/node_modules/y": {"version": "1.0.0", "inBundle": true},
            "node_modules/local": {"version": "1.0.0", "resolved": "file:vendor/local"}"#,
        );
        assert_eq!(
            d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["x"]
        );
    }

    #[test]
    fn aliases_report_the_real_package() {
        let d = deps(
            r#",
            "node_modules/lodash": {"name": "evil-lodash", "version": "1.0.0", "resolved": "https://registry.npmjs.org/evil-lodash/-/evil-lodash-1.0.0.tgz"}"#,
        );
        assert_eq!(d[0].name, "evil-lodash");
        assert_eq!(d[0].source, NPM);
    }

    #[test]
    fn tarball_of_another_package_is_not_official() {
        let d = deps(
            r#",
            "node_modules/lodash": {"version": "4.17.21", "resolved": "https://registry.npmjs.org/evil/-/evil-1.0.0.tgz"}"#,
        );
        assert_eq!(
            d[0].source,
            "url+https://registry.npmjs.org/evil/-/evil-1.0.0.tgz"
        );
    }

    #[test]
    fn other_registries_git_and_outside_paths_change_the_source() {
        let d = deps(
            r#",
            "node_modules/a": {"version": "1.0.0", "resolved": "https://npm.evil.example/a/-/a-1.0.0.tgz"},
            "node_modules/b": {"version": "1.0.0", "resolved": "git+ssh://git@github.com/someone/b.git#abc"},
            "node_modules/c": {"version": "1.0.0", "resolved": "file:../outside/c"},
            "node_modules/d": {"resolved": "../../elsewhere", "link": true}"#,
        );
        let sources: Vec<_> = d.iter().map(|d| d.source.as_str()).collect();
        assert_eq!(
            sources,
            vec![
                "url+https://npm.evil.example/a/-/a-1.0.0.tgz",
                "git+ssh://git@github.com/someone/b.git#abc",
                "path+../outside/c",
                "path+../../elsewhere"
            ]
        );
    }

    #[test]
    fn fails_closed() {
        let err = |text: &str| package_lock(text).unwrap_err().to_string();
        assert!(err(r#"{"lockfileVersion": 1, "dependencies": {}}"#).contains("lockfileVersion 1"));
        assert!(
            err(&lock(r#", "node_modules/x": {"version": "1.0.0"}"#)).contains("no `resolved`")
        );
        assert!(err(&lock(r#", "../escape": {"version": "1"}"#)).contains("outside the repo"));
        assert!(err(&lock(r#", "node_modules/x": {"resolved": "weird"}"#)).contains("cannot read"));
        assert!(err("not json").contains("package-lock.json"));
    }

    #[test]
    fn forged_in_bundle_is_refused() {
        let err = package_lock(&lock(
            r#", "node_modules/evil": {"version": "1.0.0", "resolved": "https://registry.npmjs.org/evil/-/evil-1.0.0.tgz", "inBundle": true}"#,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("does not bundle it"), "{err}");
    }

    #[test]
    fn tarball_path_tricks_are_not_official() {
        for resolved in [
            "https://registry.npmjs.org/lodash/-/%2e%2e/%2e%2e/evil/-/evil-1.0.0.tgz",
            "https://registry.npmjs.org/lodash/-/../../evil/-/evil-1.0.0.tgz",
            "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz?x=1",
            "https://registry.npmjs.org/lodash/-/lodash-9.9.9.tgz",
            "https://REGISTRY.npmjs.org/lodash/-/lodash-4.17.21.tgz",
        ] {
            let d = deps(&format!(
                r#", "node_modules/lodash": {{"version": "4.17.21", "resolved": "{resolved}"}}"#
            ));
            assert_eq!(d[0].source, format!("url+{resolved}"), "{resolved}");
        }
    }

    #[test]
    fn missing_packages_map_and_bad_names_are_refused() {
        let err = |t: &str| package_lock(t).unwrap_err().to_string();
        assert!(
            err(r#"{"lockfileVersion": 2, "dependencies": {"evil": {"version": "1.0.0"}}}"#)
                .contains("no `packages`")
        );
        assert!(err(&lock(r#", "node_modules/a/b": {"version": "1", "resolved": "https://registry.npmjs.org/a/b/-/b-1.tgz"}"#)).contains("not an npm package name"));
        assert!(err(&lock(r#", "node_modules/Up": {"version": "1", "resolved": "https://registry.npmjs.org/Up/-/Up-1.tgz"}"#)).contains("not an npm package name"));
    }

    #[test]
    fn packed_tarballs_and_remote_workspaces_are_not_repo_code() {
        let d = deps(
            r#", "node_modules/evil": {"version": "1.0.0", "resolved": "file:vendor/evil-1.0.0.tgz"}"#,
        );
        assert_eq!(d[0].source, "path+vendor/evil-1.0.0.tgz");
        let err = package_lock(&lock(
            r#", "packages/core": {"version": "1", "resolved": "https://evil.example/core.tgz"}"#,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("workspace entry outside the repo"));
        let d = deps(r#", "node_modules/x": {"resolved": "node_modules/evil", "link": true}"#);
        assert_eq!(d[0].source, "path+node_modules/evil");
    }
}
