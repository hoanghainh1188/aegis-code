use std::process::Command;

use aegis_lang::{Language, Profiles};

use super::*;

fn target(package: &str, version: &str) -> CveTarget {
    CveTarget {
        package: package.into(),
        version: version.into(),
        advisory: "RUSTSEC-2024-0001".into(),
    }
}

#[test]
fn a_target_is_plain_names_only() {
    for package in [
        "serde",
        "typing_extensions",
        "@scope/name",
        "golang.org/x/net",
        "org.example:lib",
        "Newtonsoft.Json",
    ] {
        target(package, "1.2.3").check().unwrap();
    }
    for version in ["1.0.0", "2.32.4", "1.0.0-rc.1", "1.2.3+build", "v0.23.0"] {
        target("x", version).check().unwrap();
    }
    for advisory in [
        "GHSA-abcd-efgh-ijkl",
        "CVE-2024-1234",
        "PYSEC-2024-1",
        "GO-2024-0001",
    ] {
        CveTarget {
            advisory: advisory.into(),
            ..target("x", "1")
        }
        .check()
        .unwrap();
    }
    for package in [
        "",
        "-p",
        "--config=x",
        "a b",
        "a;rm",
        "a$(x)",
        "a`b`",
        "a\nb",
    ] {
        let err = target(package, "1").check().unwrap_err();
        assert!(err.contains("package"), "{package:?}: {err}");
    }
    for version in ["", "--locked", "1 2", "1;2", ">=1"] {
        assert!(target("x", version).check().is_err(), "{version:?}");
    }
    let bad = CveTarget {
        advisory: "../etc".into(),
        ..target("x", "1")
    };
    assert!(bad.check().unwrap_err().contains("advisory"));
    assert_eq!(
        target("serde", "1.0.1").url(),
        "https://osv.dev/vulnerability/RUSTSEC-2024-0001"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const LOCK: &str = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n";

fn crate_entry(version: &str) -> String {
    format!(
        "\n[[package]]\nname = \"left-pad\"\nversion = \"{version}\"\n\
         source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
         checksum = \"{}\"\n",
        "0".repeat(64)
    )
}

/// A Rust repo on `main` locking `left-pad` 1.0.0.
fn repo(name: &str) -> (std::path::PathBuf, ProfilePart) {
    let dir = std::env::temp_dir().join(format!("aegis-cve-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(
        dir.join("Cargo.lock"),
        format!("{LOCK}{}", crate_entry("1.0.0")),
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "base"]);
    (dir, rust_part())
}

/// The repo root under the built-in Rust profile.
fn rust_part() -> ProfilePart {
    let profile = Profiles::builtin()
        .unwrap()
        .get(Language::Rust)
        .unwrap()
        .resolve(&Default::default())
        .unwrap();
    ProfilePart {
        dir: String::new(),
        profile,
    }
}

#[test]
fn the_upgrade_is_its_manifests_and_lockfiles() {
    let (dir, part) = repo("collect");
    let t = target("left_pad", "1.0.1");
    // Nothing changed yet.
    let none = collect(&part, &dir, &t).unwrap().unwrap_err();
    assert!(none.contains("changed nothing"), "{none}");

    let upgraded = format!("{LOCK}{}", crate_entry("1.0.1"));
    std::fs::write(dir.join("Cargo.lock"), &upgraded).unwrap();
    // Build output is ignored, not part of the upgrade.
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join("target/x"), "").unwrap();
    let upgrade = collect(&part, &dir, &t).unwrap().unwrap();
    assert_eq!(upgrade.package, "left_pad");
    assert_eq!(upgrade.version, "1.0.1");
    assert_eq!(
        upgrade.files,
        BTreeMap::from([("Cargo.lock".to_owned(), upgraded.clone())])
    );

    // The job's version is not what the tool locked.
    let other = collect(&part, &dir, &target("left-pad", "1.0.2"))
        .unwrap()
        .unwrap_err();
    assert!(
        other.contains("no lockfile locks `left-pad` at 1.0.2"),
        "{other}"
    );

    // A tool that also touched the code.
    std::fs::write(dir.join("src/lib.rs"), "// touched\n").unwrap();
    let beyond = collect(&part, &dir, &t).unwrap().unwrap_err();
    assert!(
        beyond.contains("beyond the manifests and lockfiles: src/lib.rs"),
        "{beyond}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_first_answer_and_message_describe_the_upgrade() {
    let t = target("left-pad", "1.0.1");
    let upgrade = Upgrade {
        package: t.package.clone(),
        version: t.version.clone(),
        files: BTreeMap::from([
            ("Cargo.lock".to_owned(), String::new()),
            ("Cargo.toml".to_owned(), String::new()),
        ]),
    };
    let a = answer(&t, &upgrade);
    assert_eq!(a.status, Status::Fixed);
    assert!(a.repro.is_none());
    assert!(
        a.cause
            .starts_with("RUSTSEC-2024-0001: `left-pad` is raised to 1.0.1"),
        "{}",
        a.cause
    );
    let locations: Vec<&str> = a.changes.iter().map(|c| c.location.as_str()).collect();
    assert_eq!(locations, ["Cargo.lock", "Cargo.toml"]);

    let m = first_message(&t, &upgrade);
    for want in [
        "RUSTSEC-2024-0001",
        "`left-pad` to 1.0.1",
        "in Cargo.lock, Cargo.toml",
        "Do not edit Cargo.lock, Cargo.toml",
        "No reproduction test is needed",
    ] {
        assert!(m.contains(want), "{want} not in {m}");
    }
}

/// `cargo` itself raises a crate from crates.io (#501). Needs the network;
/// skipped without it, unless `AEGIS_REQUIRE_NETWORK=1`.
#[tokio::test(flavor = "multi_thread")]
async fn cargo_raises_a_real_crate() {
    let dir = std::env::temp_dir().join(format!("aegis-cve-cargo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nitoa = \"1\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
    let cargo = |args: &[&str]| {
        Command::new("cargo")
            .args(args)
            .current_dir(&dir)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !(cargo(&["generate-lockfile"])
        && cargo(&["update", "--package", "itoa", "--precise", "1.0.10"]))
    {
        assert!(
            std::env::var("AEGIS_REQUIRE_NETWORK").as_deref() != Ok("1"),
            "cargo could not reach crates.io"
        );
        eprintln!("skipping: cargo could not reach crates.io");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "base"]);

    let poc = include_str!("../../../../config/poc.toml");
    let mut cfg = RunConfig::from_toml(poc).unwrap();
    cfg.run.sandbox = false;
    let part = rust_part();
    let job_dir = dir.with_extension("job");
    std::fs::create_dir_all(&job_dir).unwrap();
    let target = CveTarget {
        package: "itoa".into(),
        version: "1.0.11".into(),
        advisory: "RUSTSEC-2099-0001".into(),
    };
    let upgrade = upgrade(
        &cfg,
        &part,
        &dir,
        &job_dir,
        &target,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let lock = &upgrade.files["Cargo.lock"];
    assert!(
        lock.contains("name = \"itoa\"\nversion = \"1.0.11\""),
        "{lock}"
    );
    assert_eq!(upgrade.files.len(), 1, "{:?}", upgrade.files.keys());
    // A version crates.io does not have.
    let none = upgrade_of(&cfg, &part, &dir, &job_dir, "itoa", "0.0.999").await;
    assert!(none.contains("raising `itoa` to 0.0.999 failed"), "{none}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&job_dir);
}

async fn upgrade_of(
    cfg: &RunConfig,
    part: &ProfilePart,
    dir: &Path,
    job_dir: &Path,
    package: &str,
    version: &str,
) -> String {
    git(dir, &["checkout", "--", "."]);
    let target = CveTarget {
        package: package.into(),
        version: version.into(),
        advisory: "X-1".into(),
    };
    upgrade(cfg, part, dir, job_dir, &target, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap_err()
}
