//! Spectrum-Based Fault Localization (SBFL) algorithms: Tarantula and Ochiai (Issue #34; design §9).
//!
//! Ranks functions or code elements by suspiciousness based on test pass/fail
//! execution spectra.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Supported SBFL formulas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SbflFormula {
    Tarantula,
    #[default]
    Ochiai,
}

impl fmt::Display for SbflFormula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tarantula => write!(f, "Tarantula"),
            Self::Ochiai => write!(f, "Ochiai"),
        }
    }
}

/// A single test case execution record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestExecution {
    pub name: String,
    pub passed: bool,
    pub covered_elements: BTreeSet<String>,
}

/// A ranked code element with suspiciousness metrics.
#[derive(Debug, Clone, PartialEq)]
pub struct SuspiciousElement {
    pub element: String,
    pub score: f64,
    pub failed_covered: usize, // e_f
    pub passed_covered: usize, // e_p
    pub total_failed: usize,   // N_fail
    pub total_passed: usize,   // N_pass
}

impl fmt::Display for SuspiciousElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:.4}\t{}\t(failed: {}/{}, passed: {}/{})",
            self.score,
            self.element,
            self.failed_covered,
            self.total_failed,
            self.passed_covered,
            self.total_passed
        )
    }
}

/// Execution spectrum matrix for calculating fault suspiciousness.
#[derive(Debug, Default, Clone)]
pub struct SbflMatrix {
    tests: Vec<TestExecution>,
}

