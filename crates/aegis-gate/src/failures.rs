//! Failing test names from a test run's output, per the profile's
//! [`TestOutput`], so the gate can tell new failures from ones the base
//! branch already has.

use std::collections::BTreeSet;

use aegis_lang::TestOutput;

pub fn failing_tests(format: TestOutput, output: &str) -> BTreeSet<String> {
    match format {
        TestOutput::Libtest => libtest(output),
        TestOutput::Pytest => pytest(output),
    }
}

/// Stands for the tests of a run that stopped before reporting (a crash,
/// an abort, `exit()` in a test): they count as failing.
pub const DID_NOT_FINISH: &str = "<did not finish>";

/// `test name ... FAILED`, named after the test binary that ran it
/// (`Running … (target/debug/deps/<binary>-<hash>)`, `Doc-tests <crate>`)
/// so tests with the same path in two crates stay apart. A binary that
/// started but printed no `test result:` line did not finish.
fn libtest(output: &str) -> BTreeSet<String> {
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
        if line.starts_with("test result: ") {
            open = false;
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
/// `===` around it.
fn pytest_finished(output: &str) -> bool {
    output
        .lines()
        .rev()
        .map(|l| l.trim().trim_matches('=').trim())
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
/// optional ` - <message>`.
fn pytest(output: &str) -> BTreeSet<String> {
    let mut failed: BTreeSet<String> = output
        .lines()
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
            libtest(out).into_iter().collect::<Vec<_>>(),
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
            libtest(crashed).into_iter().collect::<Vec<_>>(),
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
