//! "Xong khi" of #65: ACI and gate work on the Python fixture repo through
//! the profile alone, and a patch adding an unknown package is blocked.
//! Runs the resolved gate commands with `ruff`, `pytest` and `pytest-cov`
//! from `PATH`; skipped when they are missing unless
//! `AEGIS_REQUIRE_PROFILE_TOOLS=1` (CI).

mod common;

use std::path::Path;

use aegis_lang::{
    AllowList, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile, SymbolKind,
    symbols, syntax_errors,
};
use common::{Workspace, fixture, tools_available};

fn python() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::Python)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

fn tools() -> bool {
    tools_available(&[
        &["ruff", "--version"],
        &["python", "-m", "pytest", "--version"],
        &["python", "-c", "import pytest_cov"],
    ])
}

#[test]
fn detects_the_fixture_as_python() {
    let entries: Vec<String> = std::fs::read_dir(fixture("python"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::Python]
    );
    assert_eq!(
        profiles.language_of(Path::new("cart/__init__.py")),
        Some(Language::Python)
    );
}

#[test]
fn gate_steps_pass_on_the_fixture() {
    if !tools() {
        return;
    }
    let profile = python();
    let ws = Workspace::new("python");
    let gate = &profile.gate;
    ws.run_ok(
        "format",
        &gate.format.command().unwrap().render(&[]).unwrap(),
    );
    ws.run_ok("lint", &gate.lint.command().unwrap().render(&[]).unwrap());
    ws.run_ok("test", &gate.test.render(&[]).unwrap());

    let repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(Placeholder::Test, "tests/test_cart.py::test_adds_up_prices")])
        .unwrap();
    let out = ws.run_ok("repro_test", &repro);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 passed"), "repro ran one test: {stdout}");

    ws.run_ok(
        "coverage",
        &gate.coverage.command().unwrap().render(&[]).unwrap(),
    );
    let report = profile.coverage.as_ref().unwrap();
    let xml = std::fs::read_to_string(ws.repo().join(&report.path)).unwrap();
    assert!(xml.contains("<coverage"), "Cobertura report expected");
}

#[test]
fn lint_and_format_fail_on_bad_code() {
    if !tools() {
        return;
    }
    let profile = python();
    let ws = Workspace::new("python");
    let module = ws.repo().join("cart/__init__.py");
    let text = std::fs::read_to_string(&module).unwrap();
    std::fs::write(
        &module,
        text.replace("MAX_ITEMS = 3", "import os\nMAX_ITEMS=3"),
    )
    .unwrap();
    let gate = &profile.gate;
    let format = ws.run(&gate.format.command().unwrap().render(&[]).unwrap());
    assert!(!format.status.success(), "format check must fail");
    let lint = ws.run(&gate.lint.command().unwrap().render(&[]).unwrap());
    assert!(!lint.status.success(), "unused import must fail lint");
}

#[test]
fn edit_format_formats_only_the_edited_file() {
    if !tools() {
        return;
    }
    let profile = python();
    let ws = Workspace::new("python");
    let messy = "def   messy( a ):\n  return a+1\n";
    std::fs::write(ws.repo().join("cart/extra.py"), messy).unwrap();
    let module = ws.repo().join("cart/__init__.py");
    let before = std::fs::read_to_string(&module).unwrap();

    assert!(syntax_errors(Language::Python, messy).unwrap().is_empty());
    let argv = profile
        .edit_format
        .command()
        .unwrap()
        .render(&[(Placeholder::File, "cart/extra.py")])
        .unwrap();
    ws.run_ok("edit.format", &argv);

    let formatted = std::fs::read_to_string(ws.repo().join("cart/extra.py")).unwrap();
    assert_eq!(formatted, "def messy(a):\n    return a + 1\n");
    assert_eq!(std::fs::read_to_string(&module).unwrap(), before);
}

#[test]
fn symbols_of_the_fixture() {
    let source = std::fs::read_to_string(fixture("python").join("cart/__init__.py")).unwrap();
    let all = symbols(Language::Python, &source).unwrap();
    let get = |name: &str| {
        all.iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no `{name}`"))
    };
    assert_eq!(get("MAX_ITEMS").kind, SymbolKind::Constant);
    assert_eq!(get("CartFullError").kind, SymbolKind::Class);
    assert_eq!(get("Cart").kind, SymbolKind::Class);
    assert_eq!(get("cart_of").kind, SymbolKind::Function);
    let add = get("add");
    assert_eq!(
        (add.kind, add.container.as_deref()),
        (SymbolKind::Method, Some("Cart"))
    );
    let total = get("total");
    assert_eq!(
        (total.kind, total.container.as_deref()),
        (SymbolKind::Method, Some("Cart"))
    );
    assert!(source.lines().nth(total.start_line - 1).unwrap().trim() == "@property");
}

#[test]
fn adding_an_unknown_package_is_blocked() {
    let profile = python().profile;
    assert!(profile.touches_dependencies(&["pyproject.toml", "requirements.txt"]));
    let before = "requests==2.32.3\n";
    let after = "requests==2.32.3\nleft-pad==1.0.0\n";
    let violations = profile
        .dependency_violations(
            "requirements.txt",
            Some(before),
            after,
            &AllowList::default(),
        )
        .unwrap();
    assert_eq!(violations.len(), 1);
    assert_eq!(
        violations[0].to_string(),
        "new dependency `left-pad@1.0.0` is not in the allow-list"
    );
}
