//! `go.mod` reader (#69). Go has no separate lockfile: since Go 1.17 the
//! `require` blocks list every module the build uses, and `go.sum` only
//! holds their hashes. `replace` decides where a module really comes from,
//! so it is part of the source. Fails closed on directives it does not know.

use std::collections::BTreeMap;

use crate::{LangError, LockedDependency};

/// Source of modules fetched through the module proxy.
pub(crate) const GO_PROXY: &str = "proxy.golang.org";

const KNOWN: &[&str] = &[
    "module",
    "go",
    "toolchain",
    "godebug",
    "require",
    "exclude",
    "replace",
    "retract",
    "tool",
    "ignore",
];

fn invalid(n: usize, message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: "go.mod".into(),
        message: format!("line {n}: {}", message.into()),
    }
}

/// Words of a line, with `"quoted"` and `` `raw` `` paths unquoted. Go
/// resolves escapes in `"…"` strings, so a word with `\` (or a quote that
/// is not at both ends) could name a different module than it looks like:
/// refused.
fn words(n: usize, line: &str) -> Result<Vec<String>, LangError> {
    line.split_ascii_whitespace()
        .map(|w| {
            let inner = w
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .or_else(|| w.strip_prefix('`').and_then(|x| x.strip_suffix('`')))
                .unwrap_or(w);
            if inner.contains(['\\', '"', '`']) || !line.is_ascii() {
                return Err(invalid(
                    n,
                    format!("`{w}`: escapes and non-ASCII are not supported"),
                ));
            }
            Ok(inner.to_owned())
        })
        .collect()
}

/// Cuts a `//` comment that is not inside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'`' => quote = Some(b),
            None if b == b'/' && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            None => {}
        }
    }
    line
}

/// Since Go 1.17 the main `go.mod` lists every module the build needs;
/// before that, transitive requirements live in other modules' go.mod.
const MIN_GO: (u32, u32) = (1, 17);

fn go_version(n: usize, v: &str) -> Result<(u32, u32), LangError> {
    let mut parts = v.split('.');
    let mut num = || parts.next().and_then(|x| x.parse::<u32>().ok());
    match (num(), num()) {
        (Some(major), Some(minor)) => Ok((major, minor)),
        _ => Err(invalid(n, format!("cannot read go version `{v}`"))),
    }
}

/// Lines of `go.mod` as (line number, directive, arguments), with
/// `( … )` blocks expanded and `//` comments removed.
fn directives(text: &str) -> Result<Vec<(usize, String, Vec<String>)>, LangError> {
    let mut out = Vec::new();
    let mut block: Option<String> = None;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(verb) = &block {
            if line == ")" {
                block = None;
            } else {
                out.push((n, verb.clone(), words(n, line)?));
            }
            continue;
        }
        let mut w = words(n, line)?;
        let verb = w.remove(0);
        if !KNOWN.contains(&verb.as_str()) {
            return Err(invalid(n, format!("unknown directive `{verb}`")));
        }
        if w.first().is_some_and(|x| x == "(") {
            if w.len() > 1 {
                return Err(invalid(n, "text after `(`"));
            }
            block = Some(verb);
        } else {
            out.push((n, verb, w));
        }
    }
    match block {
        Some(verb) => Err(invalid(
            text.lines().count(),
            format!("`{verb} (` is not closed"),
        )),
        None => Ok(out),
    }
}

/// Local replacement targets start with `./` or `../` (or are absolute).
fn is_path(target: &str) -> bool {
    target.starts_with("./") || target.starts_with("../") || target.starts_with('/')
}

