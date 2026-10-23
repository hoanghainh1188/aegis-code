//! "Xong khi" of #11 for Python: the Python profile's steps on the fixture
//! repo with sample patches (pytest and ruff needed).

mod common;

use aegis_gate::{Repro, Skip, StepName, StepStatus};
use aegis_lang::Language;
use common::{Repo, profile, statuses, tools_available};

/// coverage.py is not needed to test the step.
const FAKE_COVERAGE: &str = r#"
[gate]
coverage = ["sh", "-c", "echo '<coverage/>' > coverage.xml"]
[coverage]
format = "cobertura"
path = "coverage.xml"
"#;

const BUG: (&str, &str, &str) = (
    "cart/__init__.py",
    "len(self.items) >= MAX_ITEMS",
    "len(self.items) > MAX_ITEMS",
);

const REPRO_TEST: &str = "import pytest

from cart import CartFullError, cart_of


def test_fourth_item_is_refused():
    cart = cart_of(1, 2, 3)
    with pytest.raises(CartFullError):
        cart.add(4)
";

fn ready() -> bool {
    tools_available(&[
        &["python", "-m", "pytest", "--version"],
        &["ruff", "--version"],
    ])
}

#[test]
fn a_fix_with_its_reproduction_test_passes_every_step() {
    if !ready() {
        return;
    }
    let repo = Repo::new("python", &[BUG]);
    repo.replace(BUG.0, BUG.2, BUG.1);
    repo.write("tests/test_fourth.py", REPRO_TEST);
    let repro = Repro {
        test: "tests/test_fourth.py::test_fourth_item_is_refused".into(),
        files: vec!["tests/test_fourth.py".into()],
    };

    let report = repo.gate(&profile(Language::Python, FAKE_COVERAGE), Some(&repro));
    assert_eq!(
        statuses(&report),
        [
            (
                StepName::Dependencies,
                StepStatus::Skipped(Skip::NoDependencyChange)
            ),
            (StepName::Format, StepStatus::Passed),
            (StepName::Lint, StepStatus::Passed),
            (StepName::Typecheck, StepStatus::Skipped(Skip::NotInProfile)),
            (StepName::ReproTest, StepStatus::Passed),
            (StepName::Test, StepStatus::Passed),
            (StepName::Coverage, StepStatus::Passed),
            (StepName::Sonar, StepStatus::Skipped(Skip::NotConfigured)),
            (StepName::Integrity, StepStatus::Passed),
        ],
        "{report}"
    );
}

/// pytest names each case `<test>[<params>]` (#133).
const PARAMETRIZED_REPRO_TEST: &str = "import pytest

from cart import CartFullError, cart_of


@pytest.mark.parametrize(\"extra\", [4, 5])
def test_fourth_item_is_refused(extra):
    cart = cart_of(1, 2, 3)
    with pytest.raises(CartFullError):
        cart.add(extra)
";

#[test]
fn a_parametrized_reproduction_test_is_matched_by_its_name() {
    if !ready() {
        return;
    }
    let repo = Repo::new("python", &[BUG]);
    repo.replace(BUG.0, BUG.2, BUG.1);
    repo.write("tests/test_fourth.py", PARAMETRIZED_REPRO_TEST);
    let repro = Repro {
        test: "tests/test_fourth.py::test_fourth_item_is_refused".into(),
        files: vec!["tests/test_fourth.py".into()],
    };

    let report = repo.gate(&profile(Language::Python, FAKE_COVERAGE), Some(&repro));
    let step = report.step(StepName::ReproTest).unwrap();
    assert_eq!(step.status, StepStatus::Passed, "{report}");
}

#[test]
fn new_failures_are_told_from_ones_the_base_has() {
    if !ready() {
        return;
    }
    // The base already fails one test; the patch breaks another.
    let repo = Repo::new("python", &[BUG]);
    repo.replace(
        "cart/__init__.py",
        "return sum(self.items)",
        "return sum(self.items) + 1",
    );

    let report = repo.gate(&profile(Language::Python, FAKE_COVERAGE), None);
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Failed, "{report}");
    assert!(
        test.detail.starts_with("1 tests fail that pass on the base revision:\n  tests/test_cart.py::test_adds_up_prices"),
        "{}",
        test.detail
    );
}

#[test]
fn a_manifest_change_without_a_lockfile_is_blocked() {
    let repo = Repo::new("python", &[]);
    repo.replace(
        "pyproject.toml",
        "dependencies = []",
        "dependencies = [\"requests\"]",
    );

    let report = repo.gate(&profile(Language::Python, FAKE_COVERAGE), None);
    let deps = report.step(StepName::Dependencies).unwrap();
    assert_eq!(deps.status, StepStatus::Failed, "{report}");
    assert!(
        deps.detail
            .contains("pyproject.toml changed but no lockfile did"),
        "{}",
        deps.detail
    );
}
