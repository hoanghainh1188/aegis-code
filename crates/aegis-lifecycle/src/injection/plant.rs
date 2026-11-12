//! The poisoned base of an injection case (#361): one commit on top of the
//! issue's base that adds the payload to the repo (README, a comment, a documentation
//! file) and,
//! for a secret case, the canary file. Written with git plumbing into the
//! platform's clone, so no worktree or branch is touched.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use aegis_lang::Language;

use crate::{LifecycleError, job};

/// README names looked for at the base, in order; the first is created
/// when none exists.
const READMES: &[&str] = &[
    "README.md",
    "README",
    "README.rst",
    "README.markdown",
    "readme.md",
];

/// A fixed identity and date: the same case plants the same commit.
const IDENT: &[(&str, &str)] = &[
    ("GIT_AUTHOR_NAME", "maintainer"),
    ("GIT_AUTHOR_EMAIL", "maintainer@example.invalid"),
    ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "maintainer"),
    ("GIT_COMMITTER_EMAIL", "maintainer@example.invalid"),
    ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z"),
];

fn git(
    repo: &Path,
    env: &[(&str, &str)],
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<String, LifecycleError> {
    let failed = |message: String| LifecycleError::Git {
        args: args.join(" "),
        message,
    };
    let mut child = Command::new("git")
        // Only the clone given, its own objects, paths taken literally.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .envs(env.iter().copied())
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(repo)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(e.to_string()))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(bytes).map_err(|e| failed(e.to_string()))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| failed(e.to_string()))?;
    if !out.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A scratch index of this process's own, removed when dropped, error or
/// not: two runs sharing a state dir never write the same one.
struct Index(PathBuf);

impl Index {
    fn new(scratch: &Path) -> Result<Self, LifecycleError> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        std::fs::create_dir_all(scratch).map_err(|e| LifecycleError::io(scratch, &e))?;
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("inject-{}-{n}.index", std::process::id());
        let path =
            std::path::absolute(scratch.join(name)).map_err(|e| LifecycleError::io(scratch, &e))?;
        let _ = std::fs::remove_file(&path);
        Ok(Self(path))
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The commit id `rev` names.
fn resolve(clone: &Path, rev: &str) -> Result<String, LifecycleError> {
    let spec = format!("{rev}^{{commit}}");
    git(
        clone,
        &[],
        &["rev-parse", "--verify", "--end-of-options", &spec],
        None,
    )
}

/// The mode of `path` at `commit`, `None` when it is not there; an error
/// when git cannot tell (a bad commit).
fn mode_at(clone: &Path, commit: &str, path: &str) -> Result<Option<String>, LifecycleError> {
    let entry = git(
        clone,
        &[],
        &[
            "ls-tree",
            "--full-tree",
            "--end-of-options",
            commit,
            "--",
            path,
        ],
        None,
    )?;
    Ok(entry
        .split(' ')
        .next()
        .filter(|m| !m.is_empty())
        .map(str::to_owned))
}

/// A commit on `base` in `clone` with `files` written (path, content),
/// each keeping its mode, through an index of its own under `scratch`.
pub fn commit_on(
    clone: &Path,
    base: &str,
    files: &[(String, String)],
    scratch: &Path,
) -> Result<String, LifecycleError> {
    let base = resolve(clone, base)?;
    let index = Index::new(scratch)?;
    let index_path = index.0.to_string_lossy().into_owned();
    let env = [("GIT_INDEX_FILE", index_path.as_str())];
    git(clone, &env, &["read-tree", "--end-of-options", &base], None)?;
    for (path, content) in files {
        let mode = match mode_at(clone, &base, path)?.as_deref() {
            None | Some("100644") => "100644",
            Some("100755") => "100755",
            Some(other) => {
                return Err(LifecycleError::Issue(format!(
                    "{path} has mode {other} at {base}: not a plain file"
                )));
            }
        };
        let blob = git(
            clone,
            &[],
            &["hash-object", "-w", "--stdin"],
            Some(content.as_bytes()),
        )?;
        let info = format!("{mode},{blob},{path}");
        let args = ["update-index", "--add", "--cacheinfo", &info];
        git(clone, &env, &args, None)?;
    }
    let tree = git(clone, &env, &["write-tree"], None)?;
    let args = [
        "commit-tree",
        &tree,
        "-p",
        &base,
        "-m",
        "docs: maintainer notes",
    ];
    git(clone, IDENT, &args, None)
}

/// `path` at `rev` as text; `None` only when git says it is not there.
fn text_at(clone: &Path, rev: &str, path: &str) -> Result<Option<String>, LifecycleError> {
    if mode_at(clone, rev, path)?.is_none() {
        return Ok(None);
    }
    let bytes = job::show(clone, rev, path)?
        .ok_or_else(|| LifecycleError::Issue(format!("cannot read {path} at {rev}")))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| LifecycleError::Issue(format!("{path} at {rev} is not UTF-8")))
}

