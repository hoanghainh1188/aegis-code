//! #586: ACI and gate work on the Kotlin fixture repo through the profile
//! alone, and a patch touching the build or adding a dependency is
//! blocked. Runs the profile's fetch step (Gradle, needs network) and then
//! the offline gate commands, with the sandbox image's init script given
//! by `-I` as the image's GRADLE_USER_HOME/init.d gives it there; skipped
//! when `java` / `gradle` are missing unless `AEGIS_REQUIRE_PROFILE_TOOLS=1`
//! (CI).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use aegis_lang::{
    AllowList, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile, SkipReason,
    Step, SymbolKind, read_lockfile, symbols, syntax_errors,
};
use common::{Workspace, fixture, tools_available};

fn kotlin() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::Kotlin)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

fn tools() -> bool {
    tools_available(&[&["java", "-version"], &["gradle", "--version"]])
}

fn image_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docker/kotlin")
        .join(name)
}

/// `argv` with the image's init script, and no daemon left behind.
fn in_image(mut argv: Vec<String>) -> Vec<String> {
    let init = image_file("aegis.init.gradle");
    argv.extend([
        "-I".to_owned(),
        init.to_str().unwrap().to_owned(),
        "--no-daemon".to_owned(),
    ]);
    argv
}

fn plain(command: &aegis_lang::CommandTemplate) -> Vec<String> {
    in_image(command.render(&[]).unwrap())
}

fn fetched(profile: &ResolvedProfile) -> Workspace {
    let ws = Workspace::new("kotlin");
    ws.run_ok("fetch", &plain(profile.fetch.command().unwrap()));
    ws
}

/// Tests the JUnit XML report of `class` counts.
fn tests_run(ws: &Workspace, class: &str) -> String {
    let xml = std::fs::read_to_string(
        ws.repo()
            .join(format!("build/test-results/test/TEST-{class}.xml")),
    )
    .unwrap();
    let start = xml.find("tests=\"").unwrap() + "tests=\"".len();
    xml[start..].split('"').next().unwrap().to_owned()
}

#[test]
fn detects_the_fixture_as_kotlin() {
    let entries: Vec<String> = std::fs::read_dir(fixture("kotlin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    // A Gradle build is Java's too: detection only suggests.
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::Java, Language::Kotlin]
    );
    for path in ["src/main/kotlin/example/cart/Cart.kt", "build.gradle.kts"] {
        assert_eq!(
            profiles.language_of(Path::new(path)),
            Some(Language::Kotlin),
            "{path}"
        );
    }
}

#[test]
fn profile_invents_no_formatter() {
    let profile = kotlin();
    assert_eq!(profile.gate.format, Step::Skipped(SkipReason::NotInProfile));
    assert_eq!(profile.edit_format, Step::Skipped(SkipReason::NotInProfile));
}

#[test]
fn gate_steps_pass_offline_after_fetch() {
    if !tools() {
        return;
    }
    let profile = kotlin();
    let ws = fetched(&profile);
    let gate = &profile.gate;
    ws.run_ok("lint", &plain(gate.lint.command().unwrap()));
    ws.run_ok("test", &plain(&gate.test));
    assert_eq!(tests_run(&ws, "example.cart.CartTest"), "2");

    let repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(Placeholder::Test, "CartTest.addsUpPrices")])
        .unwrap();
    ws.run_ok("repro_test", &in_image(repro));
    assert_eq!(tests_run(&ws, "example.cart.CartTest"), "1");

    ws.run_ok("coverage", &plain(gate.coverage.command().unwrap()));
    let report = profile.coverage.as_ref().unwrap();
    let xml = std::fs::read_to_string(ws.repo().join(&report.path)).unwrap();
    assert!(xml.contains("<report"), "JaCoCo XML expected");
}

/// #648: in a multi-project build `repro_test` runs every project's test
/// task with the filter. The projects without the class pass, and an id
/// that selects no test still passes the runner but is seen as running
/// nothing (#235).
#[test]
fn repro_test_runs_one_class_of_a_multi_project_build() {
    if !tools() {
        return;
    }
    let profile = kotlin();
    let ws = Workspace::new("kotlin-multi");
    ws.run_ok("fetch", &plain(profile.fetch.command().unwrap()));
    let repro = |id: &str| {
        let argv = profile
            .gate
            .repro_test
            .command()
            .unwrap()
            .render(&[(Placeholder::Test, id)])
            .unwrap();
        in_image(argv)
    };

    let out = ws.run_ok("repro_test", &repro("example.cart.CartTest"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("aegis-gradle RAN example.cart.CartTest#addsUpPrices()"),
        "{stdout}"
    );
    assert!(!stdout.contains("TaxTest"), "{stdout}");
    assert!(!profile.gate.repro_ran_nothing(&stdout), "{stdout}");

    let out = ws.run_ok("repro_test", &repro("example.cart.NoSuchTest"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(profile.gate.repro_ran_nothing(&stdout), "{stdout}");

    // Without `--tests` the full suite names no test that passes.
    let out = ws.run_ok("test", &plain(&profile.gate.test));
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("aegis-gradle RAN"),
        "only a filtered build names its tests"
    );
}

/// The init script names a failing test with its package, the line the
/// gate's `gradle` test output reads, even under `-q`.
#[test]
fn a_failing_test_is_named_with_its_package() {
    if !tools() {
        return;
    }
    let profile = kotlin();
    let ws = fetched(&profile);
    let file = ws.repo().join("src/test/kotlin/example/cart/CartTest.kt");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace(
            "assertEquals(3, Cart.of(1, 2)",
            "assertEquals(4, Cart.of(1, 2)",
        ),
    )
    .unwrap();
    let out = ws.run(&plain(&profile.gate.test));
    assert!(!out.status.success(), "a failing test must fail the step");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("aegis-gradle FAILED example.cart.CartTest#addsUpPrices()"),
        "{stdout}"
    );
    assert!(!stdout.contains("refusesAFourthItem"), "{stdout}");
}