impl SbflMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a test case run with its covered functions/elements.
    pub fn record_test(
        &mut self,
        name: impl Into<String>,
        passed: bool,
        covered: impl IntoIterator<Item = impl Into<String>>,
    ) {
        self.tests.push(TestExecution {
            name: name.into(),
            passed,
            covered_elements: covered.into_iter().map(Into::into).collect(),
        });
    }

    /// Total number of failed tests recorded.
    pub fn total_failed(&self) -> usize {
        self.tests.iter().filter(|t| !t.passed).count()
    }

    /// Total number of passed tests recorded.
    pub fn total_passed(&self) -> usize {
        self.tests.iter().filter(|t| t.passed).count()
    }

    /// Calculate suspiciousness score using the chosen formula.
    ///
    /// Tarantula: (e_f / N_f) / ((e_f / N_f) + (e_p / N_p))
    /// Ochiai: e_f / sqrt(N_f * (e_f + e_p))
    pub fn compute_score(
        formula: SbflFormula,
        failed_covered: usize,
        passed_covered: usize,
        total_failed: usize,
        total_passed: usize,
    ) -> f64 {
        if total_failed == 0 || failed_covered == 0 {
            return 0.0;
        }

        let ef = failed_covered as f64;
        let ep = passed_covered as f64;
        let nf = total_failed as f64;
        let np = total_passed as f64;

        match formula {
            SbflFormula::Tarantula => {
                let fail_ratio = ef / nf;
                let pass_ratio = if total_passed == 0 { 0.0 } else { ep / np };
                let denom = fail_ratio + pass_ratio;
                if denom <= 0.0 {
                    0.0
                } else {
                    fail_ratio / denom
                }
            }
            SbflFormula::Ochiai => {
                let denom = (nf * (ef + ep)).sqrt();
                if denom <= 0.0 { 0.0 } else { ef / denom }
            }
        }
    }

    /// Rank all executed elements and return top `limit` entries.
    pub fn rank(&self, formula: SbflFormula, limit: usize) -> Vec<SuspiciousElement> {
        let total_failed = self.total_failed();
        let total_passed = self.total_passed();

        if total_failed == 0 {
            return Vec::new();
        }

        // Count (failed_covered, passed_covered) for each element
        let mut element_stats: BTreeMap<String, (usize, usize)> = BTreeMap::new();

        for test in &self.tests {
            for element in &test.covered_elements {
                let entry = element_stats.entry(element.clone()).or_insert((0, 0));
                if test.passed {
                    entry.1 += 1;
                } else {
                    entry.0 += 1;
                }
            }
        }

        let mut ranked: Vec<SuspiciousElement> = element_stats
            .into_iter()
            .map(|(element, (ef, ep))| {
                let score = Self::compute_score(formula, ef, ep, total_failed, total_passed);
                SuspiciousElement {
                    element,
                    score,
                    failed_covered: ef,
                    passed_covered: ep,
                    total_failed,
                    total_passed,
                }
            })
            .filter(|e| e.score > 0.0)
            .collect();

        // Sort descending by score, tie-break by name
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.element.cmp(&b.element))
        });

        if limit > 0 && ranked.len() > limit {
            ranked.truncate(limit);
        }

        ranked
    }

    /// Format top suspicious elements into a readable report.
    pub fn format_report(&self, formula: SbflFormula, limit: usize) -> String {
        let ranked = self.rank(formula, limit);
        if ranked.is_empty() {
            return "No suspicious elements identified (no failing tests or no coverage)."
                .to_owned();
        }

        let mut out = format!(
            "SBFL suspiciousness ranking ({formula}, total failed: {}, total passed: {}):\n",
            self.total_failed(),
            self.total_passed()
        );
        for (i, elem) in ranked.iter().enumerate() {
            out.push_str(&format!("{:2}. {}\n", i + 1, elem));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tarantula_and_ochiai_scores_match_ground_truth() {
        let mut matrix = SbflMatrix::new();

        // Test 1: FAIL, executes F1, F2
        matrix.record_test("test_error", false, vec!["func1", "func2"]);
        // Test 2: PASS, executes F2, F3
        matrix.record_test("test_ok1", true, vec!["func2", "func3"]);
        // Test 3: PASS, executes F3
        matrix.record_test("test_ok2", true, vec!["func3"]);

        // Total failed = 1, total passed = 2
        assert_eq!(matrix.total_failed(), 1);
        assert_eq!(matrix.total_passed(), 2);

        // Ochiai ranking
        let ochiai = matrix.rank(SbflFormula::Ochiai, 10);
        assert_eq!(ochiai.len(), 2); // func3 has ef=0, so score=0
        assert_eq!(ochiai[0].element, "func1");
        assert!((ochiai[0].score - 1.0).abs() < 1e-4); // 1 / sqrt(1 * 1) = 1.0

        assert_eq!(ochiai[1].element, "func2");
        // 1 / sqrt(1 * 2) = 1 / sqrt(2) ≈ 0.7071
        assert!((ochiai[1].score - (1.0 / 2.0_f64.sqrt())).abs() < 1e-4);

        // Tarantula ranking
        let tarantula = matrix.rank(SbflFormula::Tarantula, 10);
        assert_eq!(tarantula[0].element, "func1");
        assert!((tarantula[0].score - 1.0).abs() < 1e-4); // (1/1) / ((1/1) + (0/2)) = 1.0

        assert_eq!(tarantula[1].element, "func2");
        // (1/1) / ((1/1) + (1/2)) = 1.0 / 1.5 = 2/3 ≈ 0.6667
        assert!((tarantula[1].score - (2.0 / 3.0)).abs() < 1e-4);
    }

    #[test]
    fn empty_matrix_or_no_failing_tests_returns_empty() {
        let mut matrix = SbflMatrix::new();
        matrix.record_test("test_pass", true, vec!["func1"]);

        assert!(matrix.rank(SbflFormula::Ochiai, 10).is_empty());
        assert!(
            matrix
                .format_report(SbflFormula::Ochiai, 10)
                .contains("No suspicious elements")
        );
    }

    #[test]
    fn limit_truncates_top_results() {
        let mut matrix = SbflMatrix::new();
        matrix.record_test("test_fail", false, vec!["func1", "func2", "func3"]);

        let ranked = matrix.rank(SbflFormula::Ochiai, 2);
        assert_eq!(ranked.len(), 2);
    }
}
