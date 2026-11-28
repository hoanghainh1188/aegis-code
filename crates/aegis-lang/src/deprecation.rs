//! Deprecation warnings of a repo (#705): the profile's `[deprecations]`
//! command lists them, and its output is read here into [`Deprecation`]s,
//! one per call of a deprecated API, for a scheduled batch to fix them
//! the way Sonar warnings are (`aegis-sonar-batch`).
//!
//! Each reader keeps only deprecation warnings, with the path relative to
//! the repo root; a warning outside the repo (a dependency's code) is left
//! out. The output is the compiler's: untrusted text, kept short.

use std::path::Path;

use regex::Regex;
use serde::Deserialize;

use crate::CommandTemplate;

/// Characters kept of a warning's message.
const MAX_MESSAGE_CHARS: usize = 400;

/// How a profile lists its deprecation warnings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deprecations {
    pub command: CommandTemplate,
    pub output: DeprecationOutput,
}

/// The output of a `[deprecations]` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeprecationOutput {
    /// `cargo check --message-format=json`: rustc's `deprecated` lint.
    CargoJson,
    /// javac through Maven or Gradle: `[deprecation]` warnings, as
    /// `File.java:[12,8]` (Maven) or `File.java:12:` (javac).
    Javac,
    /// kotlinc through Gradle: `w: file:///…/File.kt:12:5 '…' is deprecated.`
    Kotlinc,
    /// MSBuild (`dotnet build`): `File.cs(12,5): warning CS0618: …`, and
    /// CS0612 and `SYSLIB0…` obsoletions.
    Msbuild,
}

/// One call of a deprecated API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deprecation {
    /// Relative to the repo root, with `/`.
    pub path: String,
    /// 1-based.
    pub line: u64,
    /// The compiler's code: `deprecated`, `CS0618`, `deprecation`.
    pub code: String,
    pub message: String,
    /// Which of the same warnings in the file this is, from 0 in line
    /// order: two calls of one API in one file have the same message.
    pub occurrence: u32,
}

impl Deprecation {
    /// Stable across runs and lines: a deprecation that only moved keeps
    /// its key. FNV-1a 64 of the path, code, message and occurrence.
    pub fn key(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let occurrence = self.occurrence.to_string();
        for byte in [
            self.path.as_str(),
            "\0",
            &self.code,
            "\0",
            &self.message,
            "\0",
            &occurrence,
        ]
        .iter()
        .flat_map(|s| s.bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("deprecation:{hash:016x}")
    }
}

/// The deprecations in `output` of a command run in `root`, each once.
pub fn read_deprecations(format: DeprecationOutput, output: &str, root: &Path) -> Vec<Deprecation> {
    let mut found: Vec<Deprecation> = match format {
        DeprecationOutput::CargoJson => cargo_json(output),
        DeprecationOutput::Javac => javac(output),
        DeprecationOutput::Kotlinc => kotlinc(output),
        DeprecationOutput::Msbuild => msbuild(output),
    }
    .into_iter()
    .filter_map(|d| {
        let path = relative(&d.path, root)?;
        Some(Deprecation {
            path,
            message: short(&d.message),
            ..d
        })
    })
    .collect();
    found.sort_by(|a, b| (&a.path, a.line, &a.message).cmp(&(&b.path, b.line, &b.message)));
    found.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.message == b.message);
    let mut seen: std::collections::BTreeMap<(String, String, String), u32> = Default::default();
    for d in &mut found {
        let n = seen
            .entry((d.path.clone(), d.code.clone(), d.message.clone()))
            .or_insert(0);
        d.occurrence = *n;
        *n += 1;
    }
    found
}

fn short(message: &str) -> String {
    let line = message.lines().next().unwrap_or("").trim();
    line.chars().take(MAX_MESSAGE_CHARS).collect()
}

/// `path` relative to `root`, or `None` outside it (or going up out of it).
fn relative(path: &str, root: &Path) -> Option<String> {
    let path = path
        .strip_prefix("file://")
        .unwrap_or(path)
        .replace('\\', "/");
    let root = root.to_string_lossy().replace('\\', "/");
    let rel = if path.starts_with('/') || path.chars().nth(1) == Some(':') {
        path.strip_prefix(&format!("{}/", root.trim_end_matches('/')))?
            .to_owned()
    } else {
        path.trim_start_matches("./").to_owned()
    };
    let inside = !rel.is_empty() && !rel.split('/').any(|p| p == "..");
    inside.then_some(rel)
}

#[derive(Deserialize)]
struct CargoLine {
    reason: String,
    #[serde(default)]
    message: Option<RustcMessage>,
}

