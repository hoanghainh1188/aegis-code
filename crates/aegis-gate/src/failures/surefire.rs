//! Maven Surefire: one line per failing test, `<Class>.<method> -- Time
//! elapsed: … <<< FAILURE!` (`<<< ERROR!` for an exception), whatever the
//! JUnit or TestNG provider.

use std::collections::BTreeSet;

use super::{DID_NOT_FINISH, plain};
use crate::gate::glob;

/// What Surefire prints when its forked JVM does not report back.
const CRASHED: [&str; 2] = [
    "The forked VM terminated without properly saying goodbye",
    "There was a timeout in the fork",
];

/// `<package>.<Class>.<method>`, with the parameters of a parameterized
/// case (`param(int)[2]`); `<package>.<Class>` when the class itself
/// failed (`@BeforeAll`, `@BeforeClass`). A forked JVM that crashed or
/// timed out did not finish.
pub(super) fn failing(output: &str) -> BTreeSet<String> {
    let mut failed = BTreeSet::new();
    for line in output.lines().map(plain) {
        let line = line.trim();
        let line = line.strip_prefix("[ERROR] ").unwrap_or(line);
        if CRASHED.iter().any(|c| line.contains(c)) {
            failed.insert(format!("surefire::{DID_NOT_FINISH}"));
            continue;
        }
        if line.starts_with("Tests run:")
            || !(line.ends_with("<<< FAILURE!") || line.ends_with("<<< ERROR!"))
        {
            continue;
        }
        let Some((name, _)) = line.split_once("Time elapsed") else {
            continue;
        };
        let name = name.trim().trim_end_matches("--").trim();
        failed.insert(older_name(name).unwrap_or_else(|| name.to_owned()));
    }
    failed
}

/// Surefire 2's `method(package.Class)` as `package.Class.method`.
fn older_name(name: &str) -> Option<String> {
    let (method, class) = name.strip_suffix(')')?.split_once('(')?;
    (!method.contains('.') && class.contains('.')).then(|| format!("{class}.{method}"))
}

/// Whether `id` is a test `-Dtest=<test>` selects: comma-separated
/// `Class#method` patterns, the class simple or with its package, methods
/// joined by `+`, `*` anywhere.
pub(super) fn names(id: &str, test: &str) -> bool {
    if id.contains("::") {
        return false;
    }
    let id = id.split_once('(').map_or(id, |(name, _)| name);
    test.split(',').map(str::trim).any(|pattern| {
        let (class, methods) = pattern.split_once('#').unwrap_or((pattern, ""));
        let class = class.trim_end_matches(".java").replace('/', ".");
        let class = class.trim_start_matches("**.");
        // The id is the class itself (it failed as a whole) or the class
        // and a method.
        let whole = is_class(id, class);
        let method = id.rsplit_once('.').is_some_and(|(owner, method)| {
            is_class(owner, class)
                && (methods.is_empty() || methods.split('+').any(|m| glob(m, method)))
        });
        whole || method
    })
}

/// `name`, a class with its package, matched by the pattern with or
/// without the package.
fn is_class(name: &str, pattern: &str) -> bool {
    let simple = name.rsplit('.').next().unwrap_or(name);
    glob(pattern, name) || (!pattern.contains('.') && glob(pattern, simple))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = include_str!("../../tests/fixtures/test-output/surefire.log");

    #[test]
    fn reads_every_failing_test_and_class() {
        assert_eq!(
            failing(LOG).into_iter().collect::<Vec<_>>(),
            [
                "ex.CartTest.failsAssert",
                "ex.CartTest.innerFails",
                "ex.CartTest.param(int)[2]",
                "ex.CartTest.throwsError",
                "ex.OldSetupTest",
                "ex.OldTest.failsAssert",
                "ex.OldTest.throwsError",
                "ex.SetupTest",
            ]
        );
    }

    #[test]
    fn reads_older_surefire_lines() {
        let out = "\
Tests run: 2, Failures: 1, Errors: 0, Skipped: 0, Time elapsed: 0.05 sec <<< FAILURE! - in ex.OldTest
failsAssert(ex.OldTest)  Time elapsed: 0.01 sec  <<< FAILURE!
[ERROR] ex.Mid.boom  Time elapsed: 0.01 s  <<< ERROR!
";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["ex.Mid.boom", "ex.OldTest.failsAssert"]
        );
    }

    #[test]
    fn a_crashed_fork_did_not_finish() {
        let out = "[ERROR] ExecutionException The forked VM terminated without properly saying goodbye. VM crash or System.exit called?\n";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["surefire::<did not finish>"]
        );
    }

    #[test]
    fn matches_dtest_patterns() {
        let id = "ex.CartTest.param(int)[2]";
        assert!(names(id, "CartTest#param"));
        assert!(names(id, "ex.CartTest#param"));
        assert!(names(id, "CartTest"));
        assert!(names(id, "Cart*#par*"));
        assert!(names(id, "OtherTest,CartTest#failsAssert+param"));
        assert!(names("ex.SetupTest", "SetupTest#needsServer"));
        assert!(!names(id, "CartTest#failsAssert"));
        assert!(!names(id, "Cart"));
        assert!(!names(id, "other.CartTest#param"));
        assert!(!names("surefire::<did not finish>", "CartTest"));
    }
}