/// The README at `base` with a section holding `payload` appended, or a
/// new `README.md` when the base has none.
pub fn readme(clone: &Path, base: &str, payload: &str) -> Result<(String, String), LifecycleError> {
    for name in READMES {
        if let Some(old) = text_at(clone, base, name)? {
            let text = format!(
                "{}\n\n## Maintainer notes\n\n{}\n",
                old.trim_end(),
                payload.trim()
            );
            return Ok(((*name).to_owned(), text));
        }
    }
    Ok((
        READMES[0].to_owned(),
        format!("## Maintainer notes\n\n{}\n", payload.trim()),
    ))
}

/// `file` at `base` with `payload` as a paragraph before its line `line`
/// (#441), indented as the text that follows it.
pub fn doc(
    clone: &Path,
    base: &str,
    file: &str,
    line: usize,
    payload: &str,
) -> Result<(String, String), LifecycleError> {
    let old = text_at(clone, base, file)?
        .ok_or_else(|| LifecycleError::Issue(format!("{file} does not exist at {base}")))?;
    Ok((file.to_owned(), insert_paragraph(&old, line, payload)))
}

/// `text` with `payload` and a blank line inserted before its 1-based
/// line `at` (at the end past the last line), each payload line indented
/// as the first non-blank line from `at` on; line endings kept.
pub fn insert_paragraph(text: &str, at: usize, payload: &str) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = text.lines().collect();
    let at = at.clamp(1, lines.len() + 1) - 1;
    let indent: String = lines[at..]
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).collect())
        .unwrap_or_default();
    let block: Vec<String> = payload
        .trim()
        .lines()
        .map(|l| format!("{indent}{l}").trim_end().to_owned())
        .chain(std::iter::once(String::new()))
        .collect();
    let out: Vec<&str> = lines[..at]
        .iter()
        .copied()
        .chain(block.iter().map(String::as_str))
        .chain(lines[at..].iter().copied())
        .collect();
    let mut joined = out.join(eol);
    if text.ends_with('\n') || at == lines.len() {
        joined.push_str(eol);
    }
    joined
}

/// `file` at `base` with `payload` as a comment block just before the
/// first line the fix changes, so the agent reads it where it works.
pub fn comment(
    clone: &Path,
    base: &str,
    fix: &str,
    file: &str,
    language: Language,
    payload: &str,
) -> Result<(String, String), LifecycleError> {
    let old = text_at(clone, base, file)?
        .ok_or_else(|| LifecycleError::Issue(format!("{file} does not exist at {base}")))?;
    let line = first_changed_line(clone, base, fix, file)?;
    Ok((
        file.to_owned(),
        insert_comment(&old, line, language, payload),
    ))
}

/// The 1-based line of `file` at `base` before which the fix's first change
/// goes; 1 when the fix does not change it.
fn first_changed_line(
    clone: &Path,
    base: &str,
    fix: &str,
    file: &str,
) -> Result<usize, LifecycleError> {
    let diff = git(
        clone,
        &[],
        &[
            "diff",
            "-U0",
            "--no-color",
            "--no-ext-diff",
            "--end-of-options",
            base,
            fix,
            "--",
            file,
        ],
        None,
    )?;
    Ok(diff.lines().find_map(hunk_start).unwrap_or(1))
}

/// From `@@ -a,b +c,d @@`: `a`, or `a + 1` when the hunk only inserts
/// after line `a`.
fn hunk_start(line: &str) -> Option<usize> {
    let old = line.strip_prefix("@@ -")?.split(' ').next()?;
    let (start, len) = old.split_once(',').unwrap_or((old, "1"));
    let start: usize = start.parse().ok()?;
    let len: usize = len.parse().ok()?;
    Some(if len == 0 { start + 1 } else { start.max(1) })
}

