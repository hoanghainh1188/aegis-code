//! ACI tools on a repo of two languages (#71): the Rust fixture at the
//! root, the Python one under `py/` with its own profile. Each file is read
//! with its part's grammar, `run_tests` runs a test of `py/` there, and
//! each part's language server answers for its files (#199).

mod common;

use std::path::Path;
use std::process::Command;

use aegis_aci::{Aci, EditOutcome, EditRequest, FormatContainer, LookupAt, Rejection};
use aegis_lang::{Language, ProfileOverrides, RepoProfiles, ResolvedProfile};
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

/// A language server that writes to `argv[1]` its root and what it is
/// given, and finds every definition on the first line of the file asked
/// about.
const FAKE_SERVER: &str = r#"
import json, sys
record = open(sys.argv[1], "a")
def read():
    n = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line: sys.exit(0)
        if line.strip() == b"": break
        k, v = line.decode().split(":", 1)
        if k.lower() == "content-length": n = int(v)
    return json.loads(sys.stdin.buffer.read(n))
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b); sys.stdout.buffer.flush()
while True:
    m = read()
    method = m.get("method")
    if method == "initialize":
        record.write("root %s\n" % m["params"]["rootUri"]); record.flush()
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {}}})
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        d = m["params"]["textDocument"]
        record.write("%s %s %s\n" % (method, d.get("languageId", "-"), d["uri"])); record.flush()
    elif method == "textDocument/definition":
        uri = m["params"]["textDocument"]["uri"]
        at = {"line": 0, "character": 0}
        send({"jsonrpc": "2.0", "id": m["id"], "result": [{"uri": uri, "range": {"start": at, "end": at}}]})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": m["id"], "result": None})
"#;

fn with_fake_server(mut profile: ResolvedProfile, record: &Path) -> ResolvedProfile {
    let argv: Vec<String> = vec![
        "python3".into(),
        "-c".into(),
        FAKE_SERVER.into(),
        record.display().to_string(),
    ];
    profile.profile.aci.lsp = Some(argv.try_into().unwrap());
    profile.profile.aci.lsp_options = None;
    profile
}

fn python3_ready() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

fn at(file: &str) -> LookupAt<'_> {
    LookupAt {
        file: Some(file),
        line: Some(2),
        column: Some(1),
        ..LookupAt::default()
    }
}

#[test]
fn each_part_has_its_own_language_server_chosen_by_the_file() {
    if !python3_ready() {
        return;
    }
    let repo = Scratch::two_languages();
    let (root_record, py_record) = (repo.outside().join("root"), repo.outside().join("py"));
    std::fs::create_dir_all(repo.outside()).unwrap();
    let profiles = RepoProfiles::new(
        with_fake_server(
            profile(Language::Rust, &ProfileOverrides::default()),
            &root_record,
        ),
        vec![(
            "py".into(),
            with_fake_server(
                profile(
                    Language::Python,
                    &toml::from_str("edit_format = false").unwrap(),
                ),
                &py_record,
            ),
        )],
    )
    .unwrap();
    let mut aci = Aci::new(&repo.repo(), profiles).unwrap();
    assert!(aci.has_language_server());
    aci.start_language_server().unwrap();

    let py = aci.find_definition(at("py/cart/__init__.py")).unwrap();
    assert_eq!(py.hits.len(), 1, "{py}");
    assert_eq!(
        (py.hits[0].path.as_str(), py.hits[0].line),
        ("py/cart/__init__.py", 1)
    );
    let rust = aci.find_definition(at("src/lib.rs")).unwrap();
    assert_eq!(
        (rust.hits[0].path.as_str(), rust.hits[0].line),
        ("src/lib.rs", 1)
    );

    // A file the agent writes goes to its own part's server only.
    let text = repo.read("py/cart/__init__.py");
    let outcome = aci
        .edit(EditRequest {
            file: "py/cart/__init__.py",
            from: 1,
            to: 1,
            content: text.lines().next().unwrap(),
        })
        .unwrap();
    assert!(matches!(outcome, EditOutcome::Applied(_)), "{outcome}");
    aci.find_definition(at("src/lib.rs")).unwrap();
    aci.find_definition(at("py/cart/__init__.py")).unwrap();
    let repo_uri = format!("file://{}", aci.workspace().root().display());
    drop(aci);

    let root = std::fs::read_to_string(&root_record).unwrap();
    let py = std::fs::read_to_string(&py_record).unwrap();
    assert!(root.starts_with(&format!("root {repo_uri}\n")), "{root}");
    assert!(py.starts_with(&format!("root {repo_uri}/py\n")), "{py}");
    assert!(
        root.contains(&format!("didOpen rust {repo_uri}/src/lib.rs")),
        "{root}"
    );
    assert!(!root.contains("cart/__init__.py"), "{root}");
    assert!(
        py.contains(&format!("didOpen python {repo_uri}/py/cart/__init__.py")),
        "{py}"
    );
    assert!(
        py.contains(&format!("didChange - {repo_uri}/py/cart/__init__.py")),
        "{py}"
    );
}

#[test]
fn a_part_without_a_language_server_says_so() {
    if !python3_ready() {
        return;
    }
    let repo = Scratch::two_languages();
    std::fs::create_dir_all(repo.outside()).unwrap();
    let mut python = profile(Language::Python, &ProfileOverrides::default());
    python.profile.aci.lsp = None;
    let profiles = RepoProfiles::new(
        with_fake_server(
            profile(Language::Rust, &ProfileOverrides::default()),
            &repo.outside().join("root"),
        ),
        vec![("py".into(), python)],
    )
    .unwrap();
    let mut aci = Aci::new(&repo.repo(), profiles).unwrap();
    aci.start_language_server().unwrap();
    let err = aci
        .find_definition(at("py/cart/__init__.py"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("`py/` (Python)"), "{err}");
    assert!(aci.find_definition(at("src/lib.rs")).is_ok());
}
