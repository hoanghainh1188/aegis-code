//! Failing test names from PHPUnit output (#37).

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

/// PHPUnit lists failing tests as `1) Namespace\ClassName::testMethod`.
fn entry_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\d+\)\s+([A-Za-z_\\][A-Za-z0-9_\\]*::[A-Za-z_][A-Za-z0-9_]*)")
            .expect("valid phpunit regex")
    })
}

pub(crate) fn failing(output: &str) -> BTreeSet<String> {
    let mut failed = BTreeSet::new();
    let re = entry_re();
    let mut in_section = false;
    for raw in output.lines() {
        let line = raw.trim();
        if line.starts_with("There was ") || line.starts_with("There were ") {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(m) = re.captures(line).and_then(|cap| cap.get(1)) {
            failed.insert(m.as_str().to_owned());
        }
    }
    failed
}

/// Whether `id` (`Namespace\Class::method`) matches the `--filter` `test`
/// passed to PHPUnit.
pub(crate) fn names(id: &str, test: &str) -> bool {
    let method = id.split_once("::").map_or(id, |(_, m)| m);
    let class = id.split_once("::").map_or(id, |(c, _)| c);
    let short_class = class.rsplit('\\').next().unwrap_or(class);
    test == id
        || test == method
        || test == class
        || test == short_class
        || test == format!("{short_class}::{method}")
}

/// PHPUnit `--filter` accepts a PCRE regex; a single `Class::method` can be
/// escaped and anchored or passed as `Class::method`.
pub(crate) fn selector(ids: &BTreeSet<String>) -> Option<String> {
    if ids.len() != 1 {
        return None;
    }
    ids.first().cloned()
}

pub(crate) fn ran_nothing(output: &str) -> bool {
    output.contains("No tests executed!")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_failing_phpunit_tests() {
        let out = "\
PHPUnit 11.3.1 by Sebastian Bergmann and contributors.

F.                                                                  2 / 2 (100%)

Time: 00:00.012, Memory: 8.00 MB

There was 1 failure:

1) Tests\\Unit\\CartTest::testAddsItem
Failed asserting that 0 matches expected 1.

FAILURES!
Tests: 2, Assertions: 2, Failures: 1.
";
        let got = failing(out);
        assert_eq!(
            got,
            BTreeSet::from(["Tests\\Unit\\CartTest::testAddsItem".to_owned()])
        );
        assert!(names(
            "Tests\\Unit\\CartTest::testAddsItem",
            "CartTest::testAddsItem"
        ));
        assert!(names("Tests\\Unit\\CartTest::testAddsItem", "testAddsItem"));
        assert!(!ran_nothing(out));
    }

    #[test]
    fn detects_no_tests_executed() {
        assert!(ran_nothing("PHPUnit 11.3.1\n\nNo tests executed!\n"));
    }
}
