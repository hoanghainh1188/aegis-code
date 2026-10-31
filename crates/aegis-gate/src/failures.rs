//! Failing test names from a test run's output, per the profile's
//! [`TestOutput`], so the gate can tell new failures from ones the base
//! branch already has.

mod dotnet;
mod go_test;
mod js;
mod surefire;

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
        TestOutput::GoTest => go_test::failing(output),
        TestOutput::Surefire => surefire::failing(output),
        TestOutput::JestVitest => js::failing(output),
        TestOutput::Dotnet => dotnet::failing(output),
    }
}

/// Whether `id`, a failing test read in `format`, is the test the repro
/// step ran, `test` given as that runner's own selector (`-run` regex,
/// `Class#method`, `--filter` expression…), or one inside it.
pub(crate) fn names_test(format: TestOutput, id: &str, test: &str) -> bool {
    match format {
        TestOutput::Libtest | TestOutput::Pytest => crate::gate::is_in(id, test),
        TestOutput::GoTest => go_test::names(id, test),
        TestOutput::Surefire => surefire::names(id, test),
        TestOutput::JestVitest => js::names(id, test),
        TestOutput::Dotnet => dotnet::names(id, test),
    }
}

/// One value for the profile's `repro_test` `{test}` that runs `ids`, and
/// nothing much else, again (#306); `None` when the runner cannot name
/// them all in one: the gate then reruns the whole suite.
pub(crate) fn selector(format: TestOutput, ids: &BTreeSet<String>) -> Option<String> {
    if ids.is_empty() || ids.iter().any(|id| id.contains('<')) {
        return None;
    }
    match format {
        TestOutput::Libtest => single(ids).map(|id| {
            id.split_once("::")
                .map_or(id.as_str(), |(_, name)| name)
                .to_owned()
        }),
        TestOutput::Pytest => single(ids).cloned(),
        TestOutput::GoTest => go_test::selector(ids),
        TestOutput::Surefire => surefire::selector(ids),
        TestOutput::JestVitest => js::selector(ids),
        TestOutput::Dotnet => dotnet::selector(ids),
    }
}

fn single(ids: &BTreeSet<String>) -> Option<&String> {
    (ids.len() == 1).then(|| ids.first()).flatten()
}

/// Whether a run that exited 0 shows it ran no test at all: a selector
/// that matched nothing (libtest's `running 0 tests`, Go's `[no tests to
/// run]`, VSTest's "No test matches"). The others exit non-zero then.
pub(crate) fn ran_nothing(format: TestOutput, output: &str) -> bool {
    match format {
        TestOutput::Libtest => ran_tests(format, output).is_some_and(|ran| ran.is_empty()),
        TestOutput::GoTest => go_test::ran_nothing(output),
        TestOutput::Dotnet => dotnet::ran_nothing(output),
        TestOutput::Pytest | TestOutput::Surefire | TestOutput::JestVitest => false,
    }
}

/// Whether a run of `ids` alone shows it did not run them all: it ran
/// nothing, or libtest, which names every test it runs, left one out.
pub(crate) fn missed(format: TestOutput, output: &str, ids: &BTreeSet<String>) -> bool {
    let name = |id: &str| -> String { id.split_once("::").map_or(id, |(_, n)| n).to_owned() };
    ran_nothing(format, output)
        || ran_tests(format, output)
            .is_some_and(|ran| ids.iter().any(|id| !ran.contains(&name(id))))
}

/// `line` without ANSI escapes: Vitest and Jest color their output even
/// when it goes to a file.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Every test a run reports, whatever its result, when the format names
/// them all: libtest's `test <name> ... <result>`. `None` for the others,
/// whose output names only the tests that fail.
pub fn ran_tests(format: TestOutput, output: &str) -> Option<BTreeSet<String>> {
    match format {
        TestOutput::Libtest => Some(
            output
                .lines()
                .map(str::trim)
                .filter_map(|l| l.strip_prefix("test "))
                .filter_map(|l| l.split_once(" ..."))
                .map(|(name, _)| name.to_owned())
                .collect(),
        ),
        // These name only the tests that fail.
        TestOutput::Pytest
        | TestOutput::GoTest
        | TestOutput::Surefire
        | TestOutput::JestVitest
        | TestOutput::Dotnet => None,
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
    fn libtest_lists_every_test_it_ran() {
        let out = "\
$ cargo test --workspace -- --exact issue_tests::refuses
     Running unittests src/lib.rs (target/debug/deps/cart-1a2b3c)
running 2 tests
test issue_tests::refuses ... ok
test tests::slow ... ignored
test result: ok. 1 passed; 0 failed; 1 ignored
     Running tests/extra.rs (target/debug/deps/extra-9f8e)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out
";
        assert_eq!(
            ran_tests(TestOutput::Libtest, out)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            ["issue_tests::refuses", "tests::slow"]
        );
        assert_eq!(ran_tests(TestOutput::Pytest, "1 passed in 0.1s"), None);
    }

    #[test]
    fn selects_one_libtest_or_pytest_test_and_nothing_unnamed() {
        let ids = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(
            selector(TestOutput::Libtest, &ids(&["cart::tests::refuses"])).as_deref(),
            Some("tests::refuses")
        );
        assert_eq!(
            selector(TestOutput::Pytest, &ids(&["tests/a.py::test_b[1]"])).as_deref(),
            Some("tests/a.py::test_b[1]")
        );
        assert_eq!(
            selector(TestOutput::Pytest, &ids(&["a.py::x", "a.py::y"])),
            None
        );
        assert_eq!(
            selector(TestOutput::Libtest, &ids(&["extra::<did not finish>"])),
            None
        );
        assert!(ran_nothing(
            TestOutput::Libtest,
            "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\n"
        ));
        assert!(!ran_nothing(
            TestOutput::Libtest,
            "test tests::refuses ... ok\ntest result: ok. 1 passed; 0 failed\n"
        ));
    }

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
