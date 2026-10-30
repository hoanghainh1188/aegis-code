//! #210: the gate of the TypeScript, Java, Go and C# profiles on their
//! fixture repos (C# also under Microsoft.Testing.Platform, #228), inside
//! the profile's image with no network, and the reproduction-test command
//! `run_tests` runs there too. A test the base revision already fails is
//! told apart from one the patch breaks (#291), which for C# and
//! TypeScript needs the base copy's own dependencies (#303). Dependencies
//! are fetched first in a container of the same image that has the
//! network, into the caches the gate then mounts read-only, as the
//! lifecycle does (#66); the gate fetches the base copy's through the
//! egress proxy. Needs Docker, the network and the images built from
//! `docker/<image>` (`docker build -t aegis-sandbox-node docker/node`), the
//! proxy's too (`aegis-sandbox-python`); skipped otherwise, unless
//! `AEGIS_REQUIRE_DOCKER=1`.

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use aegis_gate::{
    CancelToken, Egress, FetchStep, GateReport, GateRequest, Repro, Sandbox, Skip, StepName,
    StepStatus, run_gate,
};
use aegis_lang::{Language, Placeholder, ResolvedProfile};
use common::{Repo, profile, statuses};

/// The egress proxy runs in the Python image (#66).
const PROXY_IMAGE: &str = "aegis-sandbox-python";

fn docker_ready(image: &str) -> bool {
    let ok = [image, PROXY_IMAGE].iter().all(|image| {
        Command::new("docker")
            .args(["image", "inspect", image])
            .output()
            .is_ok_and(|o| o.status.success())
    });
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

/// The gate on `repo` inside the profile's image, with no network, and the
/// profile's fetch through the egress proxy, as the lifecycle gives it.
fn gate_in_container(
    repo: &Repo,
    profile: &ResolvedProfile,
    mounts: &[(PathBuf, String)],
    repro: Option<&Repro>,
) -> GateReport {
    let allowed = BTreeSet::new();
    let (dir, work) = (repo.dir(), repo.work());
    run_gate(&GateRequest {
        repo: &dir,
        base: "main",
        profile,
        allowed: &allowed,
        repro,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(900),
        cancel: CancelToken::new(),
        sandbox: Some(Sandbox {
            docker: "docker".into(),
            image: profile.image.clone(),
            mounts: mounts.to_vec(),
            env: vec![],
            network: None,
            read_only_mounts: true,
        }),
        fetch: Some(FetchStep {
            egress: Egress {
                docker: "docker".into(),
                proxy_image: PROXY_IMAGE.into(),
                allow: profile.fetch_hosts.clone(),
            },
            command: profile.fetch.command().unwrap().render(&[]).unwrap(),
            audit_hosts: vec![],
        }),
        build_cache: None,
        allowed_tests: vec![],
        reference_groups: vec![],
        sonar: None,
        audit: Default::default(),
        parts: vec![],
    })
    .unwrap()
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
    let report = gate_in_container(&repo, &profile, &mounts, None);
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
#[allow(clippy::too_many_arguments)]
fn base_failures_are_not_new(
    language: Language,
    overrides: &str,
    fixture: &str,
    ignored: &str,
    failing: Edit,
    harmless: Edit,
    breaking: Edit,
    broken: &str,
) {
    let profile = profile(language, overrides);
    if !docker_ready(&profile.image) {
        return;
    }
    let repo = Repo::new(fixture, &[failing]);
    repo.write(".gitignore", ignored);
    repo.commit_base("ignore what the steps write");
    let mounts = fetched(&repo, &profile);

    repo.replace(harmless.0, harmless.1, harmless.2);
    let report = gate_in_container(&repo, &profile, &mounts, None);
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Passed, "{report}");
    assert!(
        test.detail
            .starts_with("1 tests fail, as on the base revision"),
        "{}",
        test.detail
    );

    repo.replace(breaking.0, breaking.1, breaking.2);
    let report = gate_in_container(&repo, &profile, &mounts, None);
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
        "",
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
        "",
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

#[test]
fn the_csharp_gate_tells_base_failures_from_new_ones() {
    for fixture in ["csharp", "csharp-mtp"] {
        base_failures_are_not_new(
            Language::CSharp,
            "",
            fixture,
            "bin/\nobj/\nTestResults/\n",
            (
                "tests/Cart.Tests/CartTests.cs",
                "Assert.Equal(3,",
                "Assert.Equal(4,",
            ),
            (
                "src/Cart/Cart.cs",
                "/// <summary>Small file",
                "/// <summary>A small file",
            ),
            ("src/Cart/Cart.cs", "MaxItems = 3", "MaxItems = 4"),
            "RefusesAFourthItem",
        );
    }
}

/// The Vitest fixture has no formatter, linter or coverage set up.
const VITEST_ONLY: &str =
    "[gate]\nformat = false\nlint = false\ntypecheck = false\ncoverage = false\n";

#[test]
fn the_typescript_gate_tells_base_failures_from_new_ones() {
    base_failures_are_not_new(
        Language::TypeScript,
        VITEST_ONLY,
        "typescript-vitest",
        "node_modules/\n",
        ("test/cart.test.ts", "toBe(3)", "toBe(4)"),
        (
            "src/cart.ts",
            "/** A cart of prices. */",
            "/** A cart of the prices. */",
        ),
        ("src/cart.ts", "MAX_ITEMS = 3", "MAX_ITEMS = 4"),
        "test/cart.test.ts > cart > refuses a fourth item",
    );
}

/// #303: the reproduction test fails on the base copy, whose `obj/` comes
/// from its own restore; with none, `dotnet test --no-restore` there ran
/// nothing and exited 0, as if the test passed without the fix.
#[test]
fn a_csharp_reproduction_test_fails_on_the_base_copy() {
    let profile = profile(Language::CSharp, "");
    if !docker_ready(&profile.image) {
        return;
    }
    let repo = Repo::new(
        "csharp",
        &[("src/Cart/Cart.cs", "MaxItems = 3", "MaxItems = 4")],
    );
    repo.write(".gitignore", "bin/\nobj/\nTestResults/\n");
    repo.commit_base("ignore what the steps write");
    let mounts = fetched(&repo, &profile);
    repo.replace("src/Cart/Cart.cs", "MaxItems = 4", "MaxItems = 3");
    repo.write(
        "tests/Cart.Tests/ReproTests.cs",
        "namespace Cart.Tests;

public class ReproTests
{
    [Fact]
    public void RefusesAFourthItemAgain()
    {
        var cart = Example.Cart.Cart.Of(1, 2, 3);
        Assert.Throws<Example.Cart.CartFullException>(() => cart.Add(4));
    }
}
",
    );
    let repro = Repro {
        test: "FullyQualifiedName~Cart.Tests.ReproTests".into(),
        files: vec!["tests/Cart.Tests/ReproTests.cs".into()],
    };
    let report = gate_in_container(&repo, &profile, &mounts, Some(&repro));
    let step = report.step(StepName::ReproTest).unwrap();
    assert_eq!(step.status, StepStatus::Passed, "{report}");
}
