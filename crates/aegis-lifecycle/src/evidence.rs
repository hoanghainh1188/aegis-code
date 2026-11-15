//! Checking what the agent's answer cites (#508). The code of a fix is
//! checked by running it; the text is checked here, against the job's
//! tree: every `changes[].location` and every source in `evidence` must
//! name a file of the patched tree or of the base, lines inside it, and,
//! when it quotes, text that is there. A fix must cite at least one source
//! for its cause. An answer that fails goes back to the agent, as one that
//! cannot be read does; out of tries, the job stops and no PR is opened.

use std::path::{Component, Path};
use std::process::Command;

use crate::answer::FinalAnswer;
use crate::{ForkConfig, ForkPlatform};

/// The longest excerpt checked; a longer one is cut to this before it is
/// looked for.
const MAX_EXCERPT_CHARS: usize = 500;

/// A cited place: a file, and its lines when given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cited {
    pub path: String,
    /// 1-based, inclusive.
    pub lines: Option<(usize, usize)>,
}

/// `path`, `path:12` or `path:12-30`. The path is relative to the repo
/// root and stays inside it.
pub fn parse_location(text: &str) -> Result<Cited, String> {
    let text = text.trim();
    let (path, lines) = match text.rsplit_once(':') {
        Some((path, tail)) if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            let (from, to) = match tail.split_once('-') {
                Some((a, b)) => (a, b),
                None => (tail, tail),
            };
            let parse = |n: &str| n.trim().parse::<usize>().ok().filter(|&n| n > 0);
            let (Some(from), Some(to)) = (parse(from), parse(to)) else {
                return Err(format!("`{text}`: lines must be `N` or `N-M`, from 1"));
            };
            if to < from {
                return Err(format!("`{text}`: the range ends before it starts"));
            }
            (path, Some((from, to)))
        }
        _ => (text, None),
    };
    let path = path.trim().trim_start_matches("./");
    let inside = !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    if !inside {
        return Err(format!(
            "`{text}`: not a path relative to the repository root"
        ));
    }
    Ok(Cited {
        path: path.to_owned(),
        lines,
    })
}

/// What the check found: how many places were cited, and what is wrong
/// with those that do not hold, one line each.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceCheck {
    pub cited: usize,
    pub wrong: Vec<String>,
}

impl SourceCheck {
    pub fn holds(&self) -> bool {
        self.wrong.is_empty()
    }
}

fn squashed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The file at `path` in the patched tree and at `base`, those that
/// exist: the patched one first.
fn versions(tree: &Path, base: &str, path: &str) -> Vec<String> {
    let patched = std::fs::read_to_string(tree.join(path)).ok();
    let before = Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["show", &format!("{base}:{path}")])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    patched.into_iter().chain(before).collect()
}

/// Why `cited` (with `excerpt`) does not hold in `text`, if it does not.
fn wrong_in(text: &str, location: &str, cited: &Cited, excerpt: Option<&str>) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let shown = match cited.lines {
        Some((_, to)) if to > lines.len() => {
            return Some(format!(
                "`{location}`: `{}` has {} lines",
                cited.path,
                lines.len()
            ));
        }
        Some((from, to)) => lines[from - 1..to].join("\n"),
        None => text.to_owned(),
    };
    let excerpt = excerpt.map(str::trim).filter(|x| !x.is_empty())?;
    let wanted: String = squashed(excerpt).chars().take(MAX_EXCERPT_CHARS).collect();
    (!squashed(&shown).contains(&wanted))
        .then(|| format!("`{location}`: the quoted text is not there"))
}

/// Why `location` (with `excerpt`) does not hold, if it does not: it must
/// hold in the patched file or in the file at `base`.
fn wrong_with(tree: &Path, base: &str, location: &str, excerpt: Option<&str>) -> Option<String> {
    let cited = match parse_location(location) {
        Ok(c) => c,
        Err(why) => return Some(why),
    };
    let versions = versions(tree, base, &cited.path);
    if versions.is_empty() {
        return Some(format!(
            "`{location}`: no file `{}` in the patched tree or the base",
            cited.path
        ));
    }
    // Holds when it holds in one of them; else why it fails in the first.
    let mut why = versions
        .iter()
        .map(|text| wrong_in(text, location, &cited, excerpt));
    let first = why.next()?;
    let first = first?;
    why.all(|w| w.is_some()).then_some(first)
}

/// Checks every place `answer` cites against `tree` and `base`. A fix must
/// cite at least one source for its cause.
pub fn check_sources(tree: &Path, base: &str, answer: &FinalAnswer) -> SourceCheck {
    let mut check = SourceCheck::default();
    if answer.evidence.is_empty() {
        check.wrong.push(
            "`evidence` is empty: cite at least one `file:line` of the repository the cause \
             rests on"
                .into(),
        );
    }
    for e in &answer.evidence {
        check.cited += 1;
        if let Some(why) = wrong_with(tree, base, &e.location, e.excerpt.as_deref()) {
            check.wrong.push(format!("evidence {why}"));
        }
    }
    for c in &answer.changes {
        check.cited += 1;
        if let Some(why) = wrong_with(tree, base, &c.location, None) {
            check.wrong.push(format!("change {why}"));
        }
    }
    check
}

