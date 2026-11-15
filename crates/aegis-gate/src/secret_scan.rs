//! The gate's `secrets` step (#509): the lines a patch adds are scanned
//! with the platform's secret rules ([`crate::secrets`], the same that hide
//! values from the model). A hardcoded API key, a token in a sample config
//! or a private key fails the step, named by file, line and kind; the value
//! itself is never written anywhere. A repo may let some through, by path
//! and kind, with a reason (fixtures with fake keys).

use std::collections::HashMap;
use std::path::Path;

use aegis_lang::glob_match;
use serde::{Deserialize, Serialize};

use crate::GateError;
use crate::git::{self, Change};
use crate::secrets::{is_sensitive_path, kinds_on};

/// The longest exception reason kept; it is shown on the step.
const MAX_REASON_CHARS: usize = 200;

/// What the repo lets through.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretPolicy {
    #[serde(default)]
    pub allow: Vec<SecretException>,
}

/// Secrets the step lets through in `path`: of `kinds`, or of any kind
/// when it names none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretException {
    /// A path from the repo root; `*` matches any run of characters,
    /// `/` included (`tests/fixtures/*`).
    pub path: String,
    /// `aws-access-key`, `private-key`, `high-entropy`…; empty for any.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Why these are not real secrets; required.
    pub reason: String,
}

impl SecretPolicy {
    /// Every exception has a path and a reason.
    pub fn check(&self) -> Result<(), String> {
        for (i, e) in self.allow.iter().enumerate() {
            if e.path.trim().is_empty() {
                return Err(format!("repo.secrets.allow[{i}].path must not be empty"));
            }
            let reason = e.reason.trim();
            if reason.is_empty() {
                return Err(format!(
                    "repo.secrets.allow[{i}].reason must say why `{}` holds no real secret",
                    e.path
                ));
            }
            if reason.chars().count() > MAX_REASON_CHARS {
                return Err(format!(
                    "repo.secrets.allow[{i}].reason: at most {MAX_REASON_CHARS} characters"
                ));
            }
        }
        Ok(())
    }

    fn allows(&self, path: &str, kind: &str) -> bool {
        self.allow.iter().any(|e| {
            glob_match(&e.path, path) && (e.kinds.is_empty() || e.kinds.iter().any(|k| k == kind))
        })
    }
}

/// A secret the patch adds: where, and of what kind. Never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    pub path: String,
    /// 1-based, in the patched file.
    pub line: usize,
    pub kind: &'static str,
}

/// What the scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretScan {
    /// Secrets that fail the step.
    pub found: Vec<SecretFinding>,
    /// Secrets an exception let through.
    pub allowed: usize,
}

impl SecretScan {
    /// One line per secret found, then how many were let through.
    pub fn summary(&self) -> String {
        let mut lines: Vec<String> = self
            .found
            .iter()
            .map(|f| format!("{}:{}: {}", f.path, f.line, f.kind))
            .collect();
        if self.allowed > 0 {
            lines.push(format!(
                "{} let through by the repo's `secrets.allow`",
                self.allowed
            ));
        }
        lines.join("\n")
    }
}

