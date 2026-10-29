//! Limits shared by every profile: paths stay in the workspace, at most 50
//! results, 100-line windows, edit ranges.

mod common;

use aegis_aci::{AciError, EditOutcome, EditRequest, Hits, MAX_RESULTS, WINDOW};
use aegis_lang::{Language, ProfileOverrides, StepOverride};
use common::{Scratch, profile};

fn rust_repo() -> (Scratch, aegis_aci::Aci) {
    let repo = Scratch::empty();
    repo.write("src/lib.rs", "pub fn one() {}\n");
    let aci = repo.aci(profile(Language::Rust, &ProfileOverrides::default()));
    (repo, aci)
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("// line {i}\n")).collect()
}

#[test]
fn paths_outside_the_workspace_are_refused() {
    let (repo, mut aci) = rust_repo();
    std::fs::create_dir_all(repo.outside()).unwrap();
    std::fs::write(repo.outside().join("secret.rs"), "fn secret() {}\n").unwrap();
    let absolute = repo.outside().join("secret.rs").display().to_string();

    for path in [
        "../outside/secret.rs",
        "src/../../outside/secret.rs",
        absolute.as_str(),
        ".git/config",
    ] {
        assert_eq!(
            aci.open(path, None).unwrap_err(),
            AciError::OutsideWorkspace(path.to_owned()),
            "{path}"
        );
    }
    assert_eq!(
        aci.search_text("fn", Some("..")).unwrap_err(),
        AciError::OutsideWorkspace("..".to_owned())
    );
    assert_eq!(
        aci.open("src/missing.rs", None).unwrap_err(),
        AciError::NotFound("src/missing.rs".to_owned())
    );
    assert_eq!(
        aci.open("src", None).unwrap_err(),
        AciError::NotAFile("src".to_owned())
    );
}

#[cfg(unix)]
#[test]
fn symlinks_out_of_the_workspace_are_refused_and_not_walked() {
    let (repo, mut aci) = rust_repo();
    std::fs::create_dir_all(repo.outside()).unwrap();
    std::fs::write(repo.outside().join("secret.rs"), "fn secret() {}\n").unwrap();
    std::os::unix::fs::symlink(
        repo.outside().join("secret.rs"),
        repo.repo().join("src/link.rs"),
    )
    .unwrap();
    std::os::unix::fs::symlink(repo.outside(), repo.repo().join("out")).unwrap();

    assert_eq!(
        aci.open("src/link.rs", None).unwrap_err(),
        AciError::OutsideWorkspace("src/link.rs".to_owned())
    );
    assert_eq!(
        aci.edit(EditRequest {
            file: "out/secret.rs",
            from: 1,
            to: 1,
            content: ""
        })
        .unwrap_err(),
        AciError::OutsideWorkspace("out/secret.rs".to_owned())
    );
    assert_eq!(
        aci.search_symbol("secret").unwrap().to_string(),
        "No matches for \"secret\"."
    );
    assert_eq!(
        aci.search_text("secret", None).unwrap().to_string(),
        "No matches for \"secret\"."
    );
    assert_eq!(
        std::fs::read_to_string(repo.outside().join("secret.rs")).unwrap(),
        "fn secret() {}\n"
    );
}

#[test]
fn more_than_50_results_ask_to_narrow() {
    let (repo, aci) = rust_repo();
    let many: String = (0..=MAX_RESULTS)
        .map(|i| format!("pub fn f() {{}} // {i}\n"))
        .collect();
    repo.write("src/many.rs", &many);
    for i in 0..=MAX_RESULTS {
        repo.write(&format!("data/file{i}.txt"), "x\n");
    }

    assert_eq!(
        aci.search_symbol("f").unwrap().hits,
        Hits::TooMany { total: 51 }
    );
    assert_eq!(
        aci.search_text("pub fn f", None).unwrap().to_string(),
        "51 matches for \"pub fn f\", more than 50. Narrow the search."
    );
    assert_eq!(
        aci.find_file("file*.txt", None).unwrap().hits,
        Hits::TooMany { total: 51 }
    );
    // Exactly 50 is still listed.
    std::fs::remove_file(repo.repo().join("data/file0.txt")).unwrap();
    let Hits::Found(hits) = aci.find_file("file*.txt", None).unwrap().hits else {
        panic!("expected a list")
    };
    assert_eq!(hits.len(), MAX_RESULTS);
}

#[test]
fn build_caches_and_binary_files_are_skipped() {
    let (repo, aci) = rust_repo();
    repo.write("target/debug/gen.rs", "pub fn one() {}\n");
    std::fs::write(repo.repo().join("blob.bin"), [0xff, 0xfe, b'o', b'n', b'e']).unwrap();

    let result = aci.search_text("one", None).unwrap();
    assert_eq!(
        result.to_string(),
        "1 matches for \"one\":\nsrc/lib.rs:1: pub fn one() {}\n\
         (1 files skipped: binary, too large, unreadable or not parsable)"
    );
    assert!(matches!(
        aci.search_symbol(" ").unwrap_err(),
        AciError::InvalidInput(_)
    ));
}

