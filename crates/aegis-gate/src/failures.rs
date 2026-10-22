//! Failing test names from a test run's output, per the profile's
//! [`TestOutput`], so the gate can tell new failures from ones the base
//! branch already has.

use std::collections::BTreeSet;

use aegis_lang::{CustomHarness, TestOutput};
use regex::Regex;

pub fn failing_tests(format: TestOutput, output: &str) -> BTreeSet<String> {
    failing_tests_with(format, None, output)
}

/// A harness's own lines (typst's `❌ name (file:line)`), compiled; the
/// profile checked them when it was resolved.
struct Harness {
    failure: Regex,
    finished: Option<Regex>,
}

impl Harness {
    fn of(custom: &CustomHarness) -> Option<Self> {
        Some(Self {
            failure: Regex::new(&custom.failure).ok()?,
            finished: match &custom.finished {
                Some(f) => Some(Regex::new(f).ok()?),
                None => None,
            },
        })
    }

    fn failed<'a>(&self, line: &'a str) -> Option<&'a str> {
        self.failure.captures(line)?.get(1).map(|m| m.as_str())
    }

    fn finished(&self, line: &str) -> bool {
        self.finished.as_ref().is_some_and(|f| f.is_match(line))
    }
}

/// [`failing_tests`], with a test binary's own harness read on top.
pub fn failing_tests_with(
    format: TestOutput,
    custom: Option<&CustomHarness>,
    output: &str,
) -> BTreeSet<String> {
    let harness = custom.and_then(Harness::of);
    match format {
        TestOutput::Libtest => libtest(output, harness.as_ref()),
        TestOutput::Pytest => {
            let mut failed = pytest(output);
            if let Some(h) = &harness {
                failed.extend(
                    output
                        .lines()
                        .filter_map(|l| h.failed(l))
                        .map(str::to_owned),
                );
            }
            failed
        }
    }
}

/// Stands for the tests of a run that stopped before reporting (a crash,
/// an abort, `exit()` in a test): they count as failing.
pub const DID_NOT_FINISH: &str = "<did not finish>";

/// `test name ... FAILED`, named after the test binary that ran it
/// (`Running … (target/debug/deps/<binary>-<hash>)`, `Doc-tests <crate>`)
/// so tests with the same path in two crates stay apart. A binary that
/// started but printed no `test result:` line did not finish, unless the
/// harness's own end line came.
fn libtest(output: &str, harness: Option<&Harness>) -> BTreeSet<String> {
    let mut binary = String::new();
    let mut open = false;
    let mut failed = BTreeSet::new();
    let close = |binary: &str, open: &mut bool, failed: &mut BTreeSet<String>| {
        if std::mem::take(open) {
            failed.insert(format!("{binary}::{DID_NOT_FINISH}"));
        }
    };
    for line in output.lines().map(str::trim) {
        if line.starts_with("Running ") || line.starts_with("Doc-tests ") {
            close(&binary, &mut open, &mut failed);
            open = true;
        }
        if line.starts_with("test result: ") || harness.is_some_and(|h| h.finished(line)) {
            open = false;
        }
        if let Some(name) = harness.and_then(|h| h.failed(line)) {
            failed.insert(format!("{binary}::{name}"));
            continue;
        }
        if let Some(rest) = line.strip_prefix("Running ") {
            binary = rest
                .rsplit_once('(')
                .and_then(|(_, p)| p.strip_suffix(')'))
                .and_then(|p| p.rsplit('/').next())
                .map(|file| file.rsplit_once('-').map_or(file, |(name, _)| name))
                .unwrap_or(rest)
                .to_owned();
        } else if let Some(name) = line.strip_prefix("Doc-tests ") {
            binary = format!("doc:{name}");
        } else if let Some(name) = line
            .strip_prefix("test ")
            .and_then(|l| l.strip_suffix(" ... FAILED"))
        {
            failed.insert(format!("{binary}::{name}"));
        }
    }
    close(&binary, &mut open, &mut failed);
    failed
}

/// pytest's last line: `1 failed, 2 passed in 0.12s`, with or without the
/// `===` around it, and with `(0:01:13)` after runs over a minute.
fn pytest_finished(output: &str) -> bool {
    output
        .lines()
        .rev()
        .map(|l| l.trim().trim_matches('=').trim())
        .map(|l| match l.rsplit_once(" (") {
            Some((head, clock)) if clock.ends_with(')') && clock.contains(':') => head,
            _ => l,
        })
        .any(|l| {
            l.contains(" in ")
                && l.ends_with('s')
                && [
                    "passed",
                    "failed",
                    "error",
                    "skipped",
                    "deselected",
                    "no tests ran",
                ]
                .iter()
                .any(|w| l.contains(w))
        })
}

