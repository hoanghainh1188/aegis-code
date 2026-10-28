//! "Xong khi" of #70: ACI and gate work on the C# fixture repo through the
//! profile alone, and a patch adding an unknown package or feed is blocked.
//! Runs the profile's fetch step (`dotnet restore`, needs network) and then
//! the offline gate commands; skipped when `dotnet` is missing unless
//! `AEGIS_REQUIRE_PROFILE_TOOLS=1` (CI).

mod common;

use std::path::{Path, PathBuf};

use aegis_lang::{
    AllowList, Language, Placeholder, ProfileOverrides, Profiles, ResolvedProfile, SymbolKind,
    ViolationKind, read_lockfile, symbols, syntax_errors,
};
use common::{Workspace, fixture, tools_available};

fn csharp() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::CSharp)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

fn tools() -> bool {
    tools_available(&[&["dotnet", "--version"]])
}

fn fetched(profile: &ResolvedProfile) -> Workspace {
    fetched_fixture(profile, "csharp")
}

fn fetched_fixture(profile: &ResolvedProfile, fixture: &str) -> Workspace {
    let ws = Workspace::new(fixture);
    ws.run_ok(
        "fetch",
        &profile.fetch.command().unwrap().render(&[]).unwrap(),
    );
    ws
}

/// The one file matching a coverage path pattern.
fn find_report(root: &Path, pattern: &str) -> Option<PathBuf> {
    aegis_lang::find_reports(root, pattern).into_iter().next()
}

#[test]
fn detects_the_fixture_as_csharp() {
    let entries: Vec<String> = std::fs::read_dir(fixture("csharp"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::CSharp]
    );
    assert_eq!(
        profiles.language_of(Path::new("src/Cart/Cart.cs")),
        Some(Language::CSharp)
    );
}

#[test]
fn gate_steps_pass_offline_after_fetch() {
    if !tools() {
        return;
    }
    let profile = csharp();
    let ws = fetched(&profile);
    let gate = &profile.gate;
    let plain = |c: &aegis_lang::CommandTemplate| c.render(&[]).unwrap();
    ws.run_ok("format", &plain(gate.format.command().unwrap()));
    ws.run_ok("lint", &plain(gate.lint.command().unwrap()));
    ws.run_ok("test", &plain(&gate.test));

    let repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(
            Placeholder::Test,
            "FullyQualifiedName~CartTests.AddsUpPrices",
        )])
        .unwrap();
    let out = ws.run_ok("repro_test", &repro);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let total = stdout
        .split("Total:")
        .nth(1)
        .map(|t| t.trim_start().chars().next());
    assert_eq!(total, Some(Some('1')), "one test: {stdout}");

    ws.run_ok("coverage", &plain(gate.coverage.command().unwrap()));
    let pattern = &profile.coverage.as_ref().unwrap().path;
    let report = find_report(&ws.repo(), pattern).expect("OpenCover report");
    let xml = std::fs::read_to_string(report).unwrap();
    assert!(xml.contains("<CoverageSession"), "OpenCover XML expected");
}

/// #228: under Microsoft.Testing.Platform the profile's test steps run but
/// `--collect` does not, so its coverage is skipped there and only there.
#[test]
fn testing_platform_runs_the_tests_and_skips_the_coverage() {
    let profile = csharp();
    let skipped = |ws: &Workspace| {
        let repo = ws.repo();
        profile
            .gate
            .coverage_skip
            .iter()
            .any(|m| m.matches(&repo, &repo))
    };
    assert!(!skipped(&Workspace::new("csharp")));
    assert!(skipped(&Workspace::new("csharp-mtp")));
    if !tools() {
        return;
    }
    let ws = fetched_fixture(&profile, "csharp-mtp");
    let gate = &profile.gate;
    let plain = |c: &aegis_lang::CommandTemplate| c.render(&[]).unwrap();
    ws.run_ok("format", &plain(gate.format.command().unwrap()));
    ws.run_ok("lint", &plain(gate.lint.command().unwrap()));
    ws.run_ok("test", &plain(&gate.test));
    let repro = gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(
            Placeholder::Test,
            "FullyQualifiedName~CartTests.AddsUpPrices",
        )])
        .unwrap();
    let out = ws.run_ok("repro_test", &repro);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("total: 1"), "one test: {stdout}");
    // What the skip is for: VSTest's `--collect` is an unknown option here.
    let coverage = ws.run(&plain(gate.coverage.command().unwrap()));
    assert!(!coverage.status.success());
}

