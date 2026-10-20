//! ACI tools on the Python fixture repo, through the Python profile alone
//! (ruff must be installed for the formatting test).

mod common;

use aegis_aci::{EditOutcome, EditRequest, Hits, Rejection};
use aegis_lang::{Language, ProfileOverrides};
use common::{Scratch, profile, tool_available};

fn python() -> aegis_lang::ResolvedProfile {
    profile(Language::Python, &ProfileOverrides::default())
}

#[test]
fn search_symbol_finds_methods_and_decorated_definitions() {
    let repo = Scratch::fixture("python");
    let aci = repo.aci(python());

    assert_eq!(
        aci.search_symbol("Cart.add").unwrap().to_string(),
        "1 matches for \"Cart.add\":\n\
         cart/__init__.py:16-19 method Cart::add: def add(self, price: int) -> None:"
    );
    // The range of a decorated definition starts at its decorator.
    let result = aci.search_symbol("total").unwrap();
    let Hits::Found(hits) = &result.hits else {
        panic!("{result}")
    };
    assert_eq!(hits.len(), 1, "{result}");
    assert_eq!(hits[0].symbol.start_line, 21);
    assert_eq!(hits[0].symbol.end_line, 23);
}

#[test]
fn search_text_limits_to_a_directory() {
    let repo = Scratch::fixture("python");
    let aci = repo.aci(python());

    let all = aci.search_text("cart_of", None).unwrap().to_string();
    assert!(all.starts_with("4 matches"), "{all}");
    assert_eq!(
        aci.search_text("cart_of", Some("cart"))
            .unwrap()
            .to_string(),
        "1 matches for \"cart_of\":\ncart/__init__.py:26: def cart_of(*prices: int) -> Cart:"
    );
    assert_eq!(
        aci.find_file("test_*.py", None).unwrap().to_string(),
        "1 matches for \"test_*.py\":\ntests/test_cart.py"
    );
}

#[test]
fn edit_with_a_syntax_error_is_refused_and_not_written() {
    let repo = Scratch::fixture("python");
    let mut aci = repo.aci(python());
    let before = repo.read("cart/__init__.py");

    let outcome = aci
        .edit(EditRequest {
            file: "cart/__init__.py",
            from: 26,
            to: 26,
            content: "def cart_of(*prices: int -> Cart:",
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
    assert_eq!(repo.read("cart/__init__.py"), before);
}

#[test]
fn edit_is_formatted_with_the_profile_formatter() {
    if !tool_available(&["ruff", "--version"]) {
        return;
    }
    let repo = Scratch::fixture("python");
    let mut aci = repo.aci(python());

    let outcome = aci
        .edit(EditRequest {
            file: "cart/__init__.py",
            from: 23,
            to: 23,
            content: "        return sum( self.items ,start = 0 )",
        })
        .unwrap();
    let EditOutcome::Applied(snippet) = &outcome else {
        panic!("{outcome}")
    };
    assert_eq!(snippet.lines[5], "        return sum(self.items, start=0)");
    assert!(
        repo.read("cart/__init__.py")
            .contains("return sum(self.items, start=0)\n")
    );
    assert!(!repo.repo().join("cart/.aegis-edit-__init__.py").exists());
}
