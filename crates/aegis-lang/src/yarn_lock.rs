//! `yarn.lock` reader (#78): Yarn 1 (its own line format) and Yarn Berry
//! (YAML, `__metadata` versions 4 to 8). Same rules as `package-lock.json`:
//! the source is where yarn really downloads from, aliases report the real
//! package, a registry tarball of another package is not the package it
//! claims to be, and anything unexpected is an error.
//!
//! Yarn 1 downloads from `registry.yarnpkg.com` by default, which serves
//! the npm registry; its tarballs count as official like npm's.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::npm_lock::{
    NPM, dep, is_npm_tarball, join_in_repo, local_source, split_name, valid_name,
};
use crate::pnpm_lock::{invalid, parse_yaml};
use crate::{LangError, LockedDependency};

const FILE: &str = "yarn.lock";
/// Yarn's mirror of the npm registry.
const YARNPKG: &str = "https://registry.yarnpkg.com";

pub(crate) fn yarn_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    if text.lines().any(|l| l.trim_end() == "__metadata:") {
        berry(text)
    } else if text.lines().any(|l| l.trim_end() == "# yarn lockfile v1") {
        classic(text).map_err(|m| invalid(FILE, m))
    } else {
        Err(invalid(FILE, "neither a Yarn 1 nor a Yarn Berry lockfile"))
    }
}

/// Real package name of an `npm:` alias range, or `name` itself. A target
/// without `@` is a range unless it looks like a package name.
fn real_name<'a>(name: &'a str, range: &'a str) -> &'a str {
    let Some(target) = range.strip_prefix("npm:") else {
        return name;
    };
    match split_name(target) {
        Some((real, _)) => real,
        None if valid_name(target) && !target.starts_with(|c: char| c.is_ascii_digit()) => target,
        None => name,
    }
}

// ----------------------------------------------------------------- Yarn 1

#[derive(Default)]
struct ClassicEntry {
    patterns: Vec<String>,
    version: Option<String>,
    resolved: Option<String>,
}

/// One scalar at the start of `s`: a JSON-quoted string, or bare text up to
/// a space or comma.
fn scalar(s: &str) -> Result<(String, &str), String> {
    if s.starts_with('"') {
        let mut escaped = false;
        for (i, c) in s.char_indices().skip(1) {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    let value = serde_json::from_str(&s[..=i]).map_err(|e| e.to_string())?;
                    return Ok((value, &s[i + 1..]));
                }
                _ => escaped = false,
            }
        }
        return Err(format!("unterminated string `{s}`"));
    }
    let end = s.find([' ', ',']).unwrap_or(s.len());
    if end == 0 {
        return Err(format!("empty value in `{s}`"));
    }
    Ok((s[..end].to_owned(), &s[end..]))
}

/// `"a@^1", a@^2:` into its patterns.
fn patterns(line: &str) -> Result<Vec<String>, String> {
    let mut rest = line
        .strip_suffix(':')
        .ok_or_else(|| format!("expected `pattern:` at `{line}`"))?;
    let mut out = Vec::new();
    loop {
        let (pattern, after) = scalar(rest)?;
        out.push(pattern);
        if after.is_empty() {
            return Ok(out);
        }
        rest = after
            .strip_prefix(", ")
            .ok_or_else(|| format!("expected `, ` in `{line}`"))?;
    }
}

fn classic_entries(text: &str) -> Result<Vec<ClassicEntry>, String> {
    let mut entries: Vec<ClassicEntry> = Vec::new();
    let mut in_block = false;
    for (n, raw) in text.lines().enumerate() {
        let at = |m: String| format!("line {}: {m}", n + 1);
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let body = line.trim_start_matches(' ');
        if body.starts_with('\t') {
            return Err(at("tab indentation".into()));
        }
        match line.len() - body.len() {
            0 => {
                entries.push(ClassicEntry {
                    patterns: patterns(body).map_err(at)?,
                    ..ClassicEntry::default()
                });
                in_block = false;
            }
            2 => {
                let entry = entries
                    .last_mut()
                    .ok_or_else(|| at("field outside an entry".into()))?;
                let (key, rest) = match body.strip_suffix(':') {
                    Some(key) if !key.contains([' ', '"']) => (key.to_owned(), ":"),
                    _ => scalar(body).map_err(at)?,
                };
                if rest == ":" {
                    in_block = true; // dependencies:, optionalDependencies:
                    continue;
                }
                in_block = false;
                let value = rest
                    .strip_prefix(' ')
                    .ok_or_else(|| at(format!("expected `{key} value`")))?;
                let (value, tail) = scalar(value).map_err(at)?;
                if !tail.is_empty() {
                    return Err(at(format!("unexpected `{tail}`")));
                }
                let slot = match key.as_str() {
                    "version" => &mut entry.version,
                    "resolved" => &mut entry.resolved,
                    _ => continue,
                };
                if slot.replace(value).is_some() {
                    return Err(at(format!("`{key}` given twice")));
                }
            }
            4 if in_block => {}
            _ => return Err(at("unexpected indentation".into())),
        }
    }
    Ok(entries)
}

