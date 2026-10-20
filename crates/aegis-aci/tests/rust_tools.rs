//! ACI tools on the Rust fixture repo, through the Rust profile alone
//! (rustfmt must be installed for the formatting tests).

mod common;

use aegis_aci::{EditOutcome, EditRequest, Hits, Rejection};
use aegis_lang::{Language, ProfileOverrides, StepOverride};
use common::{Scratch, profile, tool_available};

fn rust() -> aegis_lang::ResolvedProfile {
    profile(Language::Rust, &ProfileOverrides::default())
}

#[test]
fn search_symbol_finds_definitions_with_lines_and_signature() {
    let repo = Scratch::fixture("rust");
    let aci = repo.aci(rust());

    let result = aci.search_symbol("Cart::add").unwrap();
    let Hits::Found(hits) = &result.hits else {
        panic!("{result}")
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].to_string(),
        "src/lib.rs:23-29 method Cart::add: \
         pub fn add(&mut self, price: u32) -> Result<(), CartError>"
    );

    let structs = aci.search_symbol("Cart").unwrap().to_string();
    assert!(structs.starts_with("3 matches for \"Cart\":\nsrc/lib.rs:9-11 struct Cart: pub struct Cart\nsrc/lib.rs:22-30 impl Cart: impl Cart\n"), "{structs}");
    assert_eq!(
        aci.search_symbol("nothing_like_this").unwrap().to_string(),
        "No matches for \"nothing_like_this\"."
    );
}

#[test]
fn search_text_and_find_file_report_paths_and_lines() {
    let repo = Scratch::fixture("rust");
    let aci = repo.aci(rust());

    assert_eq!(
        aci.search_text("CartError::Full", Some("src"))
            .unwrap()
            .to_string(),
        "2 matches for \"CartError::Full\":\n\
         src/lib.rs:25:             return Err(CartError::Full);\n\
         src/lib.rs:59:         assert_eq!(cart.add(4), Err(CartError::Full));"
    );
    assert_eq!(
        aci.find_file("*.toml", None).unwrap().to_string(),
        "1 matches for \"*.toml\":\nCargo.toml"
    );
}

#[test]
fn open_shows_numbered_lines() {
    let repo = Scratch::fixture("rust");
    let mut aci = repo.aci(rust());

    let window = aci.open("src/lib.rs", None).unwrap();
    assert_eq!(window.total, 61);
    assert_eq!(window.lines.len(), 61);
    let text = window.to_string();
    assert!(
        text.starts_with("[File: src/lib.rs (61 lines total)]\n1://! Small crate"),
        "{text}"
    );
    assert!(text.ends_with("\n61:}"), "{text}");
}

#[test]
fn edit_is_formatted_with_the_profile_formatter() {
    if !tool_available(&["rustfmt", "--version"]) {
        return;
    }
    let repo = Scratch::fixture("rust");
    let mut aci = repo.aci(rust());
    aci.open("src/lib.rs", Some(30)).unwrap();

    let outcome = aci
        .edit(EditRequest {
            file: "src/lib.rs",
            from: 32,
            to: 36,
            content: "impl Priced for Cart {\nfn total(&self)->Total{ self.items.iter().copied().sum() }\n}",
        })
        .unwrap();
    let EditOutcome::Applied(snippet) = &outcome else {
        panic!("{outcome}")
    };
    let after = repo.read("src/lib.rs");
    assert!(after.contains(
        "    fn total(&self) -> Total {\n        self.items.iter().copied().sum()\n    }"
    ));
    assert!(!repo.repo().join("src/.aegis-edit-lib.rs").exists());
    assert_eq!(snippet.first, 27);
    assert!(
        outcome
            .to_string()
            .starts_with("Edit applied.\n[File: src/lib.rs (61 lines total), lines 27-41]")
    );
    // The open file is reloaded.
    assert_eq!(
        aci.goto(61).unwrap().lines[32],
        "    fn total(&self) -> Total {"
    );
}

#[test]
fn edit_with_a_syntax_error_is_refused_and_not_written() {
    let repo = Scratch::fixture("rust");
    let mut aci = repo.aci(rust());
    let before = repo.read("src/lib.rs");

    let outcome = aci
        .edit(EditRequest {
            file: "src/lib.rs",
            from: 34,
            to: 34,
            content: "        self.items.iter().sum(",
        })
        .unwrap();
    let EditOutcome::Rejected { reason, proposed } = &outcome else {
        panic!("{outcome}")
    };
    assert!(matches!(reason, Rejection::Syntax(e) if !e.is_empty()));
    assert_eq!((proposed.first, proposed.lines.len()), (29, 11));
    assert_eq!(proposed.lines[5], "        self.items.iter().sum(");
    assert!(outcome.to_string().starts_with(
        "Edit rejected, `src/lib.rs` is unchanged: the edit adds syntax errors:\n  line "
    ));
    assert_eq!(repo.read("src/lib.rs"), before);
}

#[test]
fn edit_refused_by_the_formatter_is_not_written() {
    let overrides = ProfileOverrides {
        edit_format: Some(StepOverride::Run(
            vec!["false".to_owned(), "{file}".to_owned()]
                .try_into()
                .unwrap(),
        )),
        ..ProfileOverrides::default()
    };
    let repo = Scratch::fixture("rust");
    let mut aci = repo.aci(profile(Language::Rust, &overrides));
    let before = repo.read("src/lib.rs");

    let outcome = aci
        .edit(EditRequest {
            file: "src/lib.rs",
            from: 3,
            to: 3,
            content: "pub const MAX_ITEMS: usize = 4;",
        })
        .unwrap();
    assert!(
        matches!(
            &outcome,
            EditOutcome::Rejected {
                reason: Rejection::Formatter { code: Some(1), .. },
                ..
            }
        ),
        "{outcome}"
    );
    assert_eq!(repo.read("src/lib.rs"), before);
    assert!(!repo.repo().join("src/.aegis-edit-lib.rs").exists());
}

#[test]
fn files_the_profile_does_not_own_are_written_as_is() {
    let repo = Scratch::fixture("rust");
    let mut aci = repo.aci(rust());
    let outcome = aci
        .edit(EditRequest {
            file: "Cargo.toml",
            from: 1,
            to: 0,
            content: "# edited   without   formatting",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
    assert!(
        repo.read("Cargo.toml")
            .starts_with("# edited   without   formatting\n")
    );
}

#[test]
fn edit_of_a_file_with_out_of_line_modules_is_formatted() {
    if !tool_available(&["rustfmt", "--version"]) {
        return;
    }
    let repo = Scratch::fixture("rust");
    repo.write("src/shop.rs", "mod till;\npub fn open() {}\n");
    repo.write("src/shop/till.rs", "pub fn ring() {}\n");
    let mut aci = repo.aci(rust());

    let outcome = aci
        .edit(EditRequest {
            file: "src/shop.rs",
            from: 2,
            to: 2,
            content: "pub fn open( ) {   }",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
    assert_eq!(repo.read("src/shop.rs"), "mod till;\npub fn open() {}\n");
}