#[derive(Deserialize)]
struct RustcMessage {
    message: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    code: Option<RustcCode>,
    #[serde(default)]
    spans: Vec<RustcSpan>,
}

#[derive(Deserialize)]
struct RustcCode {
    code: String,
}

#[derive(Deserialize)]
struct RustcSpan {
    file_name: String,
    line_start: u64,
    #[serde(default)]
    is_primary: bool,
}

fn cargo_json(output: &str) -> Vec<Deprecation> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<CargoLine>(line).ok())
        .filter(|l| l.reason == "compiler-message")
        .filter_map(|l| {
            let m = l.message?;
            let code = m.code?.code;
            let deprecated = matches!(code.as_str(), "deprecated" | "deprecated_in_future");
            if !deprecated || m.level != "warning" {
                return None;
            }
            let span = m.spans.iter().find(|s| s.is_primary)?;
            Some(Deprecation {
                path: span.file_name.clone(),
                line: span.line_start,
                code,
                message: m.message,
                occurrence: 0,
            })
        })
        .collect()
}

fn javac(output: &str) -> Vec<Deprecation> {
    // Maven: `[WARNING] /r/src/A.java:[12,8] [deprecation] f() in B has been deprecated`
    // javac: `src/A.java:12: warning: [deprecation] f() in B has been deprecated`
    let warning = Regex::new(
        r"^(?:\[WARNING\]\s+)?(?P<path>\S+?\.java):(?:\[(?P<l1>\d+),\d+\]|(?P<l2>\d+):)\s*(?:warning:\s*)?\[(?P<code>deprecation|removal)\]\s*(?P<msg>.+)$",
    )
    .expect("javac pattern");
    output
        .lines()
        .filter_map(|line| {
            let c = warning.captures(line.trim())?;
            let line = c
                .name("l1")
                .or_else(|| c.name("l2"))?
                .as_str()
                .parse()
                .ok()?;
            Some(Deprecation {
                path: c["path"].to_owned(),
                line,
                code: c["code"].to_owned(),
                message: c["msg"].to_owned(),
                occurrence: 0,
            })
        })
        .collect()
}

fn kotlinc(output: &str) -> Vec<Deprecation> {
    let warning = Regex::new(
        r"^w: (?P<path>(?:file://)?\S+?\.kts?):(?P<line>\d+):\d+ (?P<msg>.*\bdeprecated\b.*)$",
    )
    .expect("kotlinc pattern");
    output
        .lines()
        .filter_map(|line| {
            let c = warning.captures(line.trim())?;
            Some(Deprecation {
                path: c["path"].to_owned(),
                line: c["line"].parse().ok()?,
                code: "deprecation".into(),
                message: c["msg"].to_owned(),
                occurrence: 0,
            })
        })
        .collect()
}

