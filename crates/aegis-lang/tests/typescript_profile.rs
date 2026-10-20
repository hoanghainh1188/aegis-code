//! "Xong khi" of #67: ACI and gate work on the TypeScript fixture repo
//! through the profile alone, and a patch adding an unknown package is
//! blocked. Runs the profile's fetch step (`npm ci --ignore-scripts`, needs
//! network) and then the gate commands; skipped when `node` / `npm` are
//! missing unless `AEGIS_REQUIRE_PROFILE_TOOLS=1` (CI).

mod common;

use std::path::Path;

use aegis_lang::{
    AllowList, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile, SymbolKind,
    read_lockfile, symbols_in, syntax_errors_in,
};
use common::{Workspace, fixture, tools_available};

fn typescript() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::TypeScript)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

fn tools() -> bool {
    tools_available(&[&["node", "--version"], &["npm", "--version"]])
}

/// A fixture copy with its dependencies fetched by the profile.
fn fetched(profile: &ResolvedProfile) -> Workspace {
    let ws = Workspace::new("typescript");
    ws.run_ok(
        "fetch",
        &profile.fetch.command().unwrap().render(&[]).unwrap(),
    );
    ws
}

#[test]
fn detects_the_fixture_as_typescript() {
    let entries: Vec<String> = std::fs::read_dir(fixture("typescript"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::TypeScript]
    );
    for file in ["src/cart.ts", "App.tsx", "index.mjs"] {
        assert_eq!(
            profiles.language_of(Path::new(file)),
            Some(Language::TypeScript),
            "{file}"
        );
    }
}

#[test]
fn gate_steps_pass_on_the_fixture() {
    if !tools() {
        return;
    }
    let profile = typescript();
    let ws = fetched(&profile);
    let gate = &profile.gate;
    let plain = |c: &aegis_lang::CommandTemplate| c.render(&[]).unwrap();
    ws.run_ok("format", &plain(gate.format.command().unwrap()));
    ws.run_ok("lint", &plain(gate.lint.command().unwrap()));
    ws.run_ok("typecheck", &plain(gate.typecheck.command().unwrap()));
    ws.run_ok("test", &plain(&gate.test));

    let repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(Placeholder::Test, "test/cart.test.ts")])
        .unwrap();
    let out = ws.run_ok("repro_test", &repro);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("pass 2"),
        "ran the given test file: {stdout}"
    );

    ws.run_ok("coverage", &plain(gate.coverage.command().unwrap()));
    let report = profile.coverage.as_ref().unwrap();
    let lcov = std::fs::read_to_string(ws.repo().join(&report.path)).unwrap();
    assert!(lcov.contains("SF:"), "LCOV report expected");
}

#[test]
fn lint_typecheck_and_format_fail_on_bad_code() {
    if !tools() {
        return;
    }
    let profile = typescript();
    let ws = fetched(&profile);
    let file = ws.repo().join("src/cart.ts");
    let good = std::fs::read_to_string(&file).unwrap();
    let gate = &profile.gate;
    let fails = |step: &str, c: &aegis_lang::CommandTemplate| {
        let out = ws.run(&c.render(&[]).unwrap());
        assert!(!out.status.success(), "{step} must fail");
    };

    std::fs::write(
        &file,
        good.replace("export const MAX_ITEMS = 3;", "export const   MAX_ITEMS=3"),
    )
    .unwrap();
    fails("format", gate.format.command().unwrap());

    std::fs::write(&file, format!("{good}\nexport const loose: any = 1;\n")).unwrap();
    fails("lint", gate.lint.command().unwrap());

    std::fs::write(
        &file,
        format!("{good}\nexport const wrong: number = \"text\";\n"),
    )
    .unwrap();
    fails("typecheck", gate.typecheck.command().unwrap());
}

#[test]
fn edit_format_formats_only_the_edited_file() {
    if !tools() {
        return;
    }
    let profile = typescript();
    let ws = fetched(&profile);
    let messy = "export   function messy( a:number ){return a+1}\n";
    std::fs::write(ws.repo().join("src/extra.ts"), messy).unwrap();
    let before = std::fs::read_to_string(ws.repo().join("src/cart.ts")).unwrap();

    assert!(
        syntax_errors_in(Language::TypeScript, Some(Path::new("src/extra.ts")), messy)
            .unwrap()
            .is_empty()
    );
    let argv = profile
        .edit_format
        .command()
        .unwrap()
        .render(&[(Placeholder::File, "src/extra.ts")])
        .unwrap();
    ws.run_ok("edit.format", &argv);

    let formatted = std::fs::read_to_string(ws.repo().join("src/extra.ts")).unwrap();
    assert_eq!(
        formatted,
        "export function messy(a: number) {\n  return a + 1;\n}\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.repo().join("src/cart.ts")).unwrap(),
        before
    );
}

#[test]
fn symbols_of_the_fixture() {
    let path = Path::new("src/cart.ts");
    let source = std::fs::read_to_string(fixture("typescript").join(path)).unwrap();
    let all = symbols_in(Language::TypeScript, Some(path), &source).unwrap();
    let get = |name: &str, kind: SymbolKind| {
        all.iter()
            .find(|s| s.name == name && s.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} `{name}` in {all:#?}"))
    };
    get("MAX_ITEMS", SymbolKind::Variable);
    get("Total", SymbolKind::TypeAlias);
    get("Priced", SymbolKind::Interface);
    get("CartFullError", SymbolKind::Class);
    get("cartOf", SymbolKind::Function);
    assert_eq!(
        get("add", SymbolKind::Method).container.as_deref(),
        Some("Cart")
    );
    let totals: Vec<_> = all
        .iter()
        .filter(|s| s.name == "total" && s.kind == SymbolKind::Method)
        .map(|s| s.container.as_deref())
        .collect();
    assert_eq!(totals, vec![Some("Priced"), Some("Cart")]);
}

#[test]
fn fixture_lockfile_is_all_npm_and_adding_a_package_is_blocked() {
    let profile = typescript().profile;
    let lock = std::fs::read_to_string(fixture("typescript").join("package-lock.json")).unwrap();
    let deps = read_lockfile(Language::TypeScript, "package-lock.json", &lock).unwrap();
    assert!(deps.len() > 50, "real dependency tree");
    assert!(
        deps.iter()
            .all(|d| d.source == profile.dependencies.official_source)
    );
    assert!(profile.touches_dependencies(&["package.json"]));

    let mut value: serde_json::Value = serde_json::from_str(&lock).unwrap();
    value["packages"]["node_modules/left-pad"] = serde_json::json!({
        "version": "1.3.0",
        "resolved": "https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz"
    });
    let after = serde_json::to_string(&value).unwrap();
    let v = profile
        .dependency_violations(
            "package-lock.json",
            Some(&lock),
            &after,
            &AllowList::default(),
        )
        .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].dependency.name, "left-pad");
    assert!(matches!(
        read_lockfile(Language::TypeScript, "yarn.lock", ""),
        Err(aegis_lang::LangError::UnsupportedLockfile { .. })
    ));
}
