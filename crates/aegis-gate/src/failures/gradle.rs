//! Gradle, as the Kotlin image's init script reports it (#586): one
//! `aegis-gradle FAILED <Class>#<test>` line per failing test, the class
//! with its package (`$` for a nested one), whatever the repo's own
//! testLogging. `#` keeps a test name with dots in it (a Kotlin backtick
//! name) apart from its class. Gradle's own `Class > test FAILED` lines
//! name the class without its package and are not read.

use std::collections::BTreeSet;

use super::{DID_NOT_FINISH, plain};
use crate::gate::glob;

const MARK: &str = "aegis-gradle FAILED ";

/// What Gradle prints when a test JVM dies before it reports back.
const CRASHED: [&str; 2] = [
    "Test process encountered an unexpected problem",
    "Process 'Gradle Test Executor",
];

/// Names JUnit gives a class that failed before any of its tests ran
/// (`@BeforeAll`, a constructor): the class itself failed.
const WHOLE_CLASS: [&str; 3] = ["initializationError", "classMethod", "executionError"];

/// `<package>.<Class>#<test>`, with the parameters of a parameterized case
/// (`param(int)[2]`) and without the empty `()` of a plain one;
/// `<package>.<Class>` when the class itself failed. A test JVM that died
/// did not finish.
pub(super) fn failing(output: &str) -> BTreeSet<String> {
    let mut failed = BTreeSet::new();
    for line in output.lines().map(plain) {
        let line = line.trim();
        if CRASHED.iter().any(|c| line.contains(c)) {
            failed.insert(format!("gradle::{DID_NOT_FINISH}"));
            continue;
        }
        let Some(name) = line.strip_prefix(MARK).map(str::trim) else {
            continue;
        };
        let id = match name.split_once('#') {
            Some((class, test)) if WHOLE_CLASS.contains(&test) => class.to_owned(),
            Some((class, test)) => {
                format!("{class}#{}", test.strip_suffix("()").unwrap_or(test))
            }
            None => name.to_owned(),
        };
        if !id.is_empty() {
            failed.insert(id);
        }
    }
    failed
}

/// `(class, Some(test))`, or `(class, None)` for a class that failed as a
/// whole.
fn split(id: &str) -> (&str, Option<&str>) {
    match id.split_once('#') {
        Some((class, test)) => (class, Some(test)),
        None => (id, None),
    }
}

/// The class and test of a `--tests` pattern: the test after the last
/// `.` when it does not start with an upper-case letter.
fn split_pattern(pattern: &str) -> (&str, Option<&str>) {
    match pattern.rsplit_once('.') {
        Some((class, test)) if !test.starts_with(|c: char| c.is_ascii_uppercase()) => {
            (class, Some(test))
        }
        _ => (pattern, None),
    }
}

/// The test's function name, as `--tests` matches it: without its
/// parameters or case index.
fn method(test: &str) -> &str {
    test.find('(').map_or(test, |i| &test[..i])
}

/// The class without its package.
fn simple(class: &str) -> &str {
    class.rsplit('.').next().unwrap_or(class)
}

/// Whether `id` is a test `--tests <test>` selects: `*` anywhere, the
/// class with its package, or without it when the pattern starts with an
/// upper-case letter, as Gradle matches it.
pub(super) fn names(id: &str, test: &str) -> bool {
    if id.contains("::") {
        return false;
    }
    let test = test.trim();
    let by_simple = test.starts_with(|c: char| c.is_ascii_uppercase());
    let (class, name) = split(id);
    let classes = std::iter::once(class).chain(by_simple.then(|| simple(class)));
    let candidates: Vec<String> = match name {
        Some(name) => classes
            .flat_map(|c| [c.to_owned(), format!("{c}.{}", method(name))])
            .collect(),
        // A class that failed as a whole is in any pattern of one of its
        // tests.
        None => classes.map(str::to_owned).collect(),
    };
    let (pattern_class, _) = split_pattern(test);
    candidates
        .iter()
        .any(|c| glob(test, c) || (name.is_none() && glob(pattern_class, c)))
}

