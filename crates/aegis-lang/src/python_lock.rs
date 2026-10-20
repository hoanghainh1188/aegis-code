//! Python lockfile readers (#65): `uv.lock`, `poetry.lock`,
//! `requirements*.txt`. Names are normalized by PEP 503, registry URLs
//! without a trailing `/`, so the comparison in `deps` can be exact.
//!
//! These readers feed a security check, so they fail closed: anything they
//! do not understand is an error, never silently skipped, and anything that
//! changes where packages come from changes their `source`.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::{LangError, LockedDependency};

/// Registry PyPI packages come from when nothing else is said.
pub(crate) const PYPI: &str = "https://pypi.org/simple";
/// Host PyPI serves files from.
const PYPI_FILES: &str = "files.pythonhosted.org";

/// PEP 503: lower-case, runs of `-`, `_`, `.` become one `-`.
pub(crate) fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_sep = false;
    for c in name.trim().chars() {
        if matches!(c, '-' | '_' | '.') {
            if !in_sep {
                out.push('-');
            }
            in_sep = true;
        } else {
            out.extend(c.to_lowercase());
            in_sep = false;
        }
    }
    out
}

fn registry(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    Some(authority.rsplit_once('@').map_or(authority, |(_, h)| h))
}

/// Relative path that stays inside the repo.
fn in_repo(path: &str) -> bool {
    let p = std::path::Path::new(path);
    !path.is_empty()
        && !p.is_absolute()
        && !path.starts_with('\\')
        && !path.contains("://")
        && !p.components().any(|c| c.as_os_str() == "..")
}

fn invalid(file: &str, message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: file.to_owned(),
        message: message.into(),
    }
}

// ---------------------------------------------------------------- uv.lock

#[derive(Deserialize)]
struct UvLock {
    #[serde(default)]
    package: Vec<UvPackage>,
}

#[derive(Deserialize)]
struct UvPackage {
    name: String,
    #[serde(default)]
    version: Option<String>,
    source: Option<UvSource>,
    sdist: Option<UvFile>,
    #[serde(default)]
    wheels: Vec<UvFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UvSource {
    registry: Option<String>,
    git: Option<String>,
    url: Option<String>,
    editable: Option<String>,
    #[serde(rename = "virtual")]
    virtual_: Option<String>,
    path: Option<String>,
    directory: Option<String>,
    subdirectory: Option<String>,
}

#[derive(Deserialize)]
struct UvFile {
    url: Option<String>,
}

pub(crate) fn uv_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let lock: UvLock = toml::from_str(text).map_err(|e| invalid("uv.lock", e.to_string()))?;
    let mut out = Vec::new();
    for p in lock.package {
        let err = |m: &str| invalid("uv.lock", format!("package `{}`: {m}", p.name));
        let s = p.source.ok_or_else(|| err("no source"))?;
        let source = match (s.registry, s.git, s.url) {
            (Some(r), None, None) => {
                let r = registry(&r);
                let expected = if r == PYPI {
                    Some(PYPI_FILES)
                } else {
                    host(&r)
                };
                let foreign: Vec<&str> = file_hosts(p.sdist.iter().chain(&p.wheels))
                    .into_iter()
                    .filter(|h| Some(*h) != expected)
                    .collect();
                if foreign.is_empty() {
                    r
                } else {
                    format!("{r}+files:{}", foreign.join(","))
                }
            }
            (None, Some(g), None) => format!("git+{g}"),
            (None, None, Some(u)) => format!("url+{u}"),
            (None, None, None) => {
                let local: Vec<String> = [s.editable, s.virtual_, s.path, s.directory]
                    .into_iter()
                    .flatten()
                    .collect();
                match local.as_slice() {
                    [path] if in_repo(path) => continue,
                    [path] => format!("path+{path}"),
                    _ => return Err(err("unknown source")),
                }
            }
            _ => return Err(err("more than one source")),
        };
        let source = match s.subdirectory {
            Some(sub) => format!("{source}#subdirectory={sub}"),
            None => source,
        };
        out.push(LockedDependency {
            name: normalize_name(&p.name),
            version: p.version.unwrap_or_default(),
            source,
        });
    }
    Ok(out)
}