/// The short test summary: `FAILED <id>` and `ERROR <id>`, with an
/// optional ` - <message>`. Only after its header when there is one: a
/// test's captured log has lines like `ERROR    scrapy.core:engine.py:…`
/// too, with addresses that differ from run to run (#104).
fn pytest(output: &str) -> BTreeSet<String> {
    let summary = output
        .rfind("short test summary info")
        .map_or(output, |at| &output[at..]);
    let mut failed: BTreeSet<String> = summary
        .lines()
        .filter(|l| !l.starts_with("FAILED  ") && !l.starts_with("ERROR  "))
        .filter_map(|l| {
            l.strip_prefix("FAILED ")
                .or_else(|| l.strip_prefix("ERROR "))
        })
        .map(|rest| {
            rest.split_once(" - ")
                .map_or(rest, |(id, _)| id)
                .trim()
                .to_owned()
        })
        .filter(|id| !id.is_empty())
        .collect();
    if !pytest_finished(output) {
        failed.insert(format!("pytest::{DID_NOT_FINISH}"));
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libtest_names_failures_by_binary() {
        let out = "\
   Compiling cart v0.1.0
     Running unittests src/lib.rs (target/debug/deps/cart-1a2b3c)
test tests::adds_up ... ok
test tests::refuses ... FAILED
test result: FAILED. 1 passed; 1 failed
     Running tests/extra.rs (target/debug/deps/extra-9f8e)
test refuses ... FAILED
test slow ... ignored
test result: FAILED. 0 passed; 1 failed; 1 ignored
test result: FAILED. 1 passed; 1 failed
   Doc-tests cart
test src/lib.rs - Cart (line 3) ... FAILED
test result: FAILED. 0 passed; 1 failed

failures:
    tests::refuses
";
        assert_eq!(
            libtest(out, None).into_iter().collect::<Vec<_>>(),
            [
                "cart::tests::refuses",
                "doc:cart::src/lib.rs - Cart (line 3)",
                "extra::refuses"
            ]
        );
    }

    #[test]
    fn pytest_reads_the_short_summary() {
        let out = "\
tests/test_a.py .F.E
=========================== short test summary info ============================
FAILED tests/test_a.py::test_b - AssertionError: 1 != 2
FAILED tests/test_a.py::TestC::test_d[param-1]
ERROR tests/test_a.py::test_e - fixture 'db' not found
ERROR tests/test_broken.py - ImportError: no module
1 failed, 2 passed, 1 error in 0.12s
";
        assert_eq!(
            pytest(out).into_iter().collect::<Vec<_>>(),
            [
                "tests/test_a.py::TestC::test_d[param-1]",
                "tests/test_a.py::test_b",
                "tests/test_a.py::test_e",
                "tests/test_broken.py"
            ]
        );
    }

    #[test]
    fn a_custom_harness_is_read_on_top_of_libtest() {
        let typst = CustomHarness {
            failure: r"^❌ (\S+) \(".into(),
            finished: Some(r"^\d+ passed, \d+ failed, \d+ skipped$".into()),
        };
        let out = "\
     Running unittests src/lib.rs (target/debug/deps/typst_syntax-1a2b)
test span::tests::ok ... ok
test span::tests::bad ... FAILED
test result: FAILED. 1 passed; 1 failed
     Running src/tests.rs (target/debug/deps/tests-ee91)
❌ import-nested-item (tests/suite/scripting/import.typ:49)
3633 passed, 1 failed, 0 skipped
thread 'main' panicked at tests/src/report/mod.rs:75:79:
";
        assert_eq!(
            failing_tests_with(TestOutput::Libtest, Some(&typst), out)
                .into_iter()
                .collect::<Vec<_>>(),
            [
                "tests::import-nested-item",
                "typst_syntax::span::tests::bad"
            ]
        );
        // Without its end line the harness's binary did not finish.
        assert!(failing_tests(TestOutput::Libtest, out).contains("tests::<did not finish>"));
    }

    #[test]
    fn a_pytest_run_over_a_minute_finished() {
        let out = "FAILED tests/a.py::t\n= 5 failed, 293 passed, 10 rerun in 73.06s (0:01:13) =\n";
        assert_eq!(
            pytest(out).into_iter().collect::<Vec<_>>(),
            ["tests/a.py::t"]
        );
    }

    #[test]
    fn pytest_ignores_captured_log_lines() {
        let out = "\
------------------------------ Captured log call -------------------------------
ERROR    scrapy.downloadermiddlewares.retry:retry.py:130 Gave up retrying <GET https://x/> 0xffff9e18
=========================== short test summary info ============================
FAILED tests/test_a.py::test_b - AssertionError
ERROR    scrapy.core.scraper:scraper.py:9 in the summary section too
1 failed, 2 passed in 0.12s
";
        assert_eq!(
            pytest(out).into_iter().collect::<Vec<_>>(),
            ["tests/test_a.py::test_b"]
        );
    }

    #[test]
    fn a_run_that_stops_before_its_summary_counts_as_failing() {
        let crashed = "\
     Running unittests src/lib.rs (target/debug/deps/cart-1a2b3c)
test tests::adds_up ... ok
test result: ok. 1 passed; 0 failed
     Running tests/extra.rs (target/debug/deps/extra-9f8e)
test aborts ...
error: test failed, to rerun pass `--test extra`
";
        assert_eq!(
            libtest(crashed, None).into_iter().collect::<Vec<_>>(),
            ["extra::<did not finish>"]
        );
        assert_eq!(
            pytest("tests/test_a.py ..Fatal Python error: Segmentation fault\n")
                .into_iter()
                .collect::<Vec<_>>(),
            ["pytest::<did not finish>"]
        );
        assert!(pytest("==== 3 passed in 0.10s ====\n").is_empty());
        assert!(pytest("3 passed, 1 skipped in 2.01s\n").is_empty());
    }
}
