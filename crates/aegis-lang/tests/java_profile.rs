//! "Xong khi" of #68: ACI and gate work on the Java fixture repo through the
//! profile alone, and a patch adding an unknown dependency or repository is
//! blocked. Runs the profile's fetch step (Maven, needs network) and then the
//! offline gate commands; skipped when `java` / `mvn` are missing unless
//! `AEGIS_REQUIRE_PROFILE_TOOLS=1` (CI).

mod common;

use std::path::Path;

use aegis_lang::{
    AllowList, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile, SkipReason,
    Step, SymbolKind, ViolationKind, read_lockfile, symbols, syntax_errors,
};
use common::{Workspace, fixture, tools_available};

fn java() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::Java)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

fn tools() -> bool {
    tools_available(&[&["java", "-version"], &["mvn", "-v"]])
}

fn fetched(profile: &ResolvedProfile) -> Workspace {
    let ws = Workspace::new("java");
    ws.run_ok(
        "fetch",
        &profile.fetch.command().unwrap().render(&[]).unwrap(),
    );
    ws
}

#[test]
fn detects_the_fixture_as_java() {
    let entries: Vec<String> = std::fs::read_dir(fixture("java"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::Java]
    );
    assert_eq!(
        profiles.language_of(Path::new("src/main/java/example/cart/Cart.java")),
        Some(Language::Java)
    );
}

#[test]
fn profile_invents_no_formatter() {
    let profile = java();
    assert_eq!(profile.gate.format, Step::Skipped(SkipReason::NotInProfile));
    assert_eq!(profile.edit_format, Step::Skipped(SkipReason::NotInProfile));
}

#[test]
fn gate_steps_pass_offline_after_fetch() {
    if !tools() {
        return;
    }
    let profile = java();
    let ws = fetched(&profile);
    let gate = &profile.gate;
    let plain = |c: &aegis_lang::CommandTemplate| c.render(&[]).unwrap();
    ws.run_ok("lint", &plain(gate.lint.command().unwrap()));
    ws.run_ok("test", &plain(&gate.test));

    let mut repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(Placeholder::Test, "CartTest#addsUpPrices")])
        .unwrap();
    repro.retain(|a| a != "-q");
    let out = ws.run_ok("repro_test", &repro);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Tests run: 1,"), "one test: {stdout}");

    ws.run_ok("coverage", &plain(gate.coverage.command().unwrap()));
    let report = profile.coverage.as_ref().unwrap();
    let xml = std::fs::read_to_string(ws.repo().join(&report.path)).unwrap();
    assert!(xml.contains("<report"), "JaCoCo XML expected");
}

#[test]
fn lint_fails_on_code_that_does_not_compile() {
    if !tools() {
        return;
    }
    let profile = java();
    let ws = fetched(&profile);
    let file = ws.repo().join("src/main/java/example/cart/Cart.java");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("items.add(price);", "items.add(\"price\");"),
    )
    .unwrap();
    let out = ws.run(&profile.gate.lint.command().unwrap().render(&[]).unwrap());
    assert!(!out.status.success(), "type error must fail");
}

