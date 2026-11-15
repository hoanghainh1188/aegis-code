//! PHP profile tests (#37): detection, ACI syntax/symbols, lockfile policy,
//! and conventions.

mod common;

use std::path::Path;

use aegis_lang::{
    AllowList, Language, ProfileOverrides, Profiles, ResolvedProfile, SymbolKind, read_lockfile,
    symbols, syntax_errors,
};
use common::fixture;

fn php() -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(Language::Php)
        .unwrap()
        .resolve(&ProfileOverrides::default())
        .unwrap()
}

#[test]
fn detects_the_fixture_as_php() {
    let entries: Vec<String> = std::fs::read_dir(fixture("php"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let profiles = Profiles::builtin().unwrap();
    assert_eq!(
        profiles.detect(entries.iter().map(String::as_str)),
        vec![Language::Php]
    );
    assert_eq!(
        profiles.language_of(Path::new("src/Cart.php")),
        Some(Language::Php)
    );
    assert!(php().profile.conventions().is_some());
}

#[test]
fn aci_checks_syntax_and_finds_php_symbols() {
    let source = std::fs::read_to_string(fixture("php").join("src/Cart.php")).unwrap();
    assert!(syntax_errors(Language::Php, &source).unwrap().is_empty());
    assert!(
        !syntax_errors(
            Language::Php,
            "<?php\nclass Cart { public function add( }\n"
        )
        .unwrap()
        .is_empty()
    );

    let all = symbols(Language::Php, &source).unwrap();
    let has = |name: &str, kind: SymbolKind| all.iter().any(|s| s.name == name && s.kind == kind);
    assert!(has(r"Example\Shop", SymbolKind::Module));
    assert!(has("Cart", SymbolKind::Class));
    assert!(has("prices", SymbolKind::Variable));
    assert!(has("add", SymbolKind::Method));
    assert!(has("total", SymbolKind::Method));
}

#[test]
fn adding_an_unknown_composer_package_is_blocked() {
    let profile = php().profile;
    assert!(profile.touches_dependencies(&["composer.json", "composer.lock"]));
    let lock = std::fs::read_to_string(fixture("php").join("composer.lock")).unwrap();
    let deps = read_lockfile(Language::Php, "composer.lock", &lock).unwrap();
    assert!(deps.iter().any(|d| d.name == "symfony/console"));
    assert!(deps.iter().any(|d| d.name == "phpunit/phpunit"));
    assert!(
        deps.iter()
            .all(|d| d.source == profile.dependencies.official_source)
    );

    let mut value: serde_json::Value = serde_json::from_str(&lock).unwrap();
    value["packages"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "evil/backdoor",
            "version": "1.0.0",
            "notification-url": "https://packagist.org/downloads/"
        }));
    let after = serde_json::to_string(&value).unwrap();
    let v = profile
        .dependency_violations("composer.lock", Some(&lock), &after, &AllowList::default())
        .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].dependency.name, "evil/backdoor");
}