fn classic(text: &str) -> Result<Vec<LockedDependency>, String> {
    let mut out = Vec::new();
    for entry in classic_entries(text)? {
        let first = entry.patterns.first().cloned().unwrap_or_default();
        let err = |m: &str| format!("`{first}`: {m}");
        let mut names = BTreeSet::new();
        let mut ranges = Vec::new();
        for pattern in &entry.patterns {
            let (name, range) = split_name(pattern).ok_or_else(|| err("not `name@range`"))?;
            names.insert(real_name(name, range));
            ranges.push(range);
        }
        let name = match names.into_iter().collect::<Vec<_>>()[..] {
            [name] if valid_name(name) => name.to_owned(),
            [name] => return Err(err(&format!("`{name}` is not an npm package name"))),
            _ => return Err(err("patterns name different packages")),
        };
        let version = entry.version.ok_or_else(|| err("no `version`"))?;
        match entry.resolved {
            Some(resolved) => {
                let source = classic_source(&name, &version, &resolved).map_err(|m| err(&m))?;
                out.extend(source.map(|s| dep(name.clone(), version.clone(), s)));
            }
            // Yarn 1 writes no `resolved` for local directories.
            None => {
                for range in ranges {
                    let path = range
                        .strip_prefix("file:")
                        .or_else(|| range.strip_prefix("link:"))
                        .ok_or_else(|| err("no `resolved` URL"))?;
                    out.extend(
                        local_source(Some("."), path)
                            .map(|s| dep(name.clone(), version.clone(), s)),
                    );
                }
            }
        }
    }
    Ok(out)
}

/// Yarn 1 appends `#<sha1>` to registry tarball URLs.
fn without_sha1(url: &str) -> &str {
    match url.rsplit_once('#') {
        Some((base, hash)) if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            base
        }
        _ => url,
    }
}

fn classic_source(name: &str, version: &str, resolved: &str) -> Result<Option<String>, String> {
    if let Some(path) = resolved.strip_prefix("file:") {
        return Ok(local_source(Some("."), without_sha1(path)));
    }
    if resolved.starts_with("git+") {
        return Ok(Some(resolved.to_owned()));
    }
    if resolved.starts_with("git:") || resolved.starts_with("github:") {
        return Ok(Some(format!("git+{resolved}")));
    }
    if !resolved.contains("://") {
        return Err(format!("cannot read `resolved` = `{resolved}`"));
    }
    let url = without_sha1(resolved);
    let on_yarnpkg = url
        .strip_prefix(YARNPKG)
        .is_some_and(|rest| is_npm_tarball(name, version, &format!("{NPM}{rest}")));
    if on_yarnpkg || is_npm_tarball(name, version, url) {
        return Ok(Some(NPM.to_owned()));
    }
    Ok(Some(format!("url+{resolved}")))
}

// ------------------------------------------------------------- Yarn Berry

#[derive(Deserialize)]
struct BerryEntry {
    /// `__metadata` only.
    #[serde(rename = "cacheKey")]
    _cache_key: Option<String>,
    version: Option<String>,
    resolution: Option<String>,
}

fn berry(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let mut lock: BTreeMap<String, BerryEntry> = parse_yaml(FILE, text)?;
    let metadata = lock
        .remove("__metadata")
        .and_then(|m| m.version)
        .ok_or_else(|| invalid(FILE, "`__metadata` without `version`"))?;
    if !matches!(metadata.parse::<u32>(), Ok(4..=8)) {
        return Err(invalid(
            FILE,
            format!("__metadata version {metadata} is not supported; regenerate with Yarn 3 or 4"),
        ));
    }
    let mut out = BTreeSet::new();
    for (key, entry) in &lock {
        let err = |m: &str| invalid(FILE, format!("`{key}`: {m}"));
        let (Some(version), Some(resolution)) = (&entry.version, &entry.resolution) else {
            return Err(err("no `version` or `resolution`"));
        };
        if let Some(d) = berry_dep(resolution, version).map_err(|m| err(&m))? {
            out.insert(d);
        }
    }
    Ok(out.into_iter().collect())
}