fn file_hosts<'a>(files: impl Iterator<Item = &'a UvFile>) -> BTreeSet<&'a str> {
    files
        .filter_map(|f| f.url.as_deref().and_then(host))
        .collect()
}

// ------------------------------------------------------------ poetry.lock

#[derive(Deserialize)]
struct PoetryLock {
    #[serde(default)]
    package: Vec<PoetryPackage>,
}

#[derive(Deserialize)]
struct PoetryPackage {
    name: String,
    version: String,
    source: Option<PoetrySource>,
}

#[derive(Deserialize)]
struct PoetrySource {
    #[serde(rename = "type")]
    kind: String,
    url: Option<String>,
}

pub(crate) fn poetry_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let lock: PoetryLock =
        toml::from_str(text).map_err(|e| invalid("poetry.lock", e.to_string()))?;
    let mut out = Vec::new();
    for p in lock.package {
        let source = match p.source {
            None => PYPI.to_owned(),
            Some(s) => match (s.kind.as_str(), s.url) {
                ("directory" | "file", Some(path)) if in_repo(&path) => continue,
                ("directory" | "file", Some(path)) => format!("path+{path}"),
                ("legacy", Some(u)) => registry(&u),
                ("git", Some(u)) => format!("git+{u}"),
                ("url", Some(u)) => format!("url+{u}"),
                (kind, _) => {
                    return Err(invalid(
                        "poetry.lock",
                        format!("package `{}`: unknown source type `{kind}`", p.name),
                    ));
                }
            },
        };
        out.push(LockedDependency {
            name: normalize_name(&p.name),
            version: p.version,
            source,
        });
    }
    Ok(out)
}

// ------------------------------------------------------- requirements.txt

/// Options pip takes in a requirements file. Anything else starting with
/// `-` (typos, abbreviations) is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opt {
    IndexUrl,
    ExtraIndexUrl,
    FindLinks,
    TrustedHost,
    NoIndex,
    Include,
    Editable,
    /// Options that do not change where packages come from.
    Harmless {
        takes_value: bool,
    },
}

const OPTIONS: &[(&str, Opt)] = &[
    ("--index-url", Opt::IndexUrl),
    ("-i", Opt::IndexUrl),
    ("--extra-index-url", Opt::ExtraIndexUrl),
    ("--find-links", Opt::FindLinks),
    ("-f", Opt::FindLinks),
    ("--trusted-host", Opt::TrustedHost),
    ("--no-index", Opt::NoIndex),
    ("--requirement", Opt::Include),
    ("-r", Opt::Include),
    ("--constraint", Opt::Include),
    ("-c", Opt::Include),
    ("--editable", Opt::Editable),
    ("-e", Opt::Editable),
    ("--pre", Opt::Harmless { takes_value: false }),
    ("--prefer-binary", Opt::Harmless { takes_value: false }),
    ("--require-hashes", Opt::Harmless { takes_value: false }),
    ("--no-binary", Opt::Harmless { takes_value: true }),
    ("--only-binary", Opt::Harmless { takes_value: true }),
    ("--use-feature", Opt::Harmless { takes_value: true }),
];

fn takes_value(opt: Opt) -> bool {
    match opt {
        Opt::NoIndex => false,
        Opt::Harmless { takes_value } => takes_value,
        _ => true,
    }
}

/// Splits `-iURL`, `-i URL`, `--index-url=URL`, `--index-url URL`. `None`
/// for an unknown option or a missing value.
fn parse_option(line: &str) -> Option<(Opt, &str)> {
    OPTIONS.iter().find_map(|(name, opt)| {
        let rest = line.strip_prefix(name)?;
        if !takes_value(*opt) {
            return rest.trim().is_empty().then_some((*opt, ""));
        }
        let value = if let Some(v) = rest.strip_prefix('=') {
            v
        } else if rest.starts_with(char::is_whitespace) || !name.starts_with("--") {
            rest
        } else {
            return None;
        };
        Some((*opt, value.trim())).filter(|(_, v)| !v.is_empty())
    })
}

