//! ACI tools on a repo of two languages (#71): the Rust fixture at the
//! root, the Python one under `py/` with its own profile. Each file is read
//! with its part's grammar, and `run_tests` runs a test of `py/` there.

mod common;

use aegis_aci::{Aci, EditOutcome, EditRequest, FormatContainer, Rejection};
use aegis_lang::{Language, ProfileOverrides, RepoProfiles};
use common::{Scratch, profile};

fn aci(repo: &Scratch, python: ProfileOverrides) -> Aci {
    let profiles = RepoProfiles::new(
        profile(Language::Rust, &ProfileOverrides::default()),
        vec![("py".into(), profile(Language::Python, &python))],
    )
    .unwrap();
    Aci::new(&repo.repo(), profiles).unwrap()
}

#[test]
fn search_symbol_finds_definitions_of_both_languages() {
    let repo = Scratch::two_languages();
    let aci = aci(&repo, ProfileOverrides::default());
    let found = aci.search_symbol("Cart::add").unwrap().to_string();
    assert!(found.starts_with("2 matches"), "{found}");
    assert!(
        found.contains("py/cart/__init__.py:16-19 method Cart::add"),
        "{found}"
    );
    assert!(
        found.contains("src/lib.rs:23-29 method Cart::add"),
        "{found}"
    );
}

#[test]
fn an_edit_is_checked_with_the_grammar_of_its_part() {
    let repo = Scratch::two_languages();
    let mut aci = aci(&repo, ProfileOverrides::default());
    let before = repo.read("py/cart/__init__.py");
    // Valid Rust, broken Python.
    let outcome = aci
        .edit(EditRequest {
            file: "py/cart/__init__.py",
            from: 26,
            to: 26,
            content: "fn cart_of(prices: &[u32]) -> Cart {",
        })
        .unwrap();
    assert!(
        matches!(
            &outcome,
            EditOutcome::Rejected {
                reason: Rejection::Syntax(_),
                ..
            }
        ),
        "{outcome}"
    );
    assert_eq!(repo.read("py/cart/__init__.py"), before);
}

#[test]
fn the_module_map_lists_both_parts() {
    let repo = Scratch::two_languages();
    let aci = aci(&repo, ProfileOverrides::default());
    let map = aegis_aci::module_summary(aci.workspace()).unwrap();
    assert!(map.contains("`src`"), "{map}");
    assert!(map.contains("`py/cart`"), "{map}");
}

#[test]
fn a_test_of_a_part_runs_in_its_directory_with_its_command() {
    let repo = Scratch::two_languages();
    let overrides: ProfileOverrides = toml::from_str(
        r#"
[gate]
repro_test = ["sh", "-c", "test -f pyproject.toml && test -f \"$1\"", "sh", "{test}"]
"#,
    )
    .unwrap();
    let mut aci = aci(&repo, overrides);
    let run = aci.run_tests("py/tests/test_cart.py").unwrap();
    assert!(run.to_string().starts_with("PASSED"), "{run}");
}

#[test]
fn a_part_s_formatter_runs_in_its_image_with_its_dependency_dirs() {
    // #198: the formatter is the version in the part's own `.venv`.
    let repo = Scratch::two_languages();
    let venv = (
        "/cache/parts/py/deps/.venv".to_owned(),
        "/work/.venv".to_owned(),
    );
    let aci = aci(&repo, ProfileOverrides::default())
        .with_format_container(FormatContainer {
            docker: "docker".into(),
            image: "aegis-sandbox-rust".into(),
            mounts: vec![("/cache/registry".into(), "/usr/local/cargo/registry".into())],
        })
        .with_part_mounts([("py".to_owned(), vec![venv.clone()])].into());
    let ws = aci.workspace();
    let py = ws.format_container_of(&ws.profiles().parts()[1]).unwrap();
    assert_eq!(py.image, "aegis-sandbox-python");
    assert_eq!(py.mounts, [venv]);
    let root = ws.format_container_of(&ws.profiles().parts()[0]).unwrap();
    assert_eq!(root.mounts[0].1, "/usr/local/cargo/registry");
}
