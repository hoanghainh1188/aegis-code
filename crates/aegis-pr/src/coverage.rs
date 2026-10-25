//! Line coverage in percent from the report the gate's coverage step
//! wrote, for the PR's gate line. Formats the profiles use in Phase 1.

/// `LH` (lines hit) over `LF` (lines found), summed over all records.
pub fn lcov_percent(text: &str) -> Option<f64> {
    lcov_lines(text).map(|(hit, found)| hit as f64 * 100.0 / found as f64)
}

/// Lines hit and lines found, summed over the files of an LCOV report.
pub fn lcov_lines(text: &str) -> Option<(u64, u64)> {
    let (mut found, mut hit) = (0u64, 0u64);
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("LF:") {
            found += n.trim().parse::<u64>().ok()?;
        } else if let Some(n) = line.strip_prefix("LH:") {
            hit += n.trim().parse::<u64>().ok()?;
        }
    }
    (found > 0 && hit <= found).then_some((hit, found))
}

/// `lines-covered` and `lines-valid` of the root `<coverage>` element,
/// when the report gives them.
pub fn cobertura_lines(text: &str) -> Option<(u64, u64)> {
    let tag = coverage_tag(text)?;
    let covered: u64 = attribute(tag, "lines-covered")?.parse().ok()?;
    let valid: u64 = attribute(tag, "lines-valid")?.parse().ok()?;
    (valid > 0 && covered <= valid).then_some((covered, valid))
}

/// The root `<coverage …` tag, up to its `>`.
fn coverage_tag(text: &str) -> Option<&str> {
    let start = text.find("<coverage ")?;
    Some(&text[start..start + text[start..].find('>')?])
}

/// The value of `name="…"` (or `'…'`) in `tag`.
fn attribute<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let rest = tag.split(&format!(" {name}=")).nth(1)?;
    let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
    rest[1..].split(quote).next()
}

/// The `line-rate` of the root `<coverage>` element.
pub fn cobertura_percent(text: &str) -> Option<f64> {
    let rate: f64 = attribute(coverage_tag(text)?, "line-rate")?.parse().ok()?;
    (0.0..=1.0).contains(&rate).then_some(rate * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lcov_and_cobertura() {
        let lcov = "TN:\nSF:src/a.rs\nLF:10\nLH:8\nend_of_record\nSF:src/b.rs\nLF:10\nLH:2\nend_of_record\n";
        assert_eq!(lcov_percent(lcov), Some(50.0));
        assert_eq!(lcov_percent("TN:\n"), None);
        assert_eq!(lcov_percent("LF:1\nLH:2\n"), None);
        assert_eq!(cobertura_percent("<coverages line-rate=\"0.5\">"), None);
        let xml = r#"<?xml version="1.0" ?><coverage version="7.6" line-rate="0.8512" branch-rate="0"><packages/></coverage>"#;
        assert!((cobertura_percent(xml).unwrap() - 85.12).abs() < 1e-9);
        assert_eq!(cobertura_percent("<coverage line-rate=\"7\">"), None);
        assert_eq!(cobertura_percent("<nothing/>"), None);
    }

    #[test]
    fn counts_the_lines_of_lcov_and_cobertura() {
        let lcov = "SF:src/a.rs\nLF:10\nLH:8\nend_of_record\nSF:src/b.rs\nLF:10\nLH:2\n";
        assert_eq!(lcov_lines(lcov), Some((10, 20)));
        assert_eq!(lcov_lines("TN:\n"), None);
        let xml = r#"<coverage line-rate="0.75" lines-covered="3" lines-valid="4">"#;
        assert_eq!(cobertura_lines(xml), Some((3, 4)));
        // Without the counts, only the rate is known.
        assert_eq!(cobertura_lines(r#"<coverage line-rate="0.75">"#), None);
        assert_eq!(
            cobertura_lines(r#"<coverage lines-covered="5" lines-valid="4">"#),
            None
        );
    }
}