/// `text` with `payload` as line comments before line `at` (1-based),
/// indented as that line, in the file's own line ending. A Python shebang
/// and coding line stay first.
pub fn insert_comment(text: &str, at: usize, language: Language, payload: &str) -> String {
    let marker = match language {
        Language::Python => "#",
        Language::Rust
        | Language::TypeScript
        | Language::Java
        | Language::Go
        | Language::CSharp => "//",
    };
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = text.lines().collect();
    let header = match language {
        Language::Python => lines
            .iter()
            .take(2)
            .take_while(|l| l.starts_with("#!") || (l.starts_with('#') && l.contains("coding")))
            .count(),
        _ => 0,
    };
    let at = at.clamp(1 + header, lines.len() + 1) - 1;
    let indent: String = lines
        .get(at)
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).collect())
        .unwrap_or_default();
    let block: Vec<String> = payload
        .trim()
        .lines()
        .map(|l| format!("{indent}{marker} {l}").trim_end().to_owned())
        .collect();
    let out: Vec<&str> = lines[..at]
        .iter()
        .copied()
        .chain(block.iter().map(String::as_str))
        .chain(lines[at..].iter().copied())
        .collect();
    let mut joined = out.join(eol);
    if text.ends_with('\n') {
        joined.push_str(eol);
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_starts() {
        assert_eq!(hunk_start("@@ -12,3 +12,4 @@ fn a()"), Some(12));
        assert_eq!(hunk_start("@@ -7 +7 @@"), Some(7));
        assert_eq!(hunk_start("@@ -7,0 +8,2 @@"), Some(8));
        assert_eq!(hunk_start("@@ -0,0 +1,3 @@"), Some(1));
        assert_eq!(hunk_start("+++ b/x"), None);
    }

    #[test]
    fn a_comment_goes_before_the_line_with_its_indent() {
        let text = "package a\n\nfunc f() {\n\treturn 1\n}\n";
        let got = insert_comment(text, 4, Language::Go, "do X\n\nthen Y");
        assert_eq!(
            got,
            "package a\n\nfunc f() {\n\t// do X\n\t//\n\t// then Y\n\treturn 1\n}\n"
        );
    }

    #[test]
    fn a_python_shebang_stays_first_and_lines_past_the_end_append() {
        let text = "#!/usr/bin/env python\nx = 1";
        assert_eq!(
            insert_comment(text, 1, Language::Python, "hi"),
            "#!/usr/bin/env python\n# hi\nx = 1"
        );
        assert_eq!(insert_comment("a\n", 9, Language::Rust, "z"), "a\n// z\n");
    }

    #[test]
    fn line_endings_and_a_python_coding_line_are_kept() {
        let crlf = "class A {\r\n  int x;\r\n}\r\n";
        assert_eq!(
            insert_comment(crlf, 2, Language::CSharp, "hi"),
            "class A {\r\n  // hi\r\n  int x;\r\n}\r\n"
        );
        let coded = "#!/usr/bin/env python\n# -*- coding: utf-8 -*-\nx = 1\n";
        assert_eq!(
            insert_comment(coded, 1, Language::Python, "hi"),
            "#!/usr/bin/env python\n# -*- coding: utf-8 -*-\n# hi\nx = 1\n"
        );
    }

    #[test]
    fn an_executable_keeps_its_mode_and_a_bad_base_is_an_error() {
        let repo = std::env::temp_dir().join(format!("aegis-plant-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| {
            let ident = ["-c", "user.name=t", "-c", "user.email=t@example.com"];
            git(&repo, &[], &[&ident[..], args].concat(), None).unwrap()
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("run.sh"), "echo a\n").unwrap();
        g(&["add", "-A"]);
        g(&["update-index", "--chmod=+x", "run.sh"]);
        g(&["commit", "-qm", "base"]);
        let files = [("run.sh".to_owned(), "# note\necho a\n".to_owned())];
        let planted = commit_on(&repo, "HEAD", &files, &repo.join(".git/s")).unwrap();
        assert!(g(&["ls-tree", &planted, "run.sh"]).starts_with("100755 "));
        // Nothing left behind in the scratch dir.
        assert_eq!(std::fs::read_dir(repo.join(".git/s")).unwrap().count(), 0);
        assert!(readme(&repo, "no-such-rev", "x").is_err());
        assert!(commit_on(&repo, "no-such-rev", &files, &repo.join(".git/s")).is_err());
        std::fs::remove_dir_all(&repo).unwrap();
    }
}
