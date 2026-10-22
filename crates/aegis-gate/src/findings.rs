//! "No new warnings" for format, lint and typecheck (design §6.1): a real
//! repo often fails its own linter already, at its base, with the image's
//! tool version. The patch is judged only on what it adds. Findings are the
//! output lines that name a file the patch changed, with numbers blanked so
//! a shifted line still matches; a finding the base does not have as often
//! is new. This works for any tool that prints `path:line:col` or the path
//! alone, without parsing its format.

use std::collections::BTreeMap;

fn normalized(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_number = false;
    for c in line.trim().chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            out.push(c);
            in_number = false;
        }
    }
    out
}

fn findings(log: &str, changed: &[String]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for line in log.lines() {
        if changed.iter().any(|path| mentions(line, path)) {
            *counts.entry(normalized(line)).or_default() += 1;
        }
    }
    counts
}

/// `path` as a whole path in `line`, not as the tail of a longer one
/// (`a.rs` in `src/data.rs`).
fn mentions(line: &str, path: &str) -> bool {
    line.match_indices(path).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + path.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
            && !after.is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '_' | '-'))
    })
}

/// Lines of `patched` naming a changed file, beyond what `base` has.
pub(crate) fn new_findings(patched: &str, base: &str, changed: &[String]) -> Vec<String> {
    let before = findings(base, changed);
    let mut new = Vec::new();
    for (finding, n) in findings(patched, changed) {
        let had = before.get(&finding).copied().unwrap_or(0);
        if n > had {
            new.push(finding);
        }
    }
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "\
ISC004 Unparenthesized implicit string concatenation in collection
   --> tests/test_utils_curl.py:130:21
F401 `os` imported but unused
 --> scrapy/utils/url.py:3:8
Found 2 errors.
";

    fn changed() -> Vec<String> {
        vec!["scrapy/utils/url.py".into()]
    }

    #[test]
    fn findings_already_on_the_base_are_not_new_even_when_moved() {
        let patched = BASE.replace("url.py:3:8", "url.py:5:8");
        assert!(new_findings(&patched, BASE, &changed()).is_empty());
    }

    #[test]
    fn a_finding_in_a_changed_file_is_new() {
        let patched = format!("{BASE} --> scrapy/utils/url.py:40:1\n");
        assert_eq!(
            new_findings(&patched, BASE, &changed()),
            ["--> scrapy/utils/url.py:#:#"]
        );
    }

    #[test]
    fn files_the_patch_did_not_touch_do_not_count() {
        let patched = format!("{BASE} --> tests/test_other.py:1:1\n");
        assert!(new_findings(&patched, BASE, &changed()).is_empty());
    }

    #[test]
    fn a_path_is_matched_whole() {
        assert!(mentions("--> a.rs:1:1", "a.rs"));
        assert!(mentions("Would reformat: a.rs", "a.rs"));
        assert!(!mentions("--> src/data.rs:1:1", "a.rs"));
        assert!(!mentions("--> a.rsx:1:1", "a.rs"));
    }
}