/// One `--tests` pattern for `ids`: the test when there is one, its class
/// when several fail in one class or the class failed as a whole. `None`
/// across classes: `--tests` takes one pattern, so the gate reruns the
/// suite.
pub(super) fn selector(ids: &BTreeSet<String>) -> Option<String> {
    let parts: Vec<(&str, Option<&str>)> = ids.iter().map(|id| split(id)).collect();
    let (class, _) = *parts.first()?;
    if parts.iter().any(|(c, _)| *c != class) {
        return None;
    }
    let methods: BTreeSet<Option<&str>> = parts.iter().map(|(_, t)| t.map(method)).collect();
    Some(match methods.into_iter().collect::<Vec<_>>().as_slice() {
        [Some(m)] => format!("{class}.{m}"),
        _ => class.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = include_str!("../../tests/fixtures/test-output/gradle.log");

    #[test]
    fn reads_every_failing_test_and_class() {
        assert_eq!(
            failing(LOG).into_iter().collect::<Vec<_>>(),
            [
                "example.cart.BrokenTest#failsAssert",
                "example.cart.BrokenTest#has spaces in its name",
                "example.cart.BrokenTest#param(int)[2]",
                "example.cart.BrokenTest#throwsError",
                "example.cart.BrokenTest$Inner#innerFails",
                "example.cart.SetupTest",
                "example.other.CartTest#addsUpPrices",
            ]
        );
    }

    #[test]
    fn a_dead_test_jvm_did_not_finish() {
        let out = "\
* What went wrong:
Execution failed for task ':test'.
> Test process encountered an unexpected problem.
   > Process 'Gradle Test Executor 1' finished with non-zero exit value 3
";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["gradle::<did not finish>"]
        );
    }

    #[test]
    fn a_test_name_with_dots_stays_apart_from_its_class() {
        let out = "aegis-gradle FAILED ex.CartTest#adds 1.5 to Total()\n";
        let ids = failing(out);
        assert_eq!(
            ids.iter().collect::<Vec<_>>(),
            ["ex.CartTest#adds 1.5 to Total"]
        );
        assert!(names(
            "ex.CartTest#adds 1.5 to Total",
            "CartTest.adds 1.5 to Total"
        ));
        assert_eq!(
            selector(&ids).as_deref(),
            Some("ex.CartTest.adds 1.5 to Total")
        );
    }

    #[test]
    fn gradles_own_lines_are_not_read() {
        assert!(failing("CartTest > addsUpPrices() FAILED\n").is_empty());
    }

    #[test]
    fn selects_one_test_or_its_class() {
        let ids = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(
            selector(&ids(&["example.cart.BrokenTest#param(int)[2]"])).as_deref(),
            Some("example.cart.BrokenTest.param")
        );
        assert_eq!(
            selector(&ids(&[
                "example.cart.BrokenTest#param(int)[1]",
                "example.cart.BrokenTest#param(int)[2]",
            ]))
            .as_deref(),
            Some("example.cart.BrokenTest.param")
        );
        assert_eq!(
            selector(&ids(&[
                "example.cart.BrokenTest#failsAssert",
                "example.cart.BrokenTest#throwsError",
            ]))
            .as_deref(),
            Some("example.cart.BrokenTest")
        );
        assert_eq!(
            selector(&ids(&["example.cart.SetupTest"])).as_deref(),
            Some("example.cart.SetupTest")
        );
        assert_eq!(
            selector(&ids(&[
                "example.cart.BrokenTest$Inner#innerFails",
                "example.cart.BrokenTest#failsAssert",
            ])),
            None
        );
        assert_eq!(selector(&BTreeSet::new()), None);
    }

    #[test]
    fn matches_tests_patterns() {
        let id = "example.cart.BrokenTest#param(int)[2]";
        assert!(names(id, "BrokenTest.param"));
        assert!(names(id, "example.cart.BrokenTest.param"));
        assert!(names(id, "BrokenTest"));
        assert!(names(id, "example.cart.*"));
        assert!(names(id, "Broken*.par*"));
        assert!(names(id, "*.param"));
        assert!(!names(id, "BrokenTest.failsAssert"));
        assert!(!names(id, "Broken"));
        assert!(!names(id, "other.BrokenTest.param"));
        assert!(!names(id, "brokenTest.param"));
        assert!(names(
            "example.cart.BrokenTest$Inner#innerFails",
            "example.cart.BrokenTest$Inner.innerFails"
        ));
        assert!(names(
            "example.cart.BrokenTest#has spaces in its name",
            "BrokenTest.has spaces in its name"
        ));
        assert!(names("example.cart.SetupTest", "SetupTest.needsServer"));
        assert!(names("example.cart.SetupTest", "SetupTest"));
        assert!(!names("example.cart.SetupTest", "OtherTest.needsServer"));
        assert!(!names("gradle::<did not finish>", "BrokenTest"));
    }
}