#[test]
fn lint_fails_on_code_that_does_not_compile() {
    if !tools() {
        return;
    }
    let profile = kotlin();
    let ws = fetched(&profile);
    let file = ws.repo().join("src/main/kotlin/example/cart/Cart.kt");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("items += price", "items += \"price\"")).unwrap();
    let out = ws.run(&plain(profile.gate.lint.command().unwrap()));
    assert!(!out.status.success(), "type error must fail");
}

#[test]
fn symbols_of_the_fixture() {
    let source =
        std::fs::read_to_string(fixture("kotlin").join("src/main/kotlin/example/cart/Cart.kt"))
            .unwrap();
    assert!(syntax_errors(Language::Kotlin, &source).unwrap().is_empty());
    let all = symbols(Language::Kotlin, &source).unwrap();
    let get = |name: &str, kind: SymbolKind| {
        all.iter()
            .find(|s| s.name == name && s.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} `{name}` in {all:#?}"))
    };
    get("Priced", SymbolKind::Interface);
    get("Cart", SymbolKind::Class);
    assert_eq!(
        get("MAX_ITEMS", SymbolKind::Constant).container.as_deref(),
        Some("Cart")
    );
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
fn a_patch_to_the_build_or_a_locked_dependency_is_caught() {
    let profile = kotlin().profile;
    for file in [
        "build.gradle.kts",
        "gradle/libs.versions.toml",
        "gradle.lockfile",
    ] {
        assert!(profile.touches_dependencies(&[file]), "{file}");
    }
    let protected = profile.protected_changes(&[
        "build.gradle.kts",
        "core/build.gradle.kts",
        "gradle/wrapper/gradle-wrapper.properties",
        "buildSrc/src/main/kotlin/Conventions.kt",
        "gradlew",
        "src/main/kotlin/example/cart/Cart.kt",
    ]);
    assert_eq!(
        protected,
        [
            "build.gradle.kts",
            "core/build.gradle.kts",
            "gradle/wrapper/gradle-wrapper.properties",
            "buildSrc/src/main/kotlin/Conventions.kt",
            "gradlew",
        ]
    );

    let lock = "\
# This is a Gradle generated file for dependency locking.
org.jetbrains.kotlin:kotlin-stdlib:2.4.21=compileClasspath,runtimeClasspath
org.junit.jupiter:junit-jupiter-api:5.13.4=testCompileClasspath
empty=
";
    let deps = read_lockfile(Language::Kotlin, "gradle.lockfile", lock).unwrap();
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "org.jetbrains.kotlin:kotlin-stdlib",
            "org.junit.jupiter:junit-jupiter-api"
        ]
    );
    let added = format!("{lock}com.evil:left-pad:1.0=runtimeClasspath\n");
    let v = profile
        .dependency_violations("gradle.lockfile", Some(lock), &added, &AllowList::default())
        .unwrap();
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].dependency.name, "com.evil:left-pad");
}

/// The profile's tree command on the fixture, with Gradle itself.
#[test]
fn gradle_writes_a_tree_the_reader_reads() {
    if !tools() {
        return;
    }
    let profile = kotlin();
    let tree = profile.tree.as_ref().unwrap();
    let ws = Workspace::new("kotlin");
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
        Language::Kotlin,
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

/// The profile's upgrade (#501) sets the version in the catalog, through a
/// `version.ref` or on the library, and fails for a dependency no catalog
/// sets.
#[test]
fn upgrade_sets_the_version_in_the_catalog() {
    let profile = kotlin();
    let upgrade = profile.upgrade.command().unwrap();
    assert_eq!(upgrade.render(&[]).ok(), None, "needs its placeholders");
    let ws = Workspace::new("kotlin");
    let catalog = ws.repo().join("gradle/libs.versions.toml");
    std::fs::create_dir_all(catalog.parent().unwrap()).unwrap();
    std::fs::write(
        &catalog,
        r#"[versions]
jackson = "2.17.0"

[libraries]
jackson-databind = { module = "com.fasterxml.jackson.core:jackson-databind", version.ref = "jackson" }
guava = { group = "com.google.guava", name = "guava", version = "32.0.0-jre" }
okio = "com.squareup.okio:okio:3.0.0"
"#,
    )
    .unwrap();
    let script = image_file("aegis-gradle-upgrade");
    let run = |package: &str, version: &str| {
        let mut argv = upgrade
            .render(&[
                (Placeholder::Package, package),
                (Placeholder::Version, version),
            ])
            .unwrap();
        assert_eq!(argv[0], "aegis-gradle-upgrade");
        argv[0] = script.to_str().unwrap().to_owned();
        Command::new("sh")
            .args(&argv)
            .current_dir(ws.repo())
            .output()
            .unwrap()
    };
    for (package, version) in [
        ("com.fasterxml.jackson.core:jackson-databind", "2.18.2"),
        ("com.google.guava:guava", "33.4.0-jre"),
        ("com.squareup.okio:okio", "3.9.1"),
    ] {
        let out = run(package, version);
        assert!(
            out.status.success(),
            "{package}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let text = std::fs::read_to_string(&catalog).unwrap();
    assert!(text.contains("jackson = \"2.18.2\""), "{text}");
    assert!(text.contains("version = \"33.4.0-jre\""), "{text}");
    assert!(text.contains("\"com.squareup.okio:okio:3.9.1\""), "{text}");
    assert!(!run("com.example:nowhere", "1.0").status.success());
    assert!(!run("bad;rm -rf /", "1.0").status.success());
}
