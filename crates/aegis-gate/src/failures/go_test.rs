//! `go test ./...` without `-v`: each package's output comes in one block,
//! its `--- FAIL: <test>` lines first and `FAIL <package>` or `ok <package>`
//! last.

use std::collections::BTreeSet;

use regex::Regex;

use super::DID_NOT_FINISH;

/// `<package>::<test>`, a subtest as `<test>/<sub>`. A package that failed
/// without naming a test (a panic outside a test, a timeout) did not
/// finish; one that did not build is `<package>::<build failed>`.
pub(super) fn failing(output: &str) -> BTreeSet<String> {
    let mut failed = BTreeSet::new();
    let mut tests: Vec<&str> = Vec::new();
    for line in output.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("--- FAIL: ") {
            tests.push(rest.rsplit_once(" (").map_or(rest, |(name, _)| name));
        } else if let Some(rest) = line.strip_prefix("FAIL\t") {
            // `FAIL <package> [build failed]`, `FAIL <package>\t<time>`.
            if let Some((package, why)) = rest.split_once(" [") {
                failed.insert(format!("{package}::<{}>", why.trim_end_matches(']')));
            } else {
                let package = rest.split('\t').next().unwrap_or(rest);
                if tests.is_empty() {
                    failed.insert(format!("{package}::{DID_NOT_FINISH}"));
                }
                failed.extend(tests.iter().map(|t| format!("{package}::{t}")));
            }
            tests.clear();
        } else if line.starts_with("ok ") && line.contains('\t') {
            tests.clear();
        }
    }
    if !tests.is_empty() {
        failed.insert(format!("go::{DID_NOT_FINISH}"));
    }
    failed
}

/// Whether `id` is a test `go test -run <test>` selects: the pattern is cut
/// at `/`, each piece a regex matched against one level of the name.
pub(super) fn names(id: &str, test: &str) -> bool {
    let Some((_, name)) = id.split_once("::") else {
        return false;
    };
    if name.starts_with('<') {
        return false;
    }
    let levels: Vec<&str> = name.split('/').collect();
    let pieces: Vec<&str> = test.split('/').collect();
    levels.len() >= pieces.len()
        && pieces.iter().zip(&levels).all(|(piece, level)| {
            Regex::new(piece).map_or_else(|_| level.contains(piece), |re| re.is_match(level))
        })
}

/// `-run ^(TestA|TestB)$`: the top-level tests, each with its subtests.
pub(super) fn selector(ids: &BTreeSet<String>) -> Option<String> {
    let tests: BTreeSet<String> = ids
        .iter()
        .filter_map(|id| id.split_once("::"))
        .map(|(_, name)| regex::escape(name.split('/').next().unwrap_or(name)))
        .collect();
    let tests: Vec<String> = tests.into_iter().collect();
    (!tests.is_empty()).then(|| format!("^({})$", tests.join("|")))
}

/// Every package printed `ok … [no tests to run]` or `? … [no test files]`.
pub(super) fn ran_nothing(output: &str) -> bool {
    !output.lines().any(|line| {
        (line.starts_with("ok ") && !line.contains("[no tests to run]"))
            || line.starts_with("FAIL")
            || line.trim_start().starts_with("--- ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = include_str!("../../tests/fixtures/test-output/go-test.log");

    #[test]
    fn reads_failures_per_package() {
        assert_eq!(
            failing(LOG).into_iter().collect::<Vec<_>>(),
            [
                "example.com/m/a::TestFail",
                "example.com/m/a::TestSub",
                "example.com/m/a::TestSub/bad_case",
                "example.com/m/a::TestSub/bad_case/deep",
                "example.com/m/b::TestPanic",
                "example.com/m/c::<build failed>",
            ]
        );
    }

    #[test]
    fn a_package_failing_without_a_test_did_not_finish() {
        let out = "\
panic: test timed out after 10m0s
	running tests:
		TestSlow (10m0s)
FAIL	example.com/m/slow	600.012s
ok  	example.com/m/a	0.002s
FAIL
";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["example.com/m/slow::<did not finish>"]
        );
        assert!(failing("ok  \texample.com/m/a\t0.002s\n").is_empty());
    }

    #[test]
    fn selects_the_top_level_tests() {
        let ids: BTreeSet<String> = [
            "example.com/m/a::TestSub/bad_case/deep",
            "example.com/m/a::TestSub",
            "example.com/m/b::TestPanic",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(selector(&ids).as_deref(), Some("^(TestPanic|TestSub)$"));
    }

    #[test]
    fn tells_a_run_of_nothing() {
        assert!(ran_nothing(
            "ok  \texample.com/m/a\t0.002s [no tests to run]\n?   \texample.com/m/d\t[no test files]\n"
        ));
        assert!(!ran_nothing("ok  \texample.com/m/a\t0.002s\n"));
        assert!(!ran_nothing("ok  \texample.com/m/a\t(cached)\n"));
    }

    #[test]
    fn matches_the_run_pattern_level_by_level() {
        let id = "example.com/m/a::TestSub/bad_case/deep";
        assert!(names(id, "TestSub"));
        assert!(names(id, "^TestSub$/bad"));
        assert!(names(id, "TestSub/bad_case/deep"));
        assert!(names("example.com/m/a::TestFail", "TestPass|TestFail"));
        assert!(!names("example.com/m/a::TestSub", "TestSub/bad_case"));
        assert!(!names("example.com/m/a::TestFail", "^TestFai$"));
        assert!(!names("example.com/m/c::<build failed>", "TestBroken"));
    }
}