#[test]
fn format_and_lint_fail_on_bad_code() {
    if !tools() {
        return;
    }
    let profile = csharp();
    let ws = fetched(&profile);
    let file = ws.repo().join("src/Cart/Cart.cs");
    let good = std::fs::read_to_string(&file).unwrap();
    let fails = |step: &str, c: &aegis_lang::CommandTemplate| {
        let out = ws.run(&c.render(&[]).unwrap());
        assert!(!out.status.success(), "{step} must fail");
    };

    std::fs::write(
        &file,
        good.replace(
            "public const int MaxItems = 3;",
            "public   const int MaxItems=3;",
        ),
    )
    .unwrap();
    fails("format", profile.gate.format.command().unwrap());

    // CS0219: assigned but never used, an error with -warnaserror.
    let unused = good.replace(
        "var cart = new Cart();",
        "var cart = new Cart();\n        int unused = 1;",
    );
    std::fs::write(&file, unused).unwrap();
    fails("lint", profile.gate.lint.command().unwrap());
}

#[test]
fn edit_format_formats_only_the_edited_file() {
    if !tools() {
        return;
    }
    let profile = csharp();
    let ws = Workspace::new("csharp");
    let messy = "namespace Example.Cart;\npublic static class Extra {\n        public static int   Twice(int x)  =>   x * 2;\n}\n";
    std::fs::write(ws.repo().join("src/Cart/Extra.cs"), messy).unwrap();
    let before = std::fs::read_to_string(ws.repo().join("src/Cart/Cart.cs")).unwrap();

    assert!(syntax_errors(Language::CSharp, messy).unwrap().is_empty());
    let argv = profile
        .edit_format
        .command()
        .unwrap()
        .render(&[(Placeholder::File, "src/Cart/Extra.cs")])
        .unwrap();
    ws.run_ok("edit.format", &argv);

    let formatted = std::fs::read_to_string(ws.repo().join("src/Cart/Extra.cs")).unwrap();
    assert_ne!(formatted, messy, "whitespace was fixed");
    assert!(
        formatted.contains("\n    public static int Twice(int x) => x * 2;\n"),
        "{formatted}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.repo().join("src/Cart/Cart.cs")).unwrap(),
        before
    );
}

#[test]
fn symbols_of_the_fixture() {
    let source = std::fs::read_to_string(fixture("csharp").join("src/Cart/Cart.cs")).unwrap();
    let all = symbols(Language::CSharp, &source).unwrap();
    let get = |name: &str, kind: SymbolKind| {
        all.iter()
            .find(|s| s.name == name && s.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} `{name}` in {all:#?}"))
    };
    get("Example.Cart", SymbolKind::Module);
    get("IPriced", SymbolKind::Interface);
    get("Color", SymbolKind::Enum);
    get("Point", SymbolKind::Class);
    get("Money", SymbolKind::Struct);
    get("CartChanged", SymbolKind::TypeAlias);
    get("MaxItems", SymbolKind::Variable);
    assert_eq!(
        get("Add", SymbolKind::Method).container.as_deref(),
        Some("Cart")
    );
    assert_eq!(
        get("Dollars", SymbolKind::Method).container.as_deref(),
        Some("Money")
    );
}

#[test]
fn adding_a_package_or_feed_is_blocked() {
    let profile = csharp().profile;
    assert!(profile.touches_dependencies(&["src/Cart/Cart.csproj", "nuget.config"]));
    let lock =
        std::fs::read_to_string(fixture("csharp").join("tests/Cart.Tests/packages.lock.json"))
            .unwrap();
    let deps = read_lockfile(Language::CSharp, "packages.lock.json", &lock).unwrap();
    assert!(deps.iter().any(|d| d.name == "xunit"));
    assert!(
        deps.iter()
            .all(|d| d.source == profile.dependencies.official_source)
    );

    let mut value: serde_json::Value = serde_json::from_str(&lock).unwrap();
    value["dependencies"]["net10.0"]["Left.Pad"] =
        serde_json::json!({"type": "Direct", "resolved": "1.0.0", "contentHash": "x"});
    let after = serde_json::to_string(&value).unwrap();
    let v = profile
        .dependency_violations(
            "packages.lock.json",
            Some(&lock),
            &after,
            &AllowList::default(),
        )
        .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].dependency.name, "left.pad");

    let config = "<configuration><packageSources><add key=\"evil\" value=\"https://evil.example/v3/index.json\" /></packageSources></configuration>";
    let v = profile
        .dependency_violations("nuget.config", None, config, &AllowList::default())
        .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].kind, ViolationKind::UnofficialSource);
}
