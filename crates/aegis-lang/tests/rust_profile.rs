//! "Xong khi" of #64: ACI and gate work on the Rust fixture repo through the
//! profile alone. Runs the resolved gate commands with the local toolchain
//! (rustfmt and clippy must be installed) on a copy of the fixture.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use aegis_lang::{
    AllowList, CommandTemplate, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile,
    SymbolKind, read_lockfile, symbols, syntax_errors,
};
use common::{Workspace, fixture};

fn rust() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::Rust)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

fn plain(c: &CommandTemplate) -> Vec<String> {
    c.render(&[]).unwrap()
}

#[test]
fn detects_the_fixture_as_rust() {
    let entries: Vec<String> = std::fs::read_dir(fixture("rust"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::Rust]
    );
    assert_eq!(
        profiles.language_of(Path::new("src/lib.rs")),
        Some(Language::Rust)
    );
}

#[test]
fn gate_steps_pass_on_the_fixture() {
    let profile = rust();
    let ws = Workspace::new("rust");
    let gate = &profile.gate;
    ws.run_ok("format", &plain(gate.format.command().unwrap()));
    ws.run_ok("lint", &plain(gate.lint.command().unwrap()));
    ws.run_ok("test", &plain(&gate.test));
    let repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(Placeholder::Test, "tests::adds_up_prices")])
        .unwrap();
    let out = ws.run(&repro);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 passed"), "repro ran one test: {stdout}");
}

#[test]
fn format_step_fails_on_unformatted_code() {
    let profile = rust();
    let ws = Workspace::new("rust");
    let lib = ws.repo().join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(
        &lib,
        text.replace(
            "pub const MAX_ITEMS: usize = 3;",
            "pub const   MAX_ITEMS:usize=3;",
        ),
    )
    .unwrap();
    let out = ws.run(&plain(profile.gate.format.command().unwrap()));
    assert!(!out.status.success(), "format check must fail");
}

#[test]
fn edit_format_formats_only_the_text_it_is_given() {
    // The profile's formatter reads the file on stdin (#88): it cannot
    // reach the `mod` files the text declares.
    let profile = rust();
    assert!(profile.edit_stdin);
    let ws = Workspace::new("rust");
    let lib = ws.repo().join("src/lib.rs");
    let before_lib = std::fs::read_to_string(&lib).unwrap();
    let messy = "mod lib;\npub fn   messy( a:u32 )->u32{a+1}\n";

    assert!(syntax_errors(Language::Rust, messy).unwrap().is_empty());
    let argv = profile
        .edit_format
        .command()
        .unwrap()
        .render(&[(Placeholder::File, "src/extra.rs")])
        .unwrap();
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(ws.repo())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("rustfmt must be installed");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(messy.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());

    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "mod lib;\npub fn messy(a: u32) -> u32 {\n    a + 1\n}\n"
    );
    assert_eq!(std::fs::read_to_string(&lib).unwrap(), before_lib);
}

#[test]
fn symbols_of_the_fixture() {
    let source = std::fs::read_to_string(fixture("rust").join("src/lib.rs")).unwrap();
    let all = symbols(Language::Rust, &source).unwrap();
    let kind = |name: &str| all.iter().find(|s| s.name == name).map(|s| s.kind);
    assert_eq!(kind("MAX_ITEMS"), Some(SymbolKind::Constant));
    assert_eq!(kind("Total"), Some(SymbolKind::TypeAlias));
    assert_eq!(kind("Cart"), Some(SymbolKind::Struct));
    assert_eq!(kind("CartError"), Some(SymbolKind::Enum));
    assert_eq!(kind("Priced"), Some(SymbolKind::Trait));
    assert_eq!(kind("cart"), Some(SymbolKind::Macro));
    assert_eq!(kind("tests"), Some(SymbolKind::Module));
    let add = all.iter().find(|s| s.name == "add").unwrap();
    assert_eq!(
        (add.kind, add.container.as_deref()),
        (SymbolKind::Method, Some("Cart"))
    );
    let test_fn = all.iter().find(|s| s.name == "adds_up_prices").unwrap();
    assert_eq!(
        (test_fn.kind, test_fn.container.as_deref()),
        (SymbolKind::Function, Some("tests"))
    );
}

#[test]
fn fixture_lockfile_has_no_external_crates_and_adding_one_is_blocked() {
    let lock = std::fs::read_to_string(fixture("rust").join("Cargo.lock")).unwrap();
    assert!(
        read_lockfile(Language::Rust, "Cargo.lock", &lock)
            .unwrap()
            .is_empty()
    );

    let profile = rust().profile;
    assert!(profile.touches_dependencies(&["Cargo.toml"]));
    let after = format!(
        "{lock}\n[[package]]\nname = \"left-pad\"\nversion = \"0.3.1\"\nsource = \"{}\"\n",
        profile.dependencies.official_source
    );
    let violations = profile
        .dependency_violations("Cargo.lock", Some(&lock), &after, &AllowList::default())
        .unwrap();
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].dependency.name, "left-pad");
}
