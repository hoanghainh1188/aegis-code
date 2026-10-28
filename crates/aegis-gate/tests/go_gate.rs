//! #235 for Go: `go test -run` exits 0 when the id selects no test, so the
//! gate tells that apart from a test that passes (Go needed).

mod common;

use aegis_gate::{Repro, StepName, StepStatus};
use aegis_lang::Language;
use common::{Repo, profile, tools_available};

const BUG: (&str, &str, &str) = (
    "cart.go",
    "len(c.items) >= MaxItems",
    "len(c.items) > MaxItems",
);

const REPRO_TEST: &str = "package cart

import (
	\"errors\"
	\"testing\"
)

func TestFourthItemIsRefused(t *testing.T) {
	if _, err := Of(1, 2, 3, 4); !errors.Is(err, ErrFull) {
		t.Fatalf(\"got %v, want ErrFull\", err)
	}
}
";

fn fixed() -> Option<Repo> {
    if !tools_available(&[&["go", "version"]]) {
        return None;
    }
    let repo = Repo::new("go", &[BUG]);
    repo.replace(BUG.0, BUG.2, BUG.1);
    repo.write("issue_regression_test.go", REPRO_TEST);
    Some(repo)
}

fn repro(test: &str) -> Repro {
    Repro {
        test: test.into(),
        files: vec!["issue_regression_test.go".into()],
    }
}

#[test]
fn an_id_that_selects_no_test_is_refused_as_such() {
    let Some(repo) = fixed() else { return };
    let id = "issue_regression_test.go::TestFourthItemIsRefused";
    let report = repo.gate(&profile(Language::Go, ""), Some(&repro(id)));
    let step = report.step(StepName::ReproTest).unwrap();
    assert_eq!(step.status, StepStatus::Failed, "{report}");
    assert!(
        step.detail
            .contains(&format!("`{id}` ran no test with the patch")),
        "{}",
        step.detail
    );
}

#[test]
fn the_bare_test_name_runs_and_reproduces_the_issue() {
    let Some(repo) = fixed() else { return };
    let report = repo.gate(
        &profile(Language::Go, ""),
        Some(&repro("TestFourthItemIsRefused")),
    );
    let step = report.step(StepName::ReproTest).unwrap();
    assert_eq!(step.status, StepStatus::Passed, "{report}");
}