/// The lines of `current` that `base` does not have, with their 1-based
/// numbers: a line moved within the file is not new.
fn added_lines<'a>(base: &str, current: &'a str) -> Vec<(usize, &'a str)> {
    let mut left: HashMap<&str, usize> = HashMap::new();
    for line in base.lines() {
        *left.entry(line).or_default() += 1;
    }
    current
        .lines()
        .enumerate()
        .filter(|(_, line)| match left.get_mut(line) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .map(|(i, line)| (i + 1, line))
        .collect()
}

/// The secrets in the lines `base`'s `path` does not have and `current`
/// does. A new key or `.env` file counts whole, once.
fn scan_file(path: &str, base: Option<&str>, current: &str) -> Vec<SecretFinding> {
    let mut found = Vec::new();
    if base.is_none() && is_sensitive_path(path) && !current.trim().is_empty() {
        found.push(SecretFinding {
            path: path.to_owned(),
            line: 1,
            kind: "sensitive-file",
        });
    }
    for (line, text) in added_lines(base.unwrap_or_default(), current) {
        for kind in kinds_on(text) {
            found.push(SecretFinding {
                path: path.to_owned(),
                line,
                kind,
            });
        }
    }
    found
}

/// Scans what `changes` add to `repo` since `base`; a file that is gone,
/// a submodule or not text is skipped.
pub fn scan_patch(
    repo: &Path,
    base: &str,
    changes: &[Change],
    policy: &SecretPolicy,
) -> Result<SecretScan, GateError> {
    let mut scan = SecretScan::default();
    for change in changes.iter().filter(|c| !c.gitlink) {
        let Ok(current) = std::fs::read_to_string(repo.join(&change.path)) else {
            continue;
        };
        let before = git::show(repo, base, &change.path)?;
        for finding in scan_file(&change.path, before.as_deref(), &current) {
            if policy.allows(&finding.path, finding.kind) {
                scan.allowed += 1;
            } else {
                scan.found.push(finding);
            }
        }
    }
    Ok(scan)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built at run time, so no secret-shaped string sits in the source.
    fn aws_key() -> String {
        format!("AKIA{}", "QWERTYUIOP234567")
    }

    #[test]
    fn only_added_lines_count() {
        let base = format!("a\nkey = \"{}\"\nb\n", aws_key());
        let moved = format!("b\nkey = \"{}\"\na\nc\n", aws_key());
        assert!(scan_file("src/app.py", Some(&base), &moved).is_empty());
        let added = format!("{base}token: {}\n", aws_key());
        assert_eq!(
            scan_file("src/app.py", Some(&base), &added),
            [SecretFinding {
                path: "src/app.py".into(),
                line: 4,
                kind: "aws-access-key"
            }]
        );
    }

    #[test]
    fn a_private_key_and_a_new_env_file_are_found() {
        let pem = format!(
            "-----BEGIN RSA {}-----\nMIIB\n-----END RSA {}-----\n",
            "PRIVATE KEY", "PRIVATE KEY"
        );
        let found = scan_file("certs/server.txt", None, &pem);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].line, found[0].kind), (1, "private-key"));
        let env = scan_file(".env", None, "DEBUG=1\n");
        assert_eq!(env[0].kind, "sensitive-file");
        assert!(scan_file(".env.example", None, "DEBUG=1\n").is_empty());
    }

    #[test]
    fn the_summary_names_place_and_kind_never_the_value() {
        let scan = SecretScan {
            found: scan_file("a.rs", None, &format!("let k = \"{}\";\n", aws_key())),
            allowed: 2,
        };
        let text = scan.summary();
        assert!(text.contains("a.rs:1: aws-access-key"), "{text}");
        assert!(text.contains("2 let through"));
        assert!(!text.contains(&aws_key()));
    }

    #[test]
    fn exceptions_match_by_path_and_kind_and_need_a_reason() {
        let policy = SecretPolicy {
            allow: vec![SecretException {
                path: "tests/fixtures/*".into(),
                kinds: vec!["private-key".into()],
                reason: "fake keys for the TLS tests".into(),
            }],
        };
        assert!(policy.check().is_ok());
        assert!(policy.allows("tests/fixtures/tls/key.pem", "private-key"));
        assert!(!policy.allows("tests/fixtures/tls/key.pem", "aws-access-key"));
        assert!(!policy.allows("src/key.pem", "private-key"));
        let any = SecretPolicy {
            allow: vec![SecretException {
                kinds: vec![],
                ..policy.allow[0].clone()
            }],
        };
        assert!(any.allows("tests/fixtures/a.txt", "high-entropy"));
        let silent = SecretPolicy {
            allow: vec![SecretException {
                reason: " ".into(),
                ..policy.allow[0].clone()
            }],
        };
        assert!(silent.check().is_err());
    }
}