#[test]
fn windows_are_100_lines_with_hidden_counts() {
    let (repo, mut aci) = rust_repo();
    repo.write("src/long.rs", &numbered(250));

    let top = aci.open("src/long.rs", None).unwrap();
    assert_eq!((top.first, top.lines.len()), (1, WINDOW));
    let text = top.to_string();
    assert!(text.starts_with("[File: src/long.rs (250 lines total)]\n1:// line 1\n"));
    assert!(text.ends_with("\n100:// line 100\n(150 more lines below)"));

    let next = aci.scroll_down().unwrap();
    assert_eq!(next.first, 101);
    assert!(
        next.to_string()
            .contains("(100 more lines above)\n101:// line 101")
    );
    // The last window is full and stops at the end.
    assert_eq!(aci.scroll_down().unwrap().first, 151);
    assert_eq!(aci.scroll_down().unwrap().first, 151);
    assert_eq!(aci.scroll_up().unwrap().first, 51);
    assert_eq!(aci.scroll_up().unwrap().first, 1);
    assert_eq!(aci.goto(120).unwrap().first, 70);
    assert_eq!(aci.open("src/long.rs", Some(240)).unwrap().first, 151);
    assert!(matches!(
        aci.goto(251).unwrap_err(),
        AciError::InvalidInput(_)
    ));
    assert!(matches!(
        aci.goto(0).unwrap_err(),
        AciError::InvalidInput(_)
    ));
}

#[test]
fn scrolling_needs_an_open_file() {
    let (_repo, mut aci) = rust_repo();
    assert_eq!(aci.scroll_down().unwrap_err(), AciError::NoOpenFile);
    assert_eq!(aci.goto(1).unwrap_err(), AciError::NoOpenFile);
}

#[test]
fn edit_ranges_insert_replace_and_delete() {
    let (repo, mut aci) = rust_repo();
    repo.write("notes.txt", "a\nb\nc\n");
    let edit = |aci: &mut aegis_aci::Aci, from, to, content| {
        aci.edit(EditRequest {
            file: "notes.txt",
            from,
            to,
            content,
        })
    };

    assert!(matches!(
        edit(&mut aci, 2, 1, "x").unwrap(),
        EditOutcome::Applied(_)
    ));
    assert_eq!(repo.read("notes.txt"), "a\nx\nb\nc\n");
    edit(&mut aci, 2, 3, "y").unwrap();
    assert_eq!(repo.read("notes.txt"), "a\ny\nc\n");
    let outcome = edit(&mut aci, 2, 2, "").unwrap();
    assert_eq!(repo.read("notes.txt"), "a\nc\n");
    assert_eq!(
        outcome.to_string(),
        "Edit applied.\n[File: notes.txt (2 lines total), lines 1-2]\n1:a\n2:c"
    );
    edit(&mut aci, 3, 2, "z").unwrap();
    assert_eq!(repo.read("notes.txt"), "a\nc\nz\n");

    for (from, to) in [(0, 0), (5, 4), (2, 4), (3, 1)] {
        assert!(
            matches!(
                edit(&mut aci, from, to, "w").unwrap_err(),
                AciError::InvalidInput(_)
            ),
            "{from}-{to}"
        );
    }
    assert_eq!(repo.read("notes.txt"), "a\nc\nz\n");
}

#[cfg(unix)]
#[test]
fn git_dir_is_refused_through_symlinks_and_any_case() {
    let (repo, mut aci) = rust_repo();
    repo.write(".git/config", "[core]\n");
    std::os::unix::fs::symlink(repo.repo().join(".git"), repo.repo().join("meta")).unwrap();

    for path in ["meta/config", ".GIT/config", "src/../.git/config"] {
        assert_eq!(
            aci.open(path, None).unwrap_err(),
            AciError::OutsideWorkspace(path.to_owned()),
            "{path}"
        );
    }
    assert_eq!(
        aci.search_text("core", Some("meta")).unwrap_err(),
        AciError::OutsideWorkspace("meta".to_owned())
    );
    assert_eq!(
        aci.search_text("core", None).unwrap().to_string(),
        "No matches for \"core\"."
    );
}

