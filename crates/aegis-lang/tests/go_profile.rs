//! "Xong khi" of #69: ACI and gate work on the Go fixture repo through the
//! profile alone, and a patch adding an unknown module is blocked. Runs the
//! resolved gate commands with `go` and `gofmt` from `PATH`; skipped when
//! they are missing unless `AEGIS_REQUIRE_PROFILE_TOOLS=1` (CI).

mod common;

use std::path::Path;

use aegis_lang::{
    AllowList, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile, SymbolKind,
    read_lockfile, symbols, syntax_errors,
};
use common::{Workspace, fixture, tools_available};

fn go() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::Go)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

fn tools() -> bool {
    tools_available(&[&["go", "version"]])
}

#[test]
fn detects_the_fixture_as_go() {
    let entries: Vec<String> = std::fs::read_dir(fixture("go"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::Go]
    );
    assert_eq!(
        profiles.language_of(Path::new("cart.go")),
        Some(Language::Go)
    );
}

#[test]
fn gate_steps_pass_on_the_fixture() {
    if !tools() {
        return;
    }
    let profile = go();
    let ws = Workspace::new("go");
    let gate = &profile.gate;
    ws.run_ok(
        "format",
        &gate.format.command().unwrap().render(&[]).unwrap(),
    );
    ws.run_ok("lint", &gate.lint.command().unwrap().render(&[]).unwrap());
    ws.run_ok("test", &gate.test.render(&[]).unwrap());

    let mut repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(Placeholder::Test, "^TestAddsUpPrices$")])
        .unwrap();
    repro.push("-v".into());
    let out = ws.run_ok("repro_test", &repro);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches("=== RUN").count(), 1, "one test: {stdout}");

    ws.run_ok(
        "coverage",
        &gate.coverage.command().unwrap().render(&[]).unwrap(),
    );
    let report = profile.coverage.as_ref().unwrap();
    let text = std::fs::read_to_string(ws.repo().join(&report.path)).unwrap();
    assert!(text.starts_with("mode:"), "Go coverprofile expected");
}

#[test]
fn format_and_lint_fail_on_bad_code() {
    if !tools() {
        return;
    }
    let profile = go();
    let ws = Workspace::new("go");
    let file = ws.repo().join("cart.go");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("const MaxItems = 3", "const   MaxItems=3"),
    )
    .unwrap();
    let format = ws.run(&profile.gate.format.command().unwrap().render(&[]).unwrap());
    assert!(!format.status.success(), "format check must fail");

    let ws = Workspace::new("go");
    let file = ws.repo().join("cart.go");
    let text = std::fs::read_to_string(&file).unwrap();
    // go vet: Printf verb with a missing argument.
    let bad = text.replace(
        "import \"errors\"",
        "import (\n\t\"errors\"\n\t\"fmt\"\n)\n\nfunc broken() { fmt.Printf(\"%d\\n\") }",
    );
    std::fs::write(&file, bad).unwrap();
    let lint = ws.run(&profile.gate.lint.command().unwrap().render(&[]).unwrap());
    assert!(!lint.status.success(), "go vet must fail");
}

#[test]
fn edit_format_formats_only_the_edited_file() {
    if !tools() {
        return;
    }
    let profile = go();
    let ws = Workspace::new("go");
    let messy = "package cart\nfunc   messy( a int )int{return a+1}\n";
    std::fs::write(ws.repo().join("extra.go"), messy).unwrap();
    let before = std::fs::read_to_string(ws.repo().join("cart.go")).unwrap();

    assert!(syntax_errors(Language::Go, messy).unwrap().is_empty());
    let argv = profile
        .edit_format
        .command()
        .unwrap()
        .render(&[(Placeholder::File, "extra.go")])
        .unwrap();
    ws.run_ok("edit.format", &argv);

    let formatted = std::fs::read_to_string(ws.repo().join("extra.go")).unwrap();
    assert_eq!(
        formatted,
        "package cart\n\nfunc messy(a int) int { return a + 1 }\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.repo().join("cart.go")).unwrap(),
        before
    );
}

#[test]
fn symbols_of_the_fixture() {
    let source = std::fs::read_to_string(fixture("go").join("cart.go")).unwrap();
    let all = symbols(Language::Go, &source).unwrap();
    let get = |name: &str, kind: SymbolKind| {
        all.iter()
            .find(|s| s.name == name && s.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} `{name}` in {all:#?}"))
    };
    get("MaxItems", SymbolKind::Constant);
    get("ErrFull", SymbolKind::Variable);
    get("Total", SymbolKind::TypeAlias);
    get("Priced", SymbolKind::Interface);
    get("Cart", SymbolKind::Struct);
    get("Of", SymbolKind::Function);
    assert_eq!(
        get("Add", SymbolKind::Method).container.as_deref(),
        Some("Cart")
    );
    let totals: Vec<_> = all
        .iter()
        .filter(|s| s.name == "Total" && s.kind == SymbolKind::Method)
        .map(|s| s.container.as_deref())
        .collect();
    assert_eq!(totals, vec![Some("Priced"), Some("Cart")]);
}

#[test]
fn adding_or_redirecting_a_module_is_blocked() {
    let profile = go().profile;
    assert!(profile.touches_dependencies(&["go.sum"]));
    let before = std::fs::read_to_string(fixture("go").join("go.mod")).unwrap();
    assert!(
        read_lockfile(Language::Go, "go.mod", &before)
            .unwrap()
            .is_empty()
    );

    let added = format!("{before}\nrequire github.com/left/pad v1.0.0\n");
    let v = profile
        .dependency_violations("go.mod", Some(&before), &added, &AllowList::default())
        .unwrap();
    assert_eq!(
        v[0].to_string(),
        "new dependency `github.com/left/pad@v1.0.0` is not in the allow-list"
    );

    let allowed = AllowList::from_toml("allowed = [\"github.com/google/uuid\"]").unwrap();
    let base = format!("{before}\nrequire github.com/google/uuid v1.6.0\n");
    let redirected =
        format!("{base}replace github.com/google/uuid => github.com/evil/uuid v1.6.0\n");
    let v = profile
        .dependency_violations("go.mod", Some(&base), &redirected, &allowed)
        .unwrap();
    assert_eq!(v.len(), 1);
    assert!(
        v[0].to_string()
            .contains("replace+github.com/evil/uuid@v1.6.0")
    );
}
