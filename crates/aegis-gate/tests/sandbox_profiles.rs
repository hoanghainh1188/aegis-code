//! #210: the gate of the TypeScript, Java, Go and C# profiles on their
//! fixture repos (C# also under Microsoft.Testing.Platform, #228), inside
//! the profile's image with no network, and the reproduction-test command
//! `run_tests` runs there too; and, for Go and Java, a test the base
//! revision already fails is told apart from one the patch breaks (#291;
//! C# and TypeScript need the base copy's dependencies, #303). Dependencies are fetched
//! first in a
//! container of the same image that has the network, into the caches the
//! gate then mounts read-only, as the lifecycle does (#66). Needs Docker,
//! the network and the image built from `docker/<image>` (`docker build -t
//! aegis-sandbox-node docker/node`); skipped otherwise, unless
//! `AEGIS_REQUIRE_DOCKER=1`.

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use aegis_gate::{CancelToken, GateReport, Sandbox, Skip, StepName, StepStatus};
use aegis_lang::{Language, Placeholder, ResolvedProfile};
use common::{Repo, profile, statuses};

fn docker_ready(image: &str) -> bool {
    let ok = Command::new("docker")
        .args(["image", "inspect", image])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or the {image} image is missing"
        );
        eprintln!("skipping: docker or the {image} image is missing");
    }
    ok
}

