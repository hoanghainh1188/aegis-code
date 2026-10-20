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
    #[serde(default)]
    packages: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    /// Real package name when the key is an npm alias.
    name: Option<String>,
    version: Option<String>,
    resolved: Option<String>,
    #[serde(default)]
    link: bool,
    /// Shipped inside its parent's tarball, which is checked instead.
    #[serde(default, rename = "inBundle")]
    in_bundle: bool,
}

fn invalid(message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: "package-lock.json".into(),
        message: message.into(),
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
    let mut out = Vec::new();
    for (key, e) in lock.packages {
        let err = |m: &str| invalid(format!("`{key}`: {m}"));
        if key.is_empty() {
            continue; // the root package
        }
        let Some((_, key_name)) = key.rsplit_once("node_modules/") else {
            // A workspace member, e.g. "packages/core".
            if in_repo(&key) {
                continue;
            }
            return Err(err("workspace path outside the repo"));
        };
        let name = e.name.clone().unwrap_or_else(|| key_name.to_owned());
        let version = e.version.clone().unwrap_or_default();
        if e.link {
            match e.resolved.as_deref() {
                Some(target) if in_repo(target) => continue,
                Some(target) => {
                    out.push(dep(name, version, format!("path+{target}")));
                    continue;
                }
                None => return Err(err("link without a target")),
            }
        }
        if e.in_bundle {
            continue;
        }
        let resolved = e
            .resolved
            .as_deref()
            .ok_or_else(|| err("no `resolved` URL"))?;
        let source = source_of(&name, resolved).map_err(|m| err(&m))?;
        let Some(source) = source else { continue };
        out.push(dep(name, version, source));
    }
    Ok(out)
}

fn dep(name: String, version: String, source: String) -> LockedDependency {
    LockedDependency {
        name,
        version,
        source,
    }
}

/// `None` for a `file:` path inside the repo.
fn source_of(name: &str, resolved: &str) -> Result<Option<String>, String> {
    if let Some(path) = resolved.strip_prefix("file:") {
        return Ok((!in_repo(path)).then(|| format!("path+{path}")));
    }
    if let Some(rest) = resolved.strip_prefix("git+") {
        return Ok(Some(format!("git+{rest}")));
    }
    if resolved.starts_with("git:") || resolved.starts_with("github:") {
        return Ok(Some(format!("git+{resolved}")));
    }
    let Some((scheme, rest)) = resolved.split_once("://") else {
        return Err(format!("cannot read `resolved` = `{resolved}`"));
    };
    let host_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..host_end]);
    let path = &rest[host_end..];
    if origin == NPM && !path.starts_with(&format!("/{name}/-/")) {
        // A registry tarball of another package.
        return Ok(Some(format!("url+{resolved}")));
    }
    Ok(Some(origin))
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
            "node_modules/x": {"version": "1.0.0", "resolved": "https://registry.npmjs.org/x/-/x-1.0.0.tgz"},
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
                "https://npm.evil.example",
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
}