/// The forge's web root from its API root: `https://api.github.com` is
/// `https://github.com`; GitHub Enterprise drops `/api/v3`, GitLab
/// `/api/v4`. `None` for anything else.
fn web_root(fork: &ForkConfig) -> Option<String> {
    let api = fork.api.trim_end_matches('/');
    let root = match fork.platform {
        ForkPlatform::GitHub if api == "https://api.github.com" => "https://github.com",
        ForkPlatform::GitHub => api.strip_suffix("/api/v3")?,
        ForkPlatform::GitLab => api.strip_suffix("/api/v4")?,
    };
    root.starts_with("https://").then(|| root.to_owned())
}

/// The cited lines on `commit` of the fork, for the PR (#508); `None` for
/// a local remote, a forge whose web page is not known, or a path with
/// characters a URL would need escaped.
pub fn line_url(fork: &ForkConfig, commit: &str, location: &str) -> Option<String> {
    if fork.push_to_local.is_some() || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let cited = parse_location(location).ok()?;
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._/".contains(c))
    };
    if !plain(&cited.path) || !plain(&fork.path()) {
        return None;
    }
    let root = web_root(fork)?;
    let blob = match fork.platform {
        ForkPlatform::GitHub => "blob",
        ForkPlatform::GitLab => "-/blob",
    };
    let anchor = match (fork.platform, cited.lines) {
        (_, None) => String::new(),
        (_, Some((a, b))) if a == b => format!("#L{a}"),
        (ForkPlatform::GitHub, Some((a, b))) => format!("#L{a}-L{b}"),
        (ForkPlatform::GitLab, Some((a, b))) => format!("#L{a}-{b}"),
    };
    Some(format!(
        "{root}/{}/{blob}/{commit}/{}{anchor}",
        fork.path(),
        cited.path
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fork(platform: ForkPlatform, api: &str) -> ForkConfig {
        ForkConfig {
            platform,
            owner: "aegis-bot".into(),
            name: "cart".into(),
            api: api.into(),
            token_env: "T".into(),
            push_to_local: None,
        }
    }

    #[test]
    fn links_point_at_the_cited_lines_of_the_commit() {
        let gh = fork(ForkPlatform::GitHub, "https://api.github.com");
        assert_eq!(
            line_url(&gh, "abc123", "src/lib.rs:12-30").as_deref(),
            Some("https://github.com/aegis-bot/cart/blob/abc123/src/lib.rs#L12-L30")
        );
        let ghe = fork(ForkPlatform::GitHub, "https://git.acme.example/api/v3/");
        assert_eq!(
            line_url(&ghe, "abc123", "a.py:4").as_deref(),
            Some("https://git.acme.example/aegis-bot/cart/blob/abc123/a.py#L4")
        );
        let gl = fork(ForkPlatform::GitLab, "https://gitlab.com/api/v4");
        assert_eq!(
            line_url(&gl, "abc123", "a.py:4-6").as_deref(),
            Some("https://gitlab.com/aegis-bot/cart/-/blob/abc123/a.py#L4-6")
        );
        assert_eq!(line_url(&gh, "abc123", "dir with space/a.py:1"), None);
        assert_eq!(line_url(&gh, "not-a-sha", "a.py:1"), None);
        let local = ForkConfig {
            push_to_local: Some("/tmp/fork.git".into()),
            ..gh
        };
        assert_eq!(line_url(&local, "abc123", "a.py:1"), None);
    }

    #[test]
    fn locations_are_a_path_with_optional_lines() {
        assert_eq!(
            parse_location("src/lib.rs:12-30").unwrap(),
            Cited {
                path: "src/lib.rs".into(),
                lines: Some((12, 30))
            }
        );
        assert_eq!(parse_location("./a.py:4").unwrap().lines, Some((4, 4)));
        assert_eq!(parse_location("README.md").unwrap().lines, None);
        // A Windows-like or URL-like colon that is not lines stays the path.
        assert_eq!(parse_location("a:b.txt").unwrap().path, "a:b.txt");
        for bad in [
            "",
            "/etc/passwd",
            "../x.rs",
            "src/../../x",
            "a.rs:0",
            "a.rs:9-3",
            "a.rs:3-x",
        ] {
            assert!(parse_location(bad).is_err(), "{bad}");
        }
    }

    struct Tree(std::path::PathBuf);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree(name: &str) -> Tree {
        let dir =
            std::env::temp_dir().join(format!("aegis-evidence-{name}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "fn add(&mut self) {\n    if self.items.len() >= MAX_ITEMS {\n        return Err(Full);\n    }\n}\n",
        )
        .unwrap();
        Tree(dir)
    }

    #[test]
    fn a_place_holds_when_the_file_lines_and_quote_are_there() {
        let t = tree("holds");
        let p = t.0.as_path();
        assert_eq!(wrong_with(p, "HEAD", "src/lib.rs:2-3", None), None);
        assert_eq!(
            wrong_with(
                p,
                "HEAD",
                "src/lib.rs:2",
                Some("if   self.items.len() >= MAX_ITEMS")
            ),
            None,
            "spaces do not matter"
        );
        assert!(
            wrong_with(p, "HEAD", "src/lib.rs:9", None)
                .unwrap()
                .contains("has 5 lines")
        );
        assert!(
            wrong_with(p, "HEAD", "src/main.rs:1", None)
                .unwrap()
                .contains("no file")
        );
        assert!(
            wrong_with(p, "HEAD", "src/lib.rs:1", Some("MAX_ITEMS"))
                .unwrap()
                .contains("not there"),
            "the quote must be in the cited lines"
        );
    }
}
