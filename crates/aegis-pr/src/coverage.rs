//! Line coverage in percent from the report the gate's coverage step
//! wrote, for the PR's gate line. Formats the profiles use in Phase 1.

/// `LH` (lines hit) over `LF` (lines found), summed over all records.
pub fn lcov_percent(text: &str) -> Option<f64> {
    let (mut found, mut hit) = (0u64, 0u64);
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("LF:") {
            found += n.trim().parse::<u64>().ok()?;
        } else if let Some(n) = line.strip_prefix("LH:") {
            hit += n.trim().parse::<u64>().ok()?;
        }
    }
    (found > 0 && hit <= found).then(|| hit as f64 * 100.0 / found as f64)
}

/// The `line-rate` of the root `<coverage>` element.
pub fn cobertura_percent(text: &str) -> Option<f64> {
    let start = text.find("<coverage ")?;
    let tag = &text[start..start + text[start..].find('>')?];
    let rate = tag.split("line-rate=").nth(1)?;
    let quote = rate.chars().next().filter(|q| *q == '"' || *q == '\'')?;
    let value = rate[1..].split(quote).next()?;
    let rate: f64 = value.parse().ok()?;
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
}
