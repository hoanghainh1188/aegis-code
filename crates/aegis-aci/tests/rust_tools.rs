//! ACI tools on the Rust fixture repo, through the Rust profile alone
//! (rustfmt must be installed for the formatting tests).

mod common;

use std::process::Command;

use aegis_aci::{EditOutcome, EditRequest, FormatContainer, Hits, LookupAt, Rejection};
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

#[test]
fn editing_a_parent_leaves_its_unformatted_child_modules_alone() {
    // #88: rustfmt given a path also rewrites the files its `mod` lines
    // declare; the profile hands it the edited file on stdin instead.
    if !tool_available(&["rustfmt", "--version"]) {
        return;
    }
    let repo = Scratch::fixture("rust");
    repo.write("src/shop.rs", "mod till;\npub fn open() {}\n");
    repo.write("src/shop/till.rs", "pub fn   ring( ){}\n");
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
    assert_eq!(repo.read("src/shop/till.rs"), "pub fn   ring( ){}\n");
}

#[test]
fn the_sandbox_formatter_reads_the_file_on_stdin_too() {
    // The same edit, with rustfmt in the Rust image (`docker run -i`).
    let image = Command::new("docker")
        .args(["image", "inspect", "aegis-sandbox-rust"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !image {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or the aegis-sandbox-rust image is missing"
        );
        return;
    }
    let repo = Scratch::fixture("rust");
    repo.write("src/shop.rs", "mod till;\npub fn open() {}\n");
    repo.write("src/shop/till.rs", "pub fn   ring( ){}\n");
    let mut aci = repo.aci(rust()).with_format_container(FormatContainer {
        docker: "docker".into(),
        image: "aegis-sandbox-rust".into(),
        mounts: vec![],
    });

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
    assert_eq!(repo.read("src/shop/till.rs"), "pub fn   ring( ){}\n");
}

fn sandbox_image_ready() -> bool {
    let image = Command::new("docker")
        .args(["image", "inspect", "aegis-sandbox-rust"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !image {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or the aegis-sandbox-rust image is missing"
        );
    }
    image
}

fn sandboxed(repo: &Scratch) -> aegis_aci::Aci {
    let mut aci = repo.aci(rust()).with_format_container(FormatContainer {
        docker: "docker".into(),
        image: "aegis-sandbox-rust".into(),
        mounts: vec![],
    });
    aci.start_language_server().unwrap();
    aci
}

/// Waits out indexing: the tools say "still indexing" for up to 30 s each.
fn retrying<T>(mut call: impl FnMut() -> Result<T, aegis_aci::AciError>) -> T {
    for _ in 0..10 {
        match call() {
            Err(e) if e.to_string().contains("still indexing") => continue,
            other => return other.unwrap(),
        }
    }
    panic!("the language server never finished indexing");
}

#[test]
fn references_follow_a_method_called_through_a_trait() {
    // #89: `cart![1, 2].total()` calls `Priced::total` on a `Cart`, inside
    // two macros; a name search cannot tell which `total` it is.
    if !sandbox_image_ready() {
        return;
    }
    let repo = Scratch::fixture("rust");
    let mut aci = sandboxed(&repo);

    let at_call = LookupAt {
        file: Some("src/lib.rs"),
        line: Some(53),
        symbol: Some("total"),
        ..LookupAt::default()
    };
    let definition = retrying(|| aci.find_definition(at_call));
    let lines: Vec<usize> = definition.hits.iter().map(|h| h.line).collect();
    assert_eq!(lines, [33], "{definition}");

    let at_trait = LookupAt {
        symbol: Some("Priced::total"),
        ..LookupAt::default()
    };
    let references = retrying(|| aci.find_references(at_trait));
    let lines: Vec<usize> = references.hits.iter().map(|h| h.line).collect();
    assert_eq!(lines, [19, 33, 53], "{references}");

    // A call the agent adds is seen without reopening anything.
    let outcome = aci
        .edit(EditRequest {
            file: "src/lib.rs",
            from: 34,
            to: 34,
            content: "        let _again = Cart::default().total();\n        self.items.iter().sum()",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
    let after = retrying(|| aci.find_references(at_trait));
    let lines: Vec<usize> = after.hits.iter().map(|h| h.line).collect();
    assert_eq!(lines, [19, 33, 34, 54], "{after}");
    assert!(
        after.to_string().contains("src/lib.rs:34: let _again"),
        "{after}"
    );
}

fn stdin_formatter(argv: &[&str]) -> ProfileOverrides {
    ProfileOverrides {
        edit_format: Some(StepOverride::Run(
            argv.iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        )),
        edit_stdin: Some(true),
        ..ProfileOverrides::default()
    }
}

#[cfg(unix)]
#[test]
fn a_stdin_formatter_s_output_is_what_gets_written() {
    let repo = Scratch::fixture("rust");
    let overrides = stdin_formatter(&["sh", "-c", "cat; echo '// formatted'"]);
    let mut aci = repo.aci(profile(Language::Rust, &overrides));

    let outcome = aci
        .edit(EditRequest {
            file: "src/lib.rs",
            from: 3,
            to: 3,
            content: "pub const MAX_ITEMS: usize = 4;",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
    let written = repo.read("src/lib.rs");
    assert!(
        written.contains("pub const MAX_ITEMS: usize = 4;"),
        "{written}"
    );
    assert!(written.ends_with("// formatted\n"), "{written}");
}

#[cfg(unix)]
#[test]
fn a_stdin_formatter_that_prints_nothing_does_not_empty_the_file() {
    let repo = Scratch::fixture("rust");
    let before = repo.read("src/lib.rs");
    let mut aci = repo.aci(profile(Language::Rust, &stdin_formatter(&["true"])));

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
                reason: Rejection::Formatter { .. },
                ..
            }
        ),
        "{outcome}"
    );
    assert!(outcome.to_string().contains("printed nothing"), "{outcome}");
    assert_eq!(repo.read("src/lib.rs"), before);
}

#[cfg(unix)]
#[test]
fn a_formatter_leaving_a_process_behind_does_not_block_the_edit() {
    // The background `sleep` keeps the output pipes open after `sh` exits.
    let argv = ["sh", "-c", "sleep 30 & echo bad >&2; exit 3", "{file}"];
    let overrides = ProfileOverrides {
        edit_format: Some(StepOverride::Run(
            argv.iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        )),
        ..ProfileOverrides::default()
    };
    let repo = Scratch::fixture("rust");
    let mut aci = repo.aci(profile(Language::Rust, &overrides));
    let started = std::time::Instant::now();

    let outcome = aci
        .edit(EditRequest {
            file: "src/lib.rs",
            from: 3,
            to: 3,
            content: "pub const MAX_ITEMS: usize = 4;",
        })
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        matches!(
            &outcome,
            EditOutcome::Rejected {
                reason: Rejection::Formatter { code: Some(3), .. },
                ..
            }
        ),
        "{outcome}"
    );
}
