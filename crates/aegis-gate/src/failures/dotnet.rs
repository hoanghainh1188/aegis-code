//! `dotnet test`: VSTest prints `  Failed <name> [<time>]` for each failing
//! test, Microsoft.Testing.Platform `failed <name> (<time>)`.

use std::collections::BTreeSet;

use super::{DID_NOT_FINISH, plain};

/// What the runners print when the test host dies mid-run.
const CRASHED: [&str; 3] = [
    "The active test run was aborted",
    "Test Run Aborted",
    "Test host process crashed",
];

/// The test's name as the runner shows it: VSTest gives the fully
/// qualified name for xUnit (`Ns.Class.Method(x: 2)`), MTP and NUnit the
/// display name. A test host that crashed did not finish.
pub(super) fn failing(output: &str) -> BTreeSet<String> {
    let mut failed = BTreeSet::new();
    for line in output.lines().map(plain) {
        if CRASHED.iter().any(|c| line.contains(c)) {
            failed.insert(format!("dotnet::{DID_NOT_FINISH}"));
        }
        // Only a name followed by its duration: an error message may start
        // with "Failed" too.
        let vstest = line
            .trim_start()
            .strip_prefix("Failed ")
            .and_then(|rest| timed(rest, " [", ']'));
        let mtp = line
            .strip_prefix("failed ")
            .and_then(|rest| timed(rest, " (", ')'));
        if let Some(name) = vstest.or(mtp) {
            failed.insert(name.trim().to_owned());
        }
    }
    failed
}

/// `rest` without its trailing `<open><duration><close>`, when it has one:
/// `[12 ms]`, `[< 1 ms]`, `(8ms)`, `(1s 20ms)`.
fn timed<'a>(rest: &'a str, open: &str, close: char) -> Option<&'a str> {
    let (name, time) = rest.strip_suffix(close)?.rsplit_once(open)?;
    let duration = time.ends_with('s')
        && time.starts_with(|c: char| c.is_ascii_digit() || c == '<')
        && time
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " .<".contains(c));
    duration.then_some(name)
}

/// Whether `id` is a test `dotnet test --filter <test>` selects. Each
/// clause of the filter, `<property><op><value>` or a bare value, is
/// checked against the name; a filter on traits alone cannot be, and
/// takes every test.
pub(super) fn names(id: &str, test: &str) -> bool {
    if id.contains("::") {
        return false;
    }
    let mut checked = false;
    let named = test
        .split(['|', '&'])
        .map(|clause| clause.trim().trim_matches(['(', ')']).trim())
        .filter(|clause| !clause.is_empty() && !clause.contains('!'))
        .any(|clause| {
            let (property, op, value) = match clause.find(['=', '~']) {
                Some(at) => (&clause[..at], &clause[at..=at], &clause[at + 1..]),
                None => ("FullyQualifiedName", "~", clause),
            };
            let value = value.replace('\\', "");
            match property.trim() {
                "FullyQualifiedName" | "Name" | "DisplayName" | "ClassName" => {
                    checked = true;
                    is_named(id, &value, op == "~")
                }
                _ => false,
            }
        });
    named || !checked
}

/// Whether the name `id` (fully qualified, or a display name alone) is the
/// filter's `value`, or contains it when the operator is `~`.
fn is_named(id: &str, value: &str, contains: bool) -> bool {
    let bare = |s: &str| s.split_once('(').map_or(s, |(n, _)| n).trim().to_owned();
    let name = bare(id);
    let dotted = |a: &str, b: &str| a == b || a.ends_with(&format!(".{b}"));
    (contains && id.contains(value))
        || dotted(&name, value)
        || name.starts_with(&format!("{value}."))
        // A display name without its class, as MTP shows it.
        || (!name.contains('.') && dotted(value, &name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_vstest() {
        let log = include_str!("../../tests/fixtures/test-output/vstest.log");
        assert_eq!(
            failing(log).into_iter().collect::<Vec<_>>(),
            [
                "Shop.Tests.CartTests.FailsAssert",
                "Shop.Tests.CartTests.Param(x: 2)",
                "Shop.Tests.CartTests.ThrowsError",
                "Shop.Tests.NeedsServerTests.Connects",
            ]
        );
    }

    #[test]
    fn reads_the_testing_platform() {
        let log = include_str!("../../tests/fixtures/test-output/mtp.log");
        assert_eq!(
            failing(log).into_iter().collect::<Vec<_>>(),
            ["FailsAssert", "Param (2)", "ThrowsError"]
        );
    }

    #[test]
    fn message_lines_are_not_tests() {
        let out = "\
  Failed Shop.Tests.RedisTests.Connects [12 ms]
  Error Message:
   Failed to connect to localhost:6379
  Failed to connect again
";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["Shop.Tests.RedisTests.Connects"]
        );
    }

    #[test]
    fn a_crashed_test_host_did_not_finish() {
        let out = "The active test run was aborted. Reason: Test host process crashed\n";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["dotnet::<did not finish>"]
        );
    }

    #[test]
    fn matches_filter_expressions() {
        let id = "Shop.Tests.CartTests.Param(x: 2)";
        assert!(names(id, "FullyQualifiedName~Shop.Tests.CartTests.Param"));
        assert!(names(id, "FullyQualifiedName=Shop.Tests.CartTests.Param"));
        assert!(names(id, "Name=Param"));
        assert!(names(id, "ClassName=Shop.Tests.CartTests"));
        assert!(names(id, "CartTests.Param"));
        assert!(names(id, "Name=Other|Name=Param"));
        assert!(names(id, "Category=Slow"));
        assert!(!names(id, "Name=Paramx"));
        assert!(!names(id, "FullyQualifiedName~Shop.Tests.OtherTests"));
        // MTP shows the method's display name only.
        assert!(names(
            "Param (2)",
            "FullyQualifiedName~Shop.Tests.CartTests.Param"
        ));
        assert!(!names("dotnet::<did not finish>", "Name=Param"));
    }
}
