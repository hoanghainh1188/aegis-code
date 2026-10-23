//! `pnpm-lock.yaml` reader (#78), lockfile version 9 (pnpm 9 and 10).
//! Same rules as `package-lock.json`: the source is where pnpm really
//! downloads from (`resolution`), package keys name the real package even
//! for aliases, a registry tarball of another package is not the package it
//! claims to be, and anything unexpected is an error.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::npm_lock::{NPM, dep, is_npm_tarball, local_source, split_name, valid_name};
use crate::python_lock::in_repo;
use crate::{LangError, LockedDependency};

#[derive(Deserialize)]
struct PnpmLock {
    #[serde(rename = "lockfileVersion")]
    version: String,
    importers: Option<BTreeMap<String, Importer>>,
    #[serde(default)]
    packages: BTreeMap<String, Package>,
}

/// One project of the workspace, keyed by its directory (`.` for the root).
#[derive(Deserialize)]
struct Importer {
    #[serde(default)]
    dependencies: BTreeMap<String, ImporterDep>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, ImporterDep>,
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: BTreeMap<String, ImporterDep>,
}

#[derive(Deserialize)]
struct ImporterDep {
    /// A key into `packages` (minus the name), or `link:<dir>`.
    version: String,
}

#[derive(Deserialize)]
struct Package {
    resolution: Resolution,
    /// Set when the key holds a URL or path instead of the version.
    version: Option<String>,
}

#[derive(Deserialize)]
struct Resolution {
    integrity: Option<String>,
    tarball: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    directory: Option<String>,
    repo: Option<String>,
    commit: Option<String>,
}

pub(crate) fn invalid(file: &str, message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: file.to_owned(),
        message: message.into(),
    }
}

/// YAML options for lockfiles: generated lockfiles use no anchors, aliases
/// or merge keys, so any of them is an error rather than something to expand.
pub(crate) fn parse_yaml<'de, T: Deserialize<'de>>(
    file: &str,
    text: &'de str,
) -> Result<T, LangError> {
    let options = serde_saphyr::options! {
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        budget: serde_saphyr::budget! { max_aliases: 0, max_anchors: 0 },
        with_snippet: false,
    };
    serde_saphyr::from_str_with_options(text, options).map_err(|e| invalid(file, e.to_string()))
}

pub(crate) fn pnpm_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let invalid = |m: String| invalid("pnpm-lock.yaml", m);
    let lock: PnpmLock = parse_yaml("pnpm-lock.yaml", text)?;
    if lock.version != "9.0" {
        return Err(invalid(format!(
            "lockfileVersion {} is not supported; regenerate with pnpm 9 or later",
            lock.version
        )));
    }
    let importers = lock
        .importers
        .ok_or_else(|| invalid("no `importers` map".into()))?;
    let mut out = Vec::new();
    for (dir, importer) in &importers {
        if !in_repo(dir) {
            return Err(invalid(format!("importer `{dir}` is outside the repo")));
        }
        let deps = importer
            .dependencies
            .iter()
            .chain(&importer.dev_dependencies)
            .chain(&importer.optional_dependencies);
        for (name, d) in deps {
            // Everything else points into `packages`, read below.
            let Some(target) = d.version.strip_prefix("link:") else {
                continue;
            };
            if !valid_name(name) {
                return Err(invalid(format!("`{name}` is not an npm package name")));
            }
            if let Some(source) = local_source(Some(dir), target) {
                out.push(dep(name.clone(), String::new(), source));
            }
        }
    }
    for (key, package) in &lock.packages {
        let err = |m: &str| invalid(format!("`{key}`: {m}"));
        let (name, spec) = split_name(key).ok_or_else(|| err("not `name@version`"))?;
        if !valid_name(name) {
            return Err(err(&format!("`{name}` is not an npm package name")));
        }
        let version = package.version.as_deref().unwrap_or(spec);
        let source = source_of(name, spec, version, &package.resolution).map_err(|m| err(&m))?;
        if let Some(source) = source {
            out.push(dep(name.to_owned(), version.to_owned(), source));
        }
    }
    Ok(out)
}

/// A version as pnpm writes it in a registry package key.
fn plain_version(spec: &str) -> bool {
    !spec.is_empty()
        && spec
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-.+_".contains(&b))
}