fn msbuild(output: &str) -> Vec<Deprecation> {
    let warning = Regex::new(
        r"^\s*(?P<path>\S.*?\.(?:cs|fs|vb))\((?P<line>\d+),\d+\): warning (?P<code>CS0612|CS0618|SYSLIB\d{4}): (?P<msg>.+?)(?:\s+\[[^\]]+\])?$",
    )
    .expect("msbuild pattern");
    output
        .lines()
        .filter_map(|line| {
            let c = warning.captures(line)?;
            Some(Deprecation {
                path: c["path"].to_owned(),
                line: c["line"].parse().ok()?,
                code: c["code"].to_owned(),
                message: c["msg"].to_owned(),
                occurrence: 0,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/work/repo";

    fn read(format: DeprecationOutput, output: &str) -> Vec<(String, u64, String)> {
        read_deprecations(format, output, Path::new(ROOT))
            .into_iter()
            .map(|d| (d.path, d.line, d.code))
            .collect()
    }

    #[test]
    fn cargo_keeps_the_deprecated_lint_in_the_repo() {
        let message = |code: &str, file: &str, line: u64, level: &str| {
            serde_json::json!({
                "reason": "compiler-message",
                "message": {
                    "message": "use of deprecated function `old`: use `new`",
                    "level": level,
                    "code": {"code": code},
                    "spans": [
                        {"file_name": "src/other.rs", "line_start": 1, "is_primary": false},
                        {"file_name": file, "line_start": line, "is_primary": true}
                    ]
                }
            })
            .to_string()
        };
        let output = [
            message("deprecated", "src/cart.rs", 12, "warning"),
            message("deprecated", "src/total.rs", 30, "warning"),
            message("unused_variables", "src/cart.rs", 3, "warning"),
            message(
                "deprecated",
                "/home/u/.cargo/registry/x/src/lib.rs",
                5,
                "warning",
            ),
            r#"{"reason": "build-finished", "success": true}"#.into(),
            "not json".into(),
        ]
        .join("\n");
        assert_eq!(
            read(DeprecationOutput::CargoJson, &output),
            [
                ("src/cart.rs".into(), 12, "deprecated".into()),
                ("src/total.rs".into(), 30, "deprecated".into())
            ]
        );
    }

    #[test]
    fn javac_reads_maven_and_plain_javac() {
        let output = "\
[INFO] Compiling 3 source files
[WARNING] /work/repo/src/main/java/acme/Cart.java:[12,8] [deprecation] total() in acme.Old has been deprecated
src/main/java/acme/Pay.java:30: warning: [removal] charge() in acme.Gate has been deprecated and marked for removal
[WARNING] /work/repo/src/main/java/acme/Cart.java:[14,1] [unchecked] unchecked call
[WARNING] /elsewhere/Gen.java:[1,1] [deprecation] x in y has been deprecated";
        assert_eq!(
            read(DeprecationOutput::Javac, output),
            [
                (
                    "src/main/java/acme/Cart.java".into(),
                    12,
                    "deprecation".into()
                ),
                ("src/main/java/acme/Pay.java".into(), 30, "removal".into())
            ]
        );
    }

    #[test]
    fn kotlinc_reads_file_urls() {
        let output = "\
> Task :compileKotlin
w: file:///work/repo/src/main/kotlin/Cart.kt:12:5 'total(): Int' is deprecated. Use sum().
w: file:///work/repo/src/main/kotlin/Cart.kt:20:5 Parameter 'x' is never used";
        let found = read_deprecations(DeprecationOutput::Kotlinc, output, Path::new(ROOT));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/main/kotlin/Cart.kt");
        assert_eq!(found[0].message, "'total(): Int' is deprecated. Use sum().");
    }

    #[test]
    fn msbuild_reads_obsolete_warnings_once() {
        let output = "\
/work/repo/src/Cart.cs(12,5): warning CS0618: 'Old.Total()' is obsolete: 'Use Sum' [/work/repo/src/Cart.csproj]
/work/repo/src/Cart.cs(12,5): warning CS0618: 'Old.Total()' is obsolete: 'Use Sum' [/work/repo/src/Cart.csproj]
/work/repo/src/Pay.cs(3,1): warning SYSLIB0014: 'WebRequest.Create' is obsolete [/work/repo/src/Cart.csproj]
/work/repo/src/Pay.cs(4,1): warning CS8618: Non-nullable field";
        let found = read_deprecations(DeprecationOutput::Msbuild, output, Path::new(ROOT));
        let got: Vec<(&str, u64, &str)> = found
            .iter()
            .map(|d| (d.path.as_str(), d.line, d.code.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("src/Cart.cs", 12, "CS0618"),
                ("src/Pay.cs", 3, "SYSLIB0014")
            ]
        );
        assert_eq!(found[0].message, "'Old.Total()' is obsolete: 'Use Sum'");
    }

    #[test]
    fn a_key_survives_a_move_and_tells_calls_apart() {
        let d = Deprecation {
            path: "src/cart.rs".into(),
            line: 12,
            code: "deprecated".into(),
            message: "use of deprecated function `old`".into(),
            occurrence: 0,
        };
        let moved = Deprecation {
            line: 40,
            ..d.clone()
        };
        let other = Deprecation {
            path: "src/total.rs".into(),
            ..d.clone()
        };
        let again = Deprecation {
            occurrence: 1,
            ..d.clone()
        };
        assert_eq!(d.key(), moved.key());
        assert_ne!(d.key(), other.key());
        assert_ne!(d.key(), again.key(), "a second call of it in the file");
        assert!(d.key().starts_with("deprecation:"));
        assert_eq!(d.key().len(), "deprecation:".len() + 16);
    }

    #[test]
    fn paths_outside_the_repo_are_left_out() {
        let root = Path::new(ROOT);
        assert_eq!(
            relative("/work/repo/src/a.rs", root).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(relative("./src/a.rs", root).as_deref(), Some("src/a.rs"));
        assert_eq!(
            relative("file:///work/repo/a.kt", root).as_deref(),
            Some("a.kt")
        );
        assert_eq!(relative("/work/other/a.rs", root), None);
        assert_eq!(relative("../a.rs", root), None);
        assert_eq!(relative("/work/repo", root), None);
        assert_eq!(relative("/work/repository/a.rs", root), None);
    }
}