#[test]
fn symbols_of_the_fixture() {
    let source =
        std::fs::read_to_string(fixture("java").join("src/main/java/example/cart/Cart.java"))
            .unwrap();
    assert!(syntax_errors(Language::Java, &source).unwrap().is_empty());
    let all = symbols(Language::Java, &source).unwrap();
    let get = |name: &str, kind: SymbolKind| {
        all.iter()
            .find(|s| s.name == name && s.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} `{name}` in {all:#?}"))
    };
    get("Cart", SymbolKind::Class);
    get("MAX_ITEMS", SymbolKind::Variable);
    assert_eq!(
        get("add", SymbolKind::Method).container.as_deref(),
        Some("Cart")
    );
    assert_eq!(
        get("of", SymbolKind::Method).container.as_deref(),
        Some("Cart")
    );
    assert_eq!(
        get("CartFullException", SymbolKind::Class)
            .container
            .as_deref(),
        Some("Cart")
    );
}

#[test]
fn adding_a_dependency_or_repository_is_blocked() {
    let profile = java().profile;
    assert!(profile.touches_dependencies(&["pom.xml"]));
    let pom = std::fs::read_to_string(fixture("java").join("pom.xml")).unwrap();
    let deps = read_lockfile(Language::Java, "pom.xml", &pom).unwrap();
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "org.apache.maven.plugins:maven-surefire-plugin",
            "org.junit.jupiter:junit-jupiter"
        ]
    );

    let added = pom.replace(
        "  </dependencies>",
        "    <dependency><groupId>com.evil</groupId><artifactId>left-pad</artifactId><version>1.0</version></dependency>\n  </dependencies>",
    );
    let v = profile
        .dependency_violations("pom.xml", Some(&pom), &added, &AllowList::default())
        .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].dependency.name, "com.evil:left-pad");

    let repo = pom.replace(
        "  <dependencies>",
        "  <repositories><repository><id>x</id><url>https://evil.example/m2</url></repository></repositories>\n  <dependencies>",
    );
    let v = profile
        .dependency_violations("pom.xml", Some(&pom), &repo, &AllowList::default())
        .unwrap();
    assert_eq!(v.len(), 2, "every coordinate now comes from another source");
    assert!(v.iter().all(|v| v.kind == ViolationKind::UnofficialSource));
}

// "Xong khi" of #80: a patch bumping an allowed dependency that pulls in an
// unknown transitive artifact is blocked, through the resolved tree.
#[test]
fn a_bump_pulling_in_an_unknown_transitive_artifact_is_blocked() {
    let profile = java();
    let tree = profile.tree.as_ref().unwrap();
    assert!(tree.on_change.iter().any(|p| p == "pom.xml"));
    let before = "example:fixture-java:jar:0.1.0
\\- org.junit.jupiter:junit-jupiter:jar:5.11.4:test
   +- org.junit.jupiter:junit-jupiter-api:jar:5.11.4:test
   |  \\- org.opentest4j:opentest4j:jar:1.3.0:test
   \\- org.junit.jupiter:junit-jupiter-engine:jar:5.11.4:test
";
    let after = before.replace("5.11.4", "5.12.0").replace(
        "   |  \\- org.opentest4j:opentest4j:jar:1.3.0:test\n",
        "   |  +- org.opentest4j:opentest4j:jar:1.3.0:test\n   |  \\- com.evil:telemetry:jar:0.1:test\n",
    );
    let allowed = AllowList::from_toml("allowed = [\"org.junit.jupiter:junit-jupiter\"]").unwrap();
    let check = |after: &str| {
        profile
            .profile
            .dependency_violations(&tree.file, Some(before), after, &allowed)
            .unwrap()
    };
    assert!(
        check(&before.replace("5.11.4", "5.12.0")).is_empty(),
        "a plain bump passes"
    );
    let v = check(&after);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].dependency.name, "com.evil:telemetry");
    assert_eq!(v[0].kind, ViolationKind::NotAllowed);
}

/// The profile's tree command on the fixture, with Maven itself.
#[test]
fn maven_writes_a_tree_the_reader_reads() {
    if !tools() {
        return;
    }
    let profile = java();
    let tree = profile.tree.as_ref().unwrap();
    let ws = Workspace::new("java");
    let output = ws.repo().with_file_name(&tree.file);
    let output = output.to_str().unwrap();
    ws.run_ok(
        "tree",
        &tree
            .command
            .render(&[(Placeholder::Output, output)])
            .unwrap(),
    );
    let deps = read_lockfile(
        Language::Java,
        &tree.file,
        &std::fs::read_to_string(output).unwrap(),
    )
    .unwrap();
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"org.junit.jupiter:junit-jupiter-api"),
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.starts_with("example:")));
}