pub(crate) fn go_mod(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let mut required: Vec<(String, String)> = Vec::new();
    // (module, version or "" for all versions) -> source
    let mut replaced: BTreeMap<(String, String), Option<String>> = BTreeMap::new();
    let mut go = None;
    for (n, verb, args) in directives(text)? {
        match verb.as_str() {
            "go" => match args.as_slice() {
                [v] => go = Some(go_version(n, v)?),
                _ => return Err(invalid(n, "`go` needs one version")),
            },
            "require" => match args.as_slice() {
                [module, version, ..] => required.push((module.clone(), version.clone())),
                _ => return Err(invalid(n, "`require` needs a module and a version")),
            },
            "replace" => {
                let arrow = args
                    .iter()
                    .position(|a| a == "=>")
                    .ok_or_else(|| invalid(n, "`replace` without `=>`"))?;
                let (from, to) = (&args[..arrow], &args[arrow + 1..]);
                let key = match from {
                    [m] => (m.clone(), String::new()),
                    [m, v] => (m.clone(), v.clone()),
                    _ => return Err(invalid(n, "bad left side of `replace`")),
                };
                let source = match to {
                    [path] if is_path(path) => {
                        let inside = !path.starts_with('/')
                            && !std::path::Path::new(path)
                                .components()
                                .any(|c| c.as_os_str() == "..");
                        (!inside).then(|| format!("path+{path}"))
                    }
                    [m, v] => Some(format!("replace+{m}@{v}")),
                    _ => return Err(invalid(n, "bad right side of `replace`")),
                };
                replaced.insert(key, source);
            }
            _ => {}
        }
    }
    match go {
        Some(v) if v >= MIN_GO => {}
        Some((major, minor)) => {
            return Err(invalid(
                1,
                format!("go {major}.{minor}: go.mod lists every module only from go 1.17"),
            ));
        }
        None => {
            return Err(invalid(
                1,
                "no `go` directive (go.mod lists every module only from go 1.17)",
            ));
        }
    }
    Ok(required
        .into_iter()
        .filter_map(|(module, version)| {
            let rule = replaced
                .get(&(module.clone(), version.clone()))
                .or_else(|| replaced.get(&(module.clone(), String::new())));
            let source = match rule {
                // Replaced by a directory inside the repo: repo code.
                Some(None) => return None,
                Some(Some(s)) => s.clone(),
                None => GO_PROXY.to_owned(),
            };
            Some(LockedDependency {
                name: module,
                version,
                source,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(name: &str, version: &str, source: &str) -> LockedDependency {
        LockedDependency {
            name: name.into(),
            version: version.into(),
            source: source.into(),
        }
    }

    #[test]
    fn reads_require_blocks_and_lines() {
        let text = "module example.com/cart

go 1.22
toolchain go1.22.5

require (
	github.com/google/uuid v1.6.0
	golang.org/x/text v0.14.0 // indirect
)

require \"github.com/quoted/mod\" v0.1.0
";
        assert_eq!(
            go_mod(text).unwrap(),
            vec![
                dep("github.com/google/uuid", "v1.6.0", GO_PROXY),
                dep("golang.org/x/text", "v0.14.0", GO_PROXY),
                dep("github.com/quoted/mod", "v0.1.0", GO_PROXY),
            ]
        );
    }

    #[test]
    fn replace_changes_the_source() {
        let text = "module m
go 1.22
require (
	github.com/a/b v1.2.3
	github.com/c/d v0.1.0
	github.com/e/f v1.0.0
	github.com/g/h v2.0.0
)
replace github.com/a/b => github.com/evil/b v1.2.4
replace github.com/c/d v0.1.0 => ../outside
replace github.com/e/f => ./internal/f
replace github.com/g/h v9.9.9 => github.com/never/used v1.0.0
";
        assert_eq!(
            go_mod(text).unwrap(),
            vec![
                dep(
                    "github.com/a/b",
                    "v1.2.3",
                    "replace+github.com/evil/b@v1.2.4"
                ),
                dep("github.com/c/d", "v0.1.0", "path+../outside"),
                dep("github.com/g/h", "v2.0.0", GO_PROXY),
            ],
            "in-repo replacement is repo code; unmatched version is ignored"
        );
    }

    #[test]
    fn fails_closed_on_unknown_or_broken_input() {
        let err = |t: &str| go_mod(t).unwrap_err().to_string();
        assert!(
            err("module m\ngo 1.22\nfrobnicate x\n").contains("unknown directive `frobnicate`")
        );
        assert!(err("module m\nrequire (\n a v1\n").contains("not closed"));
        assert!(err("module m\nrequire a\n").contains("needs a module and a version"));
        assert!(err("module m\nreplace a b\n").contains("without `=>`"));
        assert!(err("module m\nreplace a => b c d\n").contains("right side"));
    }

    #[test]
    fn escapes_and_odd_quotes_are_refused() {
        let err = |t: &str| go_mod(t).unwrap_err().to_string();
        let base = "module m\ngo 1.22\nrequire github.com/a/b v1.0.0\n";
        assert!(
            err(&format!(
                "{base}replace \"github.com/\\x61/b\" => github.com/evil/b v1.0.0\n"
            ))
            .contains("escapes")
        );
        assert!(err(&format!("{base}replace github.com/a/b\" => x v1\n")).contains("escapes"));
        assert!(
            err(&format!("{base}require github.com/\u{00e4}/b v1.0.0\n")).contains("non-ASCII")
        );
        // A raw string has no escapes and is fine.
        let raw = format!("{base}replace `github.com/a/b` => github.com/evil/b v1.0.0\n");
        assert_eq!(
            go_mod(&raw).unwrap()[0].source,
            "replace+github.com/evil/b@v1.0.0"
        );
    }

    #[test]
    fn comment_markers_inside_quotes_are_not_comments() {
        let text = "module m\ngo 1.22\nrequire \"example.com/a//b\" v1.0.0 // indirect\n";
        assert_eq!(go_mod(text).unwrap()[0].name, "example.com/a//b");
    }

    #[test]
    fn go_below_1_17_or_missing_is_refused() {
        let err = |t: &str| go_mod(t).unwrap_err().to_string();
        assert!(err("module m\ngo 1.16\nrequire a v1\n").contains("go 1.16"));
        assert!(err("module m\nrequire a v1\n").contains("no `go` directive"));
        assert!(err("module m\ngo one\n").contains("cannot read go version"));
        assert!(go_mod("module m\ngo 1.17\n").unwrap().is_empty());
    }
}