#[test]
fn edits_keep_the_file_line_ending() {
    let (repo, mut aci) = rust_repo();
    repo.write("crlf.txt", "a\r\nb\r\nc\r\n");
    repo.write("lf.txt", "a\nb \"x\r\ny\"\nc\n");

    aci.edit(EditRequest {
        file: "crlf.txt",
        from: 2,
        to: 2,
        content: "B",
    })
    .unwrap();
    assert_eq!(repo.read("crlf.txt"), "a\r\nB\r\nc\r\n");
    aci.edit(EditRequest {
        file: "lf.txt",
        from: 1,
        to: 1,
        content: "A",
    })
    .unwrap();
    assert_eq!(repo.read("lf.txt"), "A\nb \"x\ny\"\nc\n");
}

#[test]
fn a_syntax_error_on_the_edited_lines_is_refused_even_if_one_is_fixed() {
    let (repo, mut aci) = rust_repo();
    repo.write("src/broken.rs", "fn a() {\n    let x = ;\n}\nfn b() {}\n");
    let before = repo.read("src/broken.rs");

    let outcome = aci
        .edit(EditRequest {
            file: "src/broken.rs",
            from: 2,
            to: 4,
            content: "    let x = 1;\n}\nfn b( {}",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Rejected { .. }), "{outcome}");
    assert_eq!(repo.read("src/broken.rs"), before);
    // Fixing the old error alone is fine.
    let outcome = aci
        .edit(EditRequest {
            file: "src/broken.rs",
            from: 2,
            to: 2,
            content: "    let x = 1;",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
}

#[test]
fn an_error_the_file_had_before_does_not_block_an_edit_over_it() {
    // #289: tree-sitter misreads lines that compile (TypeScript's `out`
    // variance); rewriting the whole file keeps them as they are. No
    // formatter: rustfmt would refuse the file anyway.
    let repo = Scratch::empty();
    let no_formatter = ProfileOverrides {
        edit_format: Some(StepOverride::Enabled(false)),
        ..ProfileOverrides::default()
    };
    let mut aci = repo.aci(profile(Language::Rust, &no_formatter));
    repo.write("src/broken.rs", "fn a() {\n    let x = ;\n}\nfn b() {}\n");

    let outcome = aci
        .edit(EditRequest {
            file: "src/broken.rs",
            from: 1,
            to: 4,
            content: "fn a() {\n    let x = ;\n}\nfn b() { c(); }",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
    assert_eq!(
        repo.read("src/broken.rs"),
        "fn a() {\n    let x = ;\n}\nfn b() { c(); }\n"
    );
    // The same rewrite with an error of its own is refused.
    let outcome = aci
        .edit(EditRequest {
            file: "src/broken.rs",
            from: 1,
            to: 4,
            content: "fn a() {\n    let x = ;\n}\nfn b( { c(); }",
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Rejected { .. }), "{outcome}");
}

#[test]
fn create_file_makes_new_files_inside_the_repo_only() {
    let (repo, mut aci) = rust_repo();
    let outcome = aci.create_file("tests/new/case.txt", "one\ntwo").unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
    assert_eq!(repo.read("tests/new/case.txt"), "one\ntwo\n");

    assert!(matches!(
        aci.create_file("tests/new/case.txt", "x").unwrap_err(),
        AciError::InvalidInput(_)
    ));
    for bad in [
        "../outside.txt",
        "/tmp/x.txt",
        ".git/hooks/pre-commit",
        "src/..",
    ] {
        assert_eq!(
            aci.create_file(bad, "x").unwrap_err(),
            AciError::OutsideWorkspace(bad.to_owned()),
            "{bad}"
        );
    }
    // A file that does not parse is not left behind.
    let outcome = aci.create_file("src/broken.rs", "fn broken( {").unwrap();
    assert!(matches!(outcome, EditOutcome::Rejected { .. }), "{outcome}");
    assert!(!repo.repo().join("src/broken.rs").exists());
}

#[cfg(unix)]
#[test]
fn create_file_does_not_follow_a_symlinked_directory_out() {
    let (repo, mut aci) = rust_repo();
    std::fs::create_dir_all(repo.outside()).unwrap();
    std::os::unix::fs::symlink(repo.outside(), repo.repo().join("out")).unwrap();
    assert_eq!(
        aci.create_file("out/planted.txt", "x").unwrap_err(),
        AciError::OutsideWorkspace("out/planted.txt".to_owned())
    );
    assert_eq!(
        aci.create_file("out/deeper/planted.txt", "x").unwrap_err(),
        AciError::OutsideWorkspace("out/deeper/planted.txt".to_owned())
    );
    assert!(!repo.outside().join("planted.txt").exists());
    assert!(!repo.outside().join("deeper").exists());
}

#[test]
fn a_refused_new_file_leaves_no_directories() {
    let (repo, mut aci) = rust_repo();
    let outcome = aci
        .create_file("tests/deep/broken.rs", "fn broken( {")
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Rejected { .. }), "{outcome}");
    assert!(!repo.repo().join("tests").exists());
}
