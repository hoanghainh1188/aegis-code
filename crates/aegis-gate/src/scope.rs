//! Patch scope verification (ADR 0009, #359): checks that the patch does
//! not wander outside the scope of an issue. Detects excessive file/directory
//! touch counts, deleted test files, and newly added test skips/ignores.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::GateError;
use crate::git::{self, Change};

/// Configuration for scope enforcement and limits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePolicy {
    /// Maximum number of changed files permitted before warning or failing.
    pub max_files: Option<usize>,
    /// Maximum number of distinct directories permitted before warning or failing.
    pub max_dirs: Option<usize>,
    /// Whether deleting test files is allowed (default: false).
    pub allow_test_deletion: bool,
    /// Whether adding test skips/ignores is allowed (default: false).
    pub allow_test_skips: bool,
    /// Whether scope violations fail the gate (`true`) or only warn & flag reviewer (`false`).
    pub enforce: bool,
}

/// Known patterns that mark tests as ignored or skipped across supported languages.
const SKIP_PATTERNS: &[&str] = &[
    "#[ignore",          // Rust #[ignore] or #[ignore = "..."]
    "@pytest.mark.skip", // Python pytest skip
    "@unittest.skip",    // Python unittest skip
    "@Ignore",           // Java JUnit 4
    "@Disabled",         // Java JUnit 5
    "t.Skip(",           // Go testing
    "b.Skip(",           // Go benchmarking
    "test.skip(",        // JS/TS Jest/Vitest/Playwright
    "it.skip(",          // JS/TS Mocha/Jest/Vitest
    "describe.skip(",    // JS/TS Mocha/Jest/Vitest
    "xit(",              // JS/TS Mocha/Jest
    "xdescribe(",        // JS/TS Mocha/Jest
    "[Ignore",           // C# / NUnit / MSTest
];

/// Checks if a file path represents a test file based on conventions.
pub fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();

    // Check if in test directory
    let in_test_dir = parts.iter().any(|&p| {
        p == "test" || p == "tests" || p == "__tests__" || p == "testdata" || p == "testing"
    });

    if in_test_dir {
        return true;
    }

    // Check file name patterns
    if let Some(&file_name) = parts.last()
        && (file_name.starts_with("test_")
            || file_name.ends_with("_test.rs")
            || file_name.ends_with("_test.go")
            || file_name.ends_with("_test.py")
            || file_name.ends_with(".test.ts")
            || file_name.ends_with(".test.js")
            || file_name.ends_with(".test.tsx")
            || file_name.ends_with(".test.jsx")
            || file_name.ends_with(".spec.ts")
            || file_name.ends_with(".spec.js")
            || file_name.ends_with(".spec.tsx")
            || file_name.ends_with(".spec.jsx")
            || file_name.ends_with("test.java")
            || file_name.ends_with("tests.java")
            || file_name.ends_with("test.cs")
            || file_name.ends_with("tests.cs"))
    {
        return true;
    }

    false
}

/// Directory of a path (empty string if at root).
fn directory_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) => &path[..pos],
        None => "",
    }
}

/// Result of a scope analysis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeViolations {
    pub file_count: usize,
    pub dir_count: usize,
    pub file_limit_exceeded: Option<(usize, usize)>, // (actual, limit)
    pub dir_limit_exceeded: Option<(usize, usize)>,  // (actual, limit)
    pub deleted_test_files: Vec<String>,
    pub added_test_skips: Vec<(String, String)>, // (file, pattern)
    pub warnings: Vec<String>,
}