fn host_user() -> String {
    let id = |flag: &str| {
        let out = Command::new("id").arg(flag).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    format!("{}:{}", id("-u"), id("-g"))
}

/// The profile's cache directories outside the tree, each on the host
/// under the repo's work dir, as the lifecycle keeps them per repo.
fn cache_mounts(repo: &Repo, profile: &ResolvedProfile) -> Vec<(PathBuf, String)> {
    profile
        .profile
        .sandbox
        .cache_dirs
        .iter()
        .filter(|d| d.starts_with('/'))
        .map(|dir| {
            let host = repo
                .work()
                .join("cache")
                .join(dir.trim_start_matches('/').replace('/', "_"));
            std::fs::create_dir_all(&host).unwrap();
            (host, dir.clone())
        })
        .collect()
}

/// Runs `argv` in a container of `image` as the host user, the tree at
/// `/work` and the caches mounted read-write, with or without the network.
fn docker_run(
    image: &str,
    repo: &Repo,
    mounts: &[(PathBuf, String)],
    network: bool,
    argv: &[String],
) -> std::process::Output {
    let mut cmd = Command::new("docker");
    cmd.args(["run", "--rm", "--user", &host_user(), "--workdir", "/work"])
        .arg("--volume")
        .arg(format!("{}:/work", repo.dir().display()));
    for (host, dir) in mounts {
        cmd.arg("--volume").arg(format!("{}:{dir}", host.display()));
    }
    if !network {
        cmd.args(["--network", "none"]);
    }
    let out = cmd.arg(image).args(argv).output().unwrap();
    assert!(
        out.status.success(),
        "{argv:?} in {image}:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The profile's fetch in a container with the network, into the cache
/// mounts it returns.
fn fetched(repo: &Repo, profile: &ResolvedProfile) -> Vec<(PathBuf, String)> {
    let mounts = cache_mounts(repo, profile);
    let fetch = profile.fetch.command().unwrap().render(&[]).unwrap();
    docker_run(&profile.image, repo, &mounts, true, &fetch);
    mounts
}

/// The gate on `repo` inside the profile's image, with no network.
fn gate_in_container(
    repo: &Repo,
    profile: &ResolvedProfile,
    mounts: &[(PathBuf, String)],
) -> GateReport {
    let sandbox = Sandbox {
        docker: "docker".into(),
        image: profile.image.clone(),
        mounts: mounts.to_vec(),
        env: vec![],
        network: None,
        read_only_mounts: true,
    };
    repo.gate_in(
        profile,
        None,
        Duration::from_secs(900),
        CancelToken::new(),
        vec![],
        Some(sandbox),
    )
}

/// The gate of `language` on its fixture inside `image`; `ignored`: what
/// the fetch and the steps write into the tree, which the fixture's own
/// `.gitignore` lists but the test repo's does not. Then the profile's
/// `repro_test` with `test`, as `run_tests` runs it. `overrides`: as a
/// RepoConfig would. `coverage`: the coverage step's status.
fn gate_passes(
    language: Language,
    overrides: &str,
    fixture: &str,
    ignored: &str,
    test: &str,
    coverage: StepStatus,
) {
    let profile = profile(language, overrides);
    let image = profile.image.clone();
    if !docker_ready(&image) {
        return;
    }
    let repo = Repo::new(fixture, &[]);
    repo.write(".gitignore", ignored);
    repo.commit_base("ignore what the steps write");
    let mounts = fetched(&repo, &profile);
    let report = gate_in_container(&repo, &profile, &mounts);
    let has = |step: StepName| match step {
        StepName::Format => profile.gate.format.command().is_some(),
        StepName::Typecheck => profile.gate.typecheck.command().is_some(),
        _ => true,
    };
    let expected: Vec<_> = [
        StepName::Format,
        StepName::Lint,
        StepName::Typecheck,
        StepName::Test,
        StepName::Coverage,
    ]
    .into_iter()
    .map(|step| {
        let status = if step == StepName::Coverage {
            coverage.clone()
        } else if has(step) {
            StepStatus::Passed
        } else {
            StepStatus::Skipped(Skip::NotInProfile)
        };
        (step, status)
    })
    .collect();
    let ran: Vec<_> = statuses(&report)
        .into_iter()
        .filter(|(step, _)| expected.iter().any(|(s, _)| s == step))
        .collect();
    assert_eq!(ran, expected, "{report}");

    let repro = profile
        .gate
        .repro_test
        .command()
        .unwrap()
        .render(&[(Placeholder::Test, test)])
        .unwrap();
    let read_only: Vec<_> = mounts
        .iter()
        .map(|(host, dir)| (host.clone(), format!("{dir}:ro")))
        .collect();
    docker_run(&image, &repo, &read_only, false, &repro);
}

#[test]
fn the_typescript_gate_passes_inside_the_container() {
    gate_passes(
        Language::TypeScript,
        "",
        "typescript",
        "node_modules/\ncoverage/\n",
        "test/cart.test.ts",
        StepStatus::Passed,
    );
}

#[test]
fn the_java_gate_passes_inside_the_container() {
    gate_passes(
        Language::Java,
        "",
        "java",
        "target/\n",
        "CartTest#addsUpPrices",
        StepStatus::Passed,
    );
}

#[test]
fn the_go_gate_passes_inside_the_container() {
    gate_passes(
        Language::Go,
        "",
        "go",
        "coverage.out\n",
        "^TestAddsUpPrices$",
        StepStatus::Passed,
    );
}

#[test]
fn the_csharp_gate_passes_inside_the_container() {
    gate_passes(
        Language::CSharp,
        "",
        "csharp",
        "bin/\nobj/\nTestResults/\n",
        "FullyQualifiedName~CartTests.AddsUpPrices",
        StepStatus::Passed,
    );
}

/// Under Microsoft.Testing.Platform `dotnet test` takes no `--collect`: the
/// profile's coverage step is skipped and the rest passes (#228).
#[test]
fn the_csharp_gate_passes_under_the_testing_platform() {
    gate_passes(
        Language::CSharp,
        "",
        "csharp-mtp",
        "bin/\nobj/\nTestResults/\n",
        "FullyQualifiedName~CartTests.AddsUpPrices",
        StepStatus::Skipped(Skip::NotForRepo),
    );
}

/// An edit to a fixture file: `(file, from, to)`.
type Edit<'a> = (&'a str, &'a str, &'a str);

/// #291: on a base revision where `failing` makes a test fail, a patch that
/// leaves the tests alone (`harmless`) passes the test step; adding
/// `breaking` fails it, naming the newly broken test `broken` and only it.
fn base_failures_are_not_new(
    language: Language,
    fixture: &str,
    ignored: &str,
    failing: Edit,
    harmless: Edit,
    breaking: Edit,
    broken: &str,
) {
    let profile = profile(language, "");
    if !docker_ready(&profile.image) {
        return;
    }
    let repo = Repo::new(fixture, &[failing]);
    repo.write(".gitignore", ignored);
    repo.commit_base("ignore what the steps write");
    let mounts = fetched(&repo, &profile);

    repo.replace(harmless.0, harmless.1, harmless.2);
    let report = gate_in_container(&repo, &profile, &mounts);
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Passed, "{report}");
    assert!(
        test.detail
            .starts_with("1 tests fail, as on the base revision"),
        "{}",
        test.detail
    );

    repo.replace(breaking.0, breaking.1, breaking.2);
    let report = gate_in_container(&repo, &profile, &mounts);
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Failed, "{report}");
    let new = test.detail.split("\n\n").next().unwrap_or_default();
    assert!(
        new.starts_with("1 tests fail that pass on the base revision:") && new.contains(broken),
        "{}",
        test.detail
    );
}

#[test]
fn the_go_gate_tells_base_failures_from_new_ones() {
    base_failures_are_not_new(
        Language::Go,
        "go",
        "coverage.out\n",
        ("cart_test.go", "c.Total() != 3", "c.Total() != 4"),
        (
            "cart.go",
            "// Cart holds prices.",
            "// Cart holds the prices.",
        ),
        ("cart.go", "MaxItems = 3", "MaxItems = 4"),
        "::TestRefusesAFourthItem",
    );
}

#[test]
fn the_java_gate_tells_base_failures_from_new_ones() {
    base_failures_are_not_new(
        Language::Java,
        "java",
        "target/\n",
        (
            "src/test/java/example/cart/CartTest.java",
            "assertEquals(3,",
            "assertEquals(4,",
        ),
        (
            "src/main/java/example/cart/Cart.java",
            "/** Small class",
            "/** A small class",
        ),
        (
            "src/main/java/example/cart/Cart.java",
            "MAX_ITEMS = 3",
            "MAX_ITEMS = 4",
        ),
        "example.cart.CartTest.refusesAFourthItem",
    );
}