fn percent_decode(text: &str) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'%' {
            let hex = tail
                .get(..2)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| format!("bad escape in `{text}`"))?;
            bytes.push(hex);
            rest = &tail[2..];
        } else {
            bytes.push(b);
            rest = tail;
        }
    }
    String::from_utf8(bytes).map_err(|_| format!("bad escape in `{text}`"))
}

/// `key=value&...` after `::` in a locator.
fn params(text: Option<&str>) -> Result<BTreeMap<&str, String>, String> {
    text.into_iter()
        .flat_map(|t| t.split('&'))
        .map(|kv| {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| format!("bad parameter `{kv}`"))?;
            Ok((k, percent_decode(v)?))
        })
        .collect()
}

/// Repo directory of the workspace in a `locator=` parameter; `None` when
/// the path is relative to a package that is not a workspace.
fn workspace_dir(locator: Option<&String>) -> Option<String> {
    let (_, range) = split_name(locator?)?;
    let dir = range.strip_prefix("workspace:")?;
    join_in_repo(".", dir)
}

fn is_builtin_patch(spec: &str) -> bool {
    let spec = spec.strip_prefix("optional!").unwrap_or(spec);
    spec.starts_with("builtin<") && spec.ends_with('>')
}

/// `None` for workspaces and other code in the repo.
fn berry_dep(resolution: &str, version: &str) -> Result<Option<LockedDependency>, String> {
    let (name, range) = split_name(resolution).ok_or("resolution is not `name@range`")?;
    if !valid_name(name) {
        return Err(format!("`{name}` is not an npm package name"));
    }
    let (range, params_text) = match range.split_once("::") {
        Some((r, p)) => (r, Some(p)),
        None => (range, None),
    };
    let params = params(params_text)?;
    let base = workspace_dir(params.get("locator"));
    let found = |source: String| Some(dep(name.to_owned(), version.to_owned(), source));
    let (protocol, rest) = range.split_once(':').unwrap_or((range, ""));
    let local = |path: &str| local_source(base.as_deref(), path).and_then(found);
    Ok(match protocol {
        "npm" => match params.get("__archiveUrl") {
            Some(url) => found(format!("url+{url}")),
            None if params.is_empty() && rest == version => found(NPM.to_owned()),
            None => return Err(format!("unexpected npm resolution `{resolution}`")),
        },
        "workspace" => match join_in_repo(".", rest) {
            Some(_) => None,
            None => return Err("workspace outside the repo".into()),
        },
        "link" | "portal" => local(rest),
        // `file:./dir#./dir`: the path, then the same path as a selector.
        "file" => local(rest.split_once('#').map_or(rest, |(p, _)| p)),
        "patch" => {
            let (inner, patches) = rest.split_once('#').ok_or("patch without a patch file")?;
            let inner = percent_decode(inner)?;
            let patches = percent_decode(patches)?;
            let outside = patches.split('&').find(|p| {
                let p = p.strip_prefix("~/").unwrap_or(p);
                !is_builtin_patch(p) && local_source(base.as_deref().or(Some(".")), p).is_some()
            });
            match outside {
                Some(p) => found(format!("path+{p}")),
                // The patched package itself.
                None => berry_dep(&inner, version)?,
            }
        }
        // `virtual:<hash>#<range>`: a copy of `range` for its peers.
        "virtual" => {
            let (_, inner) = rest.split_once('#').ok_or("virtual without a range")?;
            berry_dep(&format!("{name}@{inner}"), version)?
        }
        "git" | "git+ssh" | "git+https" | "github" => found(format!("git+{range}")),
        "http" | "https" if rest.contains(".git#") || range.contains("#commit=") => {
            found(format!("git+{range}"))
        }
        "http" | "https" if is_npm_tarball(name, version, range) => found(NPM.to_owned()),
        "http" | "https" => found(format!("url+{range}")),
        _ => return Err(format!("unknown resolution `{resolution}`")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = "# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.\n# yarn lockfile v1\n\n\n";

    fn v1(body: &str) -> Vec<LockedDependency> {
        yarn_lock(&format!("{V1}{body}")).unwrap()
    }

    fn sources(d: &[LockedDependency]) -> Vec<(&str, &str)> {
        d.iter()
            .map(|d| (d.name.as_str(), d.source.as_str()))
            .collect()
    }

    const BERRY: &str = "__metadata:\n  version: 8\n  cacheKey: 10c0\n\n";

    fn berry(body: &str) -> Vec<LockedDependency> {
        yarn_lock(&format!("{BERRY}{body}")).unwrap()
    }

    fn entry(key: &str, version: &str, resolution: &str) -> String {
        format!(
            "\"{key}\":\n  version: {version}\n  resolution: \"{resolution}\"\n  languageName: node\n  linkType: hard\n\n"
        )
    }

    #[test]
    fn v1_reads_registry_packages_on_both_hosts() {
        let d = v1(r#""@babel/code-frame@^7.0.0", "@babel/code-frame@^7.10.4":
  version "7.12.13"
  resolved "https://registry.yarnpkg.com/@babel/code-frame/-/code-frame-7.12.13.tgz#dcfc826beef65e75c50e21d3837d7d95798dd658"
  integrity sha512-x
  dependencies:
    "@babel/highlight" "^7.12.13"

lodash@^4.17.21:
  version "4.17.21"
  resolved "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz"
"#);
        assert_eq!(
            sources(&d),
            vec![("@babel/code-frame", NPM), ("lodash", NPM)]
        );
        assert_eq!(d[0].version, "7.12.13");
    }

    #[test]
    fn v1_aliases_report_the_real_package() {
        let d = v1(r#""my-ms@npm:ms@2.1.3":
  version "2.1.3"
  resolved "https://registry.yarnpkg.com/ms/-/ms-2.1.3.tgz#574c8138ce1d2b5861f0b44579dbadd60c6615b2"
"#);
        assert_eq!(sources(&d), vec![("ms", NPM)]);
    }

    #[test]
    fn v1_tarball_of_another_package_and_other_hosts_are_not_official() {
        let d = v1(r#"lodash@^4.17.21:
  version "4.17.21"
  resolved "https://registry.yarnpkg.com/evil/-/evil-1.0.0.tgz#574c8138ce1d2b5861f0b44579dbadd60c6615b2"

"is-number@github:x/is-number#98e8ff1":
  version "7.0.0"
  resolved "https://codeload.github.com/x/is-number/tar.gz/98e8ff1da1a89f93d1397a24d7413ed15421c139"

b@git+ssh://git@github.com/x/b.git:
  version "1.0.0"
  resolved "git+ssh://git@github.com/x/b.git#abc"

lodash@1.0.0:
  version "1.0.0"
  resolved "https://registry.yarnpkg.com/lodash/-/../../evil/-/evil-1.0.0.tgz"
"#);
        let s = sources(&d);
        assert_eq!(
            s[0].1,
            "url+https://registry.yarnpkg.com/evil/-/evil-1.0.0.tgz#574c8138ce1d2b5861f0b44579dbadd60c6615b2"
        );
        assert!(s[1].1.starts_with("url+https://codeload.github.com/"));
        assert_eq!(s[2].1, "git+ssh://git@github.com/x/b.git#abc");
        assert!(s[3].1.starts_with("url+"), "{s:?}");
    }

    #[test]
    fn v1_local_directories_are_repo_code() {
        let d = v1(r#""local@file:./vendor/local":
  version "1.0.0"

"linked@link:./vendor/linked":
  version "0.0.0"
  uid ""

"out@file:../outside":
  version "1.0.0"

"packed@file:vendor/packed-1.0.0.tgz":
  version "1.0.0"
  resolved "file:vendor/packed-1.0.0.tgz#574c8138ce1d2b5861f0b44579dbadd60c6615b2"
"#);
        assert_eq!(
            sources(&d),
            vec![
                ("out", "path+../outside"),
                ("packed", "path+vendor/packed-1.0.0.tgz")
            ]
        );
    }

    #[test]
    fn v1_fails_closed() {
        let err = |body: &str| yarn_lock(&format!("{V1}{body}")).unwrap_err().to_string();
        assert!(err("x@^1:\n  version \"1.0.0\"\n").contains("no `resolved`"));
        assert!(err("x@^1:\n  resolved \"https://e.example/x.tgz\"\n").contains("no `version`"));
        assert!(err("x@^1:\n  version \"1\"\n  version \"2\"\n").contains("twice"));
        assert!(err("x@^1:\n    version \"1\"\n").contains("indentation"));
        assert!(err("x@^1:\n\tversion \"1\"\n").contains("line 6"));
        assert!(
            err("x@^1, y@^1:\n  version \"1\"\n  resolved \"https://e.example/x.tgz\"\n")
                .contains("different packages")
        );
        assert!(
            err("Up@^1:\n  version \"1\"\n  resolved \"https://e.example/x.tgz\"\n")
                .contains("not an npm package name")
        );
        assert!(err("x@^1:\n  version \"1\"\n  resolved \"weird\"\n").contains("cannot read"));
        assert!(err("\"x@^1:\n").contains("unterminated"));
        assert!(
            yarn_lock("x@^1:\n  version \"1\"\n")
                .unwrap_err()
                .to_string()
                .contains("neither")
        );
    }

    #[test]
    fn berry_reads_registry_aliases_and_patches() {
        let d = berry(&[
            entry("ms@npm:^2.1.3, my-ms@npm:ms@2.1.3", "2.1.3", "ms@npm:2.1.3"),
            entry("@types/node@npm:^22", "22.0.0", "@types/node@npm:22.0.0"),
            entry("typescript@npm:^5", "5.9.3", "typescript@npm:5.9.3"),
            entry(
                "typescript@patch:typescript@npm%3A^5#optional!builtin<compat/typescript>",
                "5.9.3",
                "typescript@patch:typescript@npm%3A5.9.3#optional!builtin<compat/typescript>::version=5.9.3&hash=5786d5",
            ),
            entry("app@workspace:.", "0.0.0-use.local", "app@workspace:."),
            entry("core@workspace:packages/core", "0.0.0-use.local", "core@workspace:packages/core"),
        ]
        .concat());
        assert_eq!(
            sources(&d),
            vec![("@types/node", NPM), ("ms", NPM), ("typescript", NPM)]
        );
    }

    #[test]
    fn berry_other_sources_change_the_source() {
        let d = berry(&[
            entry("a@npm:1.0.0", "1.0.0", "a@npm:1.0.0::__archiveUrl=https%3A%2F%2Fe.example%2Fa.tgz"),
            entry("b@https://github.com/x/b.git#abc", "1.0.0", "b@https://github.com/x/b.git#commit=abc"),
            entry("c@https://e.example/c.tgz", "1.0.0", "c@https://e.example/c.tgz"),
            entry("d@portal:../out::locator=app%40workspace%3A.", "0.0.0-use.local", "d@portal:../out::locator=app%40workspace%3A."),
            entry("e@file:./vendor/e::locator=app%40workspace%3A.", "1.0.0", "e@file:./vendor/e#./vendor/e::hash=1&locator=app%40workspace%3A."),
            entry("f@link:../f::locator=core%40workspace%3Apackages%2Fcore", "0.0.0-use.local", "f@link:../f::locator=core%40workspace%3Apackages%2Fcore"),
            entry("g@file:./g.tgz::locator=app%40workspace%3A.", "1.0.0", "g@file:./g.tgz#./g.tgz::hash=1&locator=app%40workspace%3A."),
            entry("h@patch:h@npm%3A1.0.0#../evil.patch::locator=app%40workspace%3A.", "1.0.0", "h@patch:h@npm%3A1.0.0#../evil.patch::version=1.0.0&hash=1&locator=app%40workspace%3A."),
            entry("i@patch:i@npm%3A1.0.0#~/patches/i.patch::locator=app%40workspace%3A.", "1.0.0", "i@patch:i@npm%3A1.0.0#~/patches/i.patch::version=1.0.0&hash=1&locator=app%40workspace%3A."),
        ]
        .concat());
        assert_eq!(
            sources(&d),
            vec![
                ("a", "url+https://e.example/a.tgz"),
                ("b", "git+https://github.com/x/b.git#commit=abc"),
                ("c", "url+https://e.example/c.tgz"),
                ("d", "path+../out"),
                ("g", "path+./g.tgz"),
                ("h", "path+../evil.patch"),
                ("i", NPM),
            ]
        );
    }

    #[test]
    fn berry_fails_closed() {
        let err = |body: &str| {
            yarn_lock(&format!("{BERRY}{body}"))
                .unwrap_err()
                .to_string()
        };
        assert!(
            yarn_lock("__metadata:\n  version: 3\n")
                .unwrap_err()
                .to_string()
                .contains("version 3")
        );
        assert!(
            err(&entry("x@exec:./x.js", "1.0.0", "x@exec:./x.js")).contains("unknown resolution")
        );
        assert!(err(&entry("x@npm:^1", "1.0.0", "x@npm:2.0.0")).contains("unexpected npm"));
        assert!(
            err(&entry("x@workspace:../x", "0.0.0", "x@workspace:../x"))
                .contains("outside the repo")
        );
        assert!(
            err(&entry("Up@npm:1.0.0", "1.0.0", "Up@npm:1.0.0"))
                .contains("not an npm package name")
        );
        assert!(err("\"x@npm:1\":\n  version: 1.0.0\n").contains("no `version` or `resolution`"));
        let dup = format!(
            "{}{}",
            entry("x@npm:1", "1.0.0", "x@npm:1.0.0"),
            entry("x@npm:1", "1.0.0", "x@npm:1.0.0")
        );
        assert!(err(&dup).contains("yarn.lock"));
    }
}