/// `requirements*.txt` as `pip` reads it. Index options apply to the whole
/// file wherever they appear; extra indexes, find-links, trusted hosts and
/// `--no-index` become part of every package's source, so adding one is a
/// change of source for all packages. `-r` / `-c` must name another
/// `requirements*.txt`, which the gate checks as a lockfile of its own.
pub(crate) fn requirements(file: &str, text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let lines: Vec<(usize, String)> = joined_lines(text)
        .into_iter()
        .map(|(n, l)| (n, strip_comment(&l).to_owned()))
        .filter(|(_, l)| !l.is_empty())
        .collect();
    let err = |n: usize, m: String| invalid(file, format!("line {n}: {m}"));

    let mut index = PYPI.to_owned();
    let mut extras = BTreeSet::new();
    for (n, line) in &lines {
        if !line.starts_with('-') {
            continue;
        }
        let (opt, value) = parse_option(line)
            .ok_or_else(|| err(*n, format!("unknown or incomplete option `{line}`")))?;
        match opt {
            Opt::IndexUrl => index = registry(value),
            Opt::ExtraIndexUrl => {
                extras.insert(format!("extra-index={}", registry(value)));
            }
            Opt::FindLinks => {
                extras.insert(format!("find-links={value}"));
            }
            Opt::TrustedHost => {
                extras.insert(format!("trusted-host={value}"));
            }
            Opt::NoIndex => {
                extras.insert("no-index".to_owned());
            }
            Opt::Include => {
                let name = value.rsplit('/').next().unwrap_or(value);
                if !crate::profile::glob_match("requirements*.txt", name) {
                    return Err(err(
                        *n,
                        format!("`{value}` is not a requirements*.txt the gate checks"),
                    ));
                }
            }
            Opt::Editable | Opt::Harmless { .. } => {}
        }
    }
    let index_source = if extras.is_empty() {
        index
    } else {
        let extras: Vec<String> = extras.into_iter().collect();
        format!("{index}+{}", extras.join("+"))
    };

    let mut out = Vec::new();
    for (n, line) in &lines {
        let spec = if line.starts_with('-') {
            match parse_option(line) {
                Some((Opt::Editable, value)) if is_local(value) => continue,
                Some((Opt::Editable, value)) => value,
                _ => continue,
            }
        } else if is_local(line) {
            continue;
        } else {
            line.as_str()
        };
        out.push(requirement(spec, &index_source).map_err(|m| err(*n, m))?);
    }
    Ok(out)
}

/// A wheel, sdist or directory given by a path inside the repo.
fn is_local(spec: &str) -> bool {
    let path = spec.strip_prefix("file:").unwrap_or(spec);
    (spec.starts_with('.') || spec.starts_with("file:")) && in_repo(path)
}

/// One requirement: `name[extras]==1.0 ; marker`, `name @ URL`, or a bare
/// URL with `#egg=name`.
fn requirement(line: &str, index_source: &str) -> Result<LockedDependency, String> {
    // Per-requirement options (`--hash=…`) and markers are not part of the spec.
    let spec = line.split(" --").next().unwrap_or(line);
    let spec = spec.split(';').next().unwrap_or(spec).trim();

    let url_source = |url: &str| {
        let url = url.trim();
        match url.strip_prefix("git+") {
            Some(g) => format!("git+{g}"),
            None => format!("url+{url}"),
        }
    };
    // `name @ URL` (spaces optional): an `@` before the URL's `://`.
    let named_url = spec
        .find("://")
        .is_none_or(|scheme| spec[..scheme].contains('@'));
    let (name_part, source) = if !named_url {
        let egg = spec
            .split_once("#egg=")
            .map(|(_, e)| e.split('&').next().unwrap_or(e))
            .ok_or_else(|| format!("URL `{spec}` names no package (#egg=)"))?;
        (egg, url_source(spec.split('#').next().unwrap_or(spec)))
    } else if let Some((name, url)) = spec.split_once('@') {
        (name.trim(), url_source(url))
    } else {
        (spec, index_source.to_owned())
    };

    let name_end = name_part
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(name_part.len());
    let name = &name_part[..name_end];
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err(format!("cannot read `{line}`"));
    }
    let version = name_part[name_end..]
        .split_once("==")
        .map(|(_, v)| v.split(',').next().unwrap_or(v).trim().to_owned())
        .unwrap_or_default();
    Ok(LockedDependency {
        name: normalize_name(name),
        version,
        source,
    })
}

