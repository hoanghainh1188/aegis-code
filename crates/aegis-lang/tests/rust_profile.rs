//! "Xong khi" of #64: ACI and gate work on the Rust fixture repo through the
//! profile alone. Runs the resolved gate commands with the local toolchain
//! (rustfmt and clippy must be installed) on a copy of the fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use aegis_lang::{
    AllowList, CommandTemplate, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile,
    SymbolKind, read_lockfile, symbols, syntax_errors,
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/rust");

/// Variables the outer `cargo test` / `cargo llvm-cov` set that must not
/// leak into the fixture's build.
const INHERITED: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "LLVM_PROFILE_FILE",
    "CARGO_LLVM_COV",
    "CARGO_LLVM_COV_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
];

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// A fresh copy of the fixture, with its own target dir.
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-rust-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        copy_dir(Path::new(FIXTURE), &root.join("repo"));
        Self { root }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn run(&self, argv: &[String]) -> Output {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(self.repo())
            .env("CARGO_TARGET_DIR", self.root.join("target"));
        for var in INHERITED {
            cmd.env_remove(var);
        }
        cmd.output()
            .unwrap_or_else(|e| panic!("cannot run {argv:?}: {e}"))
    }

    fn run_ok(&self, step: &str, argv: &[String]) {
        let out = self.run(argv);
        assert!(
            out.status.success(),
            "{step} failed: {argv:?}\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "target" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

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
    let entries: Vec<String> = std::fs::read_dir(FIXTURE)
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
    let ws = Workspace::new();
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
    let ws = Workspace::new();
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
fn edit_format_formats_only_the_edited_file() {
    let profile = rust();
    let ws = Workspace::new();
    let lib = ws.repo().join("src/lib.rs");
    let messy = "pub fn   messy( a:u32 )->u32{a+1}\n";
    std::fs::write(ws.repo().join("src/extra.rs"), messy).unwrap();
    let before_lib = std::fs::read_to_string(&lib).unwrap();

    assert!(syntax_errors(Language::Rust, messy).unwrap().is_empty());
    let argv = profile
        .edit_format
        .command()
        .unwrap()
        .render(&[(Placeholder::File, "src/extra.rs")])
        .unwrap();
    ws.run_ok("edit.format", &argv);

    let formatted = std::fs::read_to_string(ws.repo().join("src/extra.rs")).unwrap();
    assert_eq!(formatted, "pub fn messy(a: u32) -> u32 {\n    a + 1\n}\n");
    assert_eq!(std::fs::read_to_string(&lib).unwrap(), before_lib);
}

#[test]
fn symbols_of_the_fixture() {
    let source = std::fs::read_to_string(Path::new(FIXTURE).join("src/lib.rs")).unwrap();
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
    let lock = std::fs::read_to_string(Path::new(FIXTURE).join("Cargo.lock")).unwrap();
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