impl ScopeViolations {
    pub fn is_empty(&self) -> bool {
        self.file_limit_exceeded.is_none()
            && self.dir_limit_exceeded.is_none()
            && self.deleted_test_files.is_empty()
            && self.added_test_skips.is_empty()
            && self.warnings.is_empty()
    }

    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        if let Some((actual, limit)) = self.file_limit_exceeded {
            lines.push(format!("changed files ({actual}) exceeded limit ({limit})"));
        }
        if let Some((actual, limit)) = self.dir_limit_exceeded {
            lines.push(format!(
                "changed directories ({actual}) exceeded limit ({limit})"
            ));
        }
        if !self.deleted_test_files.is_empty() {
            lines.push(format!(
                "deleted test files ({}): {}",
                self.deleted_test_files.len(),
                self.deleted_test_files.join(", ")
            ));
        }
        if !self.added_test_skips.is_empty() {
            let skips: Vec<String> = self
                .added_test_skips
                .iter()
                .map(|(f, p)| format!("`{f}`: `{p}`"))
                .collect();
            lines.push(format!(
                "added test skip/ignore markers ({}): {}",
                self.added_test_skips.len(),
                skips.join(", ")
            ));
        }
        for w in &self.warnings {
            lines.push(w.clone());
        }
        lines.join("; ")
    }
}

/// Analyzes the scope of changes against policy.
pub fn analyze_scope(
    repo: &Path,
    base: &str,
    changes: &[Change],
    policy: &ScopePolicy,
) -> Result<ScopeViolations, GateError> {
    let mut violations = ScopeViolations {
        file_count: changes.len(),
        ..Default::default()
    };

    // 1. Check file limit
    if let Some(max_files) = policy.max_files
        && changes.len() > max_files
    {
        violations.file_limit_exceeded = Some((changes.len(), max_files));
        violations.warnings.push(format!(
            "patch changed {} files, exceeding limit of {}",
            changes.len(),
            max_files
        ));
    }

    // 2. Check directory limit
    let mut dirs = BTreeSet::new();
    for c in changes {
        dirs.insert(directory_of(&c.path));
    }
    violations.dir_count = dirs.len();
    if let Some(max_dirs) = policy.max_dirs
        && dirs.len() > max_dirs
    {
        violations.dir_limit_exceeded = Some((dirs.len(), max_dirs));
        violations.warnings.push(format!(
            "patch touched {} distinct directories, exceeding limit of {}",
            dirs.len(),
            max_dirs
        ));
    }

    // 3. Check for deleted test files & added test skips
    for change in changes {
        let is_test = is_test_file(&change.path);
        let current_path = repo.join(&change.path);
        let exists_now = current_path.exists();

        // Check if deleted
        if !exists_now {
            if is_test && !policy.allow_test_deletion {
                violations.deleted_test_files.push(change.path.clone());
                violations.warnings.push(format!(
                    "deleted test file `{}` is not allowed by patch scope policy",
                    change.path
                ));
            }
            continue;
        }

        // Check if test skips were added (only for test files or any source file)
        if !policy.allow_test_skips {
            let base_content = git::show(repo, base, &change.path)?.unwrap_or_default();
            let current_content = std::fs::read_to_string(&current_path).unwrap_or_default();

            for &pattern in SKIP_PATTERNS {
                let count_base = base_content.matches(pattern).count();
                let count_curr = current_content.matches(pattern).count();
                if count_curr > count_base {
                    violations
                        .added_test_skips
                        .push((change.path.clone(), pattern.to_owned()));
                    violations.warnings.push(format!(
                        "added test skip/ignore pattern `{pattern}` in `{}`",
                        change.path
                    ));
                }
            }
        }
    }

    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_test_files_across_ecosystems() {
        assert!(is_test_file("tests/foo.rs"));
        assert!(is_test_file("crates/aegis-gate/tests/rust_gate.rs"));
        assert!(is_test_file("test/test_api.py"));
        assert!(is_test_file("src/foo_test.go"));
        assert!(is_test_file("test/TestServer.java"));
        assert!(is_test_file("src/__tests__/app.test.tsx"));
        assert!(is_test_file("src/component.spec.js"));
        assert!(is_test_file("Tests/Services/AuthTest.cs"));

        assert!(!is_test_file("src/lib.rs"));
        assert!(!is_test_file("src/main.py"));
        assert!(!is_test_file("cmd/server/main.go"));
    }

    #[test]
    fn detects_directory_counts() {
        assert_eq!(directory_of("Cargo.toml"), "");
        assert_eq!(directory_of("src/lib.rs"), "src");
        assert_eq!(directory_of("crates/foo/src/lib.rs"), "crates/foo/src");
    }
}
