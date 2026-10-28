//! Whether a file is a test by its path, in any of the profiles' languages:
//! the code graph's impact note (#203) and the module map (#269) leave
//! tests out.

/// `relative` is a path from the repo root, with `/` separators.
pub fn is_test_path(relative: &str) -> bool {
    let mut dirs: Vec<&str> = relative.split('/').collect();
    let name = dirs.pop().unwrap_or(relative);
    let stem = name.split('.').next().unwrap_or(name);
    let test_dir = |dir: &&str| {
        matches!(*dir, "tests" | "test" | "__tests__")
            || dir.ends_with(".Tests")
            || dir.ends_with(".Test")
    };
    dirs.iter().any(test_dir)
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || name.contains(".test.")
        || name.contains(".spec.")
}

#[cfg(test)]
mod tests {
    use super::is_test_path;

    #[test]
    fn each_languages_test_layout_is_a_test() {
        for path in [
            "tests/fuzz/src/main.rs",
            "crates/typst-cli/tests/run.rs",
            "scrapy/test_utils.py",
            "pkg/gin/context_test.go",
            "src/test/java/com/google/gson/GsonTest.java",
            "src/__tests__/parse.ts",
            "packages/zod/src/string.test.ts",
            "src/Polly.Core.Tests/Retry/RetryTests.cs",
            "tests/src/report",
        ] {
            assert!(is_test_path(path), "{path}");
        }
    }

    #[test]
    fn source_files_are_not_tests() {
        for path in [
            "crates/typst-library/src/layout/grid.rs",
            "scrapy/http/request/__init__.py",
            "src/main/java/com/google/gson/Gson.java",
            "packages/zod/src/types.ts",
            "src/Polly.Core/Retry/RetryStrategyOptions.cs",
            "contest/score.py",
            "latest.rs",
        ] {
            assert!(!is_test_path(path), "{path}");
        }
    }
}
