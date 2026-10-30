//! Jest and Vitest, with their default reporters. Vitest names each
//! failure on one line, ` FAIL  <file> > <describe> > <name>`; Jest prints
//! `FAIL <file>`, then `● <describe> › <name>` for each failing test in it.

use std::collections::BTreeSet;

use super::plain;

/// `<file> > <describe> > <name>` for both runners; `<file>` alone when
/// the file failed to load. Errors outside any test (Vitest's "Unhandled
/// Errors") count as `<unhandled errors>`.
pub(super) fn failing(output: &str) -> BTreeSet<String> {
    let mut failed = BTreeSet::new();
    // The file of Jest's last `FAIL` line, until a `PASS` line.
    let mut file: Option<String> = None;
    for line in output.lines().map(plain) {
        if let Some(rest) = line.strip_prefix(" FAIL ") {
            let rest = rest.trim();
            let id = rest.split_once(" [ ").map_or(rest, |(file, _)| file.trim());
            failed.insert(id.to_owned());
        } else if let Some(rest) = line.strip_prefix("FAIL ") {
            // A slow file: `FAIL src/x.test.js (6.12 s)`.
            let rest = rest.trim();
            let rest = match rest.rsplit_once(" (") {
                Some((path, time)) if time.ends_with(" s)") => path,
                _ => rest,
            };
            file = Some(rest.to_owned());
        } else if line.starts_with("PASS ") {
            file = None;
        } else if let (Some(file), Some(name)) = (&file, line.strip_prefix("  ● ")) {
            match name {
                "Test suite failed to run" => {
                    failed.insert(file.clone());
                }
                n if n.starts_with("Console") || n.starts_with(char::is_whitespace) => {}
                n => {
                    failed.insert(format!("{file} > {}", n.trim().replace(" › ", " > ")));
                }
            }
        } else if line.contains(" Unhandled Errors ") {
            failed.insert("<unhandled errors>".to_owned());
        }
    }
    failed
}

/// Whether `id` is a test `npm test -- <test>` selects: both runners take a
/// file filter, so a file path containing `test`; or the test's own name.
pub(super) fn names(id: &str, test: &str) -> bool {
    if id.starts_with('<') {
        return false;
    }
    let (file, name) = id.split_once(" > ").unwrap_or((id, ""));
    file.contains(test) || name == test || name.ends_with(&format!(" > {test}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED: [&str; 4] = [
        "Cart > adds up prices",
        "Cart > nested group > case 2",
        "",
        "top level fails",
    ];

    fn expected(file: &str, broken: &str) -> Vec<String> {
        let mut ids: Vec<String> = EXPECTED
            .iter()
            .map(|n| match n {
                &"" => broken.to_owned(),
                n => format!("{file} > {n}"),
            })
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn reads_vitest() {
        let log = include_str!("../../tests/fixtures/test-output/vitest.log");
        assert_eq!(
            failing(log).into_iter().collect::<Vec<_>>(),
            expected("src/cart.test.ts", "src/broken.test.ts")
        );
    }

    #[test]
    fn reads_jest() {
        let log = include_str!("../../tests/fixtures/test-output/jest.log");
        assert_eq!(
            failing(log).into_iter().collect::<Vec<_>>(),
            expected("src/cart.test.js", "src/broken.test.js")
        );
    }

    #[test]
    fn skips_jest_console_blocks_and_reads_slow_files() {
        let out = "\
FAIL src/slow.test.js (6.12 s)
  ● Console

    console.log
      hello

  ● slow › times out

PASS src/fine.test.js
  ● not a failure
";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["src/slow.test.js > slow > times out"]
        );
    }

    #[test]
    fn unhandled_vitest_errors_count() {
        let out = " Test Files  1 passed (1)\n⎯⎯⎯⎯ Unhandled Errors ⎯⎯⎯⎯\nError: late\n";
        assert_eq!(
            failing(out).into_iter().collect::<Vec<_>>(),
            ["<unhandled errors>"]
        );
    }

    #[test]
    fn matches_a_file_or_a_name() {
        let id = "src/cart.test.ts > Cart > adds up prices";
        assert!(names(id, "src/cart.test.ts"));
        assert!(names(id, "cart.test"));
        assert!(names(id, "Cart > adds up prices"));
        assert!(names(id, "adds up prices"));
        assert!(names("src/broken.test.ts", "broken"));
        assert!(!names(id, "src/other.test.ts"));
        assert!(!names(id, "adds up"));
        assert!(!names("<unhandled errors>", "cart"));
    }
}