/// Joins `\` continuations, as pip does; keeps the first line's number.
fn joined_lines(text: &str) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut start = 1;
    for (i, raw) in text.lines().enumerate() {
        if current.is_empty() {
            start = i + 1;
        }
        match raw.trim_end().strip_suffix('\\') {
            Some(head) => {
                current.push_str(head);
                current.push(' ');
            }
            None => {
                current.push_str(raw);
                lines.push((start, std::mem::take(&mut current)));
            }
        }
    }
    if !current.is_empty() {
        lines.push((start, current));
    }
    lines
}

/// pip treats `#` as a comment only at the start or after whitespace, so a
/// `#egg=` fragment in a URL is kept.
fn strip_comment(line: &str) -> &str {
    let cut = line
        .char_indices()
        .find(|&(i, c)| c == '#' && (i == 0 || line[..i].ends_with(char::is_whitespace)))
        .map_or(line.len(), |(i, _)| i);
    line[..cut].trim()
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

    fn req(text: &str) -> Vec<LockedDependency> {
        requirements("requirements.txt", text).unwrap()
    }

    fn req_err(text: &str) -> String {
        requirements("requirements.txt", text)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn normalizes_names_by_pep_503() {
        assert_eq!(normalize_name("Foo_Bar.baz--Qux"), "foo-bar-baz-qux");
        assert_eq!(normalize_name("requests"), "requests");
    }

    #[test]
    fn reads_uv_lock() {
        let text = r#"
version = 1

[[package]]
name = "my-app"
version = "0.1.0"
source = { editable = "." }

[[package]]
name = "Requests"
version = "2.32.3"
source = { registry = "https://pypi.org/simple/" }
sdist = { url = "https://files.pythonhosted.org/packages/r-2.32.3.tar.gz" }
wheels = [{ url = "https://files.pythonhosted.org/packages/r-2.32.3-py3-none-any.whl" }]

[[package]]
name = "forked"
version = "1.0.0"
source = { git = "https://github.com/someone/forked?rev=abc#abc" }

[[package]]
name = "local-lib"
version = "0.1.0"
source = { directory = "libs/local" }
"#;
        assert_eq!(
            uv_lock(text).unwrap(),
            vec![
                dep("requests", "2.32.3", PYPI),
                dep(
                    "forked",
                    "1.0.0",
                    "git+https://github.com/someone/forked?rev=abc#abc"
                ),
            ]
        );
        assert!(uv_lock("package = 1").is_err());
    }

    #[test]
    fn uv_lock_fails_closed() {
        let pkg = |source: &str| format!("[[package]]\nname = \"x\"\nversion = \"1\"\n{source}\n");
        let err = |source: &str| uv_lock(&pkg(source)).unwrap_err().to_string();
        assert!(err("").contains("no source"));
        assert!(err("source = { mystery = \"y\" }").contains("unknown field"));
        assert!(
            err("source = { registry = \"https://pypi.org/simple\", git = \"x\" }")
                .contains("more than one")
        );
        // Paths outside the repo are external code.
        let outside = uv_lock(&pkg("source = { path = \"../elsewhere\" }")).unwrap();
        assert_eq!(outside[0].source, "path+../elsewhere");
        let absolute = uv_lock(&pkg("source = { directory = \"/opt/x\" }")).unwrap();
        assert_eq!(absolute[0].source, "path+/opt/x");
    }

    #[test]
    fn uv_lock_files_from_another_host_change_the_source() {
        let text = r#"
[[package]]
name = "requests"
version = "2.32.3"
source = { registry = "https://pypi.org/simple" }
wheels = [{ url = "https://evil.example/requests-2.32.3-py3-none-any.whl" }]
"#;
        assert_eq!(
            uv_lock(text).unwrap()[0].source,
            "https://pypi.org/simple+files:evil.example"
        );
    }

    #[test]
    fn reads_poetry_lock() {
        let text = r#"
[[package]]
name = "Django"
version = "5.1.0"

[[package]]
name = "private-lib"
version = "2.0.0"

[package.source]
type = "legacy"
url = "https://pypi.internal/simple/"
reference = "internal"

[[package]]
name = "vendored"
version = "0.1.0"

[package.source]
type = "directory"
url = "vendor/vendored"
"#;
        assert_eq!(
            poetry_lock(text).unwrap(),
            vec![
                dep("django", "5.1.0", PYPI),
                dep("private-lib", "2.0.0", "https://pypi.internal/simple"),
            ]
        );
        let pkg = |source: &str| {
            format!("[[package]]\nname = \"x\"\nversion = \"1\"\n[package.source]\n{source}\n")
        };
        let odd = poetry_lock(&pkg("type = \"svn\"")).unwrap_err().to_string();
        assert!(odd.contains("unknown source type"));
        let outside = poetry_lock(&pkg("type = \"directory\"\nurl = \"../x\"")).unwrap();
        assert_eq!(outside[0].source, "path+../x");
    }

    #[test]
    fn reads_requirements() {
        let text = "\
# pinned by pip-compile
Flask[async]==3.0.3 ; python_version >= \"3.9\"
urllib3==2.2.2 \\
    --hash=sha256:abc
-r requirements-base.txt
-e ./packages/core
./wheels/local.whl
black>=24  # unpinned
mylib @ git+https://github.com/someone/mylib@v1
";
        assert_eq!(
            req(text),
            vec![
                dep("flask", "3.0.3", PYPI),
                dep("urllib3", "2.2.2", PYPI),
                dep("black", "", PYPI),
                dep("mylib", "", "git+https://github.com/someone/mylib@v1"),
            ]
        );
    }

    #[test]
    fn index_url_applies_to_the_whole_file() {
        let deps = req("x==1\n--index-url https://pypi.internal/simple/\ny==2\n");
        assert!(
            deps.iter()
                .all(|d| d.source == "https://pypi.internal/simple")
        );
        for form in [
            "-ihttps://a.example/simple",
            "-i https://a.example/simple",
            "--index-url=https://a.example/simple",
        ] {
            let deps = req(&format!("x==1\n{form}\n"));
            assert_eq!(deps[0].source, "https://a.example/simple", "{form}");
        }
    }

    #[test]
    fn extra_indexes_and_find_links_change_every_source() {
        let deps = req("requests==2.32.3\n--extra-index-url https://evil.example/simple\n");
        assert_eq!(
            deps[0].source,
            "https://pypi.org/simple+extra-index=https://evil.example/simple"
        );
        let deps = req("requests==1\n-f https://evil.example/w\n--trusted-host evil.example\n");
        assert!(deps[0].source.contains("find-links=https://evil.example/w"));
        assert!(deps[0].source.contains("trusted-host=evil.example"));
        assert!(
            req("requests==1\n--no-index\n")[0]
                .source
                .ends_with("+no-index")
        );
    }

    #[test]
    fn remote_editables_and_bare_urls_are_dependencies() {
        assert_eq!(
            req("-e git+https://evil.example/x.git#egg=Foo_Pkg\n"),
            vec![dep("foo-pkg", "", "git+https://evil.example/x.git")]
        );
        let deps = req("https://evil.example/pkg-1.0.whl#egg=pkg\n");
        assert_eq!(deps[0].source, "url+https://evil.example/pkg-1.0.whl");
        assert!(req_err("git+https://evil.example/x.git\n").contains("names no package"));
        assert!(req_err("https://evil.example/x.whl\n").contains("names no package"));
        assert!(req_err("-e ../outside\n").contains("cannot read"));
        let tight = req("pkg@https://evil.example/pkg.whl\n");
        assert_eq!(
            tight,
            vec![dep("pkg", "", "url+https://evil.example/pkg.whl")]
        );
    }

    #[test]
    fn unknown_options_and_unchecked_includes_fail() {
        let unknown = req_err("--index https://evil.example/simple\n");
        assert!(
            unknown.contains("unknown or incomplete option"),
            "{unknown}"
        );
        assert!(req_err("--trusted\n").contains("unknown"));
        assert!(req_err("-i\n").contains("incomplete"));
        assert!(req_err("-r deps/prod.in\n").contains("not a requirements*.txt"));
        assert!(req_err("-c constraints.txt\n").contains("not a requirements*.txt"));
        assert!(req("-r requirements/requirements-base.txt\n--pre\n--require-hashes\n").is_empty());
    }

    #[test]
    fn reports_unreadable_requirement_lines() {
        let err = req_err("ok==1\n==1.0\n");
        assert!(err.contains("line 2"), "{err}");
    }
}