/// `None` for a directory inside the repo.
fn source_of(
    name: &str,
    spec: &str,
    version: &str,
    r: &Resolution,
) -> Result<Option<String>, String> {
    match (r.kind.as_deref(), &r.tarball, &r.directory) {
        (None, None, None) => {
            // Integrity only: the registry tarball of `name@spec`.
            if r.integrity.is_none() {
                return Err("empty `resolution`".into());
            }
            if !plain_version(spec) {
                return Err(format!("registry resolution for `{spec}`"));
            }
            Ok(Some(NPM.to_owned()))
        }
        (None, Some(tarball), None) => {
            if let Some(path) = tarball.strip_prefix("file:") {
                // A packed tarball is a third-party package, even in the repo.
                return Ok(Some(format!("path+{path}")));
            }
            if !tarball.contains("://") {
                return Err(format!("cannot read `tarball` = `{tarball}`"));
            }
            if is_npm_tarball(name, version, tarball) {
                return Ok(Some(NPM.to_owned()));
            }
            Ok(Some(format!("url+{tarball}")))
        }
        (Some("git"), None, None) => match (&r.repo, &r.commit) {
            (Some(repo), Some(commit)) => Ok(Some(format!("git+{repo}#{commit}"))),
            _ => Err("git resolution without `repo` and `commit`".into()),
        },
        (Some("directory"), None, Some(dir)) => Ok(local_source(Some("."), dir)),
        _ => Err("unknown `resolution`".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(importer: &str, packages: &str) -> String {
        format!("lockfileVersion: '9.0'\n\nimporters:\n  .:\n{importer}\npackages:\n{packages}\n")
    }

    fn deps(packages: &str) -> Vec<LockedDependency> {
        pnpm_lock(&lock("    {}", packages)).unwrap()
    }

    fn sources(d: &[LockedDependency]) -> Vec<(&str, &str)> {
        d.iter()
            .map(|d| (d.name.as_str(), d.source.as_str()))
            .collect()
    }

    #[test]
    fn reads_registry_packages() {
        let d = deps(
            "  lodash@4.17.21:\n    resolution: {integrity: sha512-x}\n\
             \x20 '@types/node@22.0.0':\n    resolution: {integrity: sha512-y}\n    engines: {node: '>=18'}\n",
        );
        assert_eq!(sources(&d), vec![("@types/node", NPM), ("lodash", NPM)]);
        assert_eq!(d[0].version, "22.0.0");
    }

    #[test]
    fn registry_tarball_urls_are_checked_against_the_package() {
        let d = deps(
            "  lodash@4.17.21:\n    resolution: {tarball: https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz}\n\
             \x20 left-pad@1.3.0:\n    resolution: {tarball: https://registry.npmjs.org/evil/-/evil-1.0.0.tgz}\n",
        );
        assert_eq!(
            sources(&d),
            vec![
                (
                    "left-pad",
                    "url+https://registry.npmjs.org/evil/-/evil-1.0.0.tgz"
                ),
                ("lodash", NPM)
            ]
        );
    }

    #[test]
    fn git_tarballs_and_outside_paths_change_the_source() {
        let d = deps(
            "  is-number@https://codeload.github.com/x/is-number/tar.gz/98e8ff1:\n    \
             resolution: {tarball: https://codeload.github.com/x/is-number/tar.gz/98e8ff1}\n    version: 7.0.0\n\
             \x20 b@git+https://github.com/x/b.git#abc:\n    resolution: {commit: abc, repo: https://github.com/x/b.git, type: git}\n    version: 1.0.0\n\
             \x20 c@file:../outside/c:\n    resolution: {directory: ../outside/c, type: directory}\n    version: 1.0.0\n\
             \x20 d@file:vendor/d-1.0.0.tgz:\n    resolution: {integrity: sha512-z, tarball: file:vendor/d-1.0.0.tgz}\n    version: 1.0.0\n",
        );
        assert_eq!(
            sources(&d),
            vec![
                ("b", "git+https://github.com/x/b.git#abc"),
                ("c", "path+../outside/c"),
                ("d", "path+vendor/d-1.0.0.tgz"),
                (
                    "is-number",
                    "url+https://codeload.github.com/x/is-number/tar.gz/98e8ff1"
                ),
            ]
        );
        assert_eq!(d[3].version, "7.0.0");
    }

    #[test]
    fn repo_directories_and_links_are_repo_code() {
        let importer = "    dependencies:\n      core:\n        specifier: workspace:*\n        version: link:packages/core\n\
                        \x20     out:\n        specifier: link:../out\n        version: link:../out\n\
                        \x20     ms:\n        specifier: npm:ms@2.1.3\n        version: ms@2.1.3\n";
        let packages = "  local@file:vendor/local:\n    resolution: {directory: vendor/local, type: directory}\n\
                        \x20 ms@2.1.3:\n    resolution: {integrity: sha512-m}\n";
        let d = pnpm_lock(&lock(importer, packages)).unwrap();
        assert_eq!(sources(&d), vec![("out", "path+../out"), ("ms", NPM)]);
        let nested = "lockfileVersion: '9.0'\nimporters:\n  packages/app:\n    dependencies:\n      core:\n        \
                      specifier: workspace:*\n        version: link:../core\n";
        assert!(pnpm_lock(nested).unwrap().is_empty());
    }

    #[test]
    fn fails_closed() {
        let err = |text: &str| pnpm_lock(text).unwrap_err().to_string();
        assert!(err("lockfileVersion: '6.0'\nimporters: {}\n").contains("lockfileVersion 6.0"));
        assert!(err("lockfileVersion: '9.0'\npackages: {}\n").contains("no `importers`"));
        assert!(err(&lock("    {}", "  x@1.0.0:\n    resolution: {}\n")).contains("empty"));
        assert!(
            err(&lock(
                "    {}",
                "  x@1.0.0:\n    resolution: {type: variations, variants: []}\n"
            ))
            .contains("unknown `resolution`")
        );
        assert!(
            err(&lock(
                "    {}",
                "  x@https://e.example/x.tgz:\n    resolution: {integrity: sha512-x}\n"
            ))
            .contains("registry resolution")
        );
        assert!(
            err(&lock(
                "    {}",
                "  Up@1.0.0:\n    resolution: {integrity: sha512-x}\n"
            ))
            .contains("not an npm package name")
        );
        assert!(
            err("lockfileVersion: '9.0'\nimporters:\n  ../x: {}\n").contains("outside the repo")
        );
        assert!(err("not: [yaml").contains("pnpm-lock.yaml"));
    }

    #[test]
    fn duplicate_keys_and_aliases_are_refused() {
        let dup = lock(
            "    {}",
            "  x@1.0.0:\n    resolution: {integrity: a}\n  x@1.0.0:\n    resolution: {tarball: https://e.example/x.tgz}\n",
        );
        assert!(pnpm_lock(&dup).is_err());
        let alias = lock(
            "    {}",
            "  x@1.0.0:\n    resolution: &r {tarball: https://e.example/x.tgz}\n  y@1.0.0:\n    resolution: *r\n",
        );
        assert!(pnpm_lock(&alias).is_err());
    }
}
