//! #5: the gate's steps in the profile's container, with no network.
//! Needs Docker and the image built from `docker/rust`
//! (`docker build -t aegis-sandbox-rust docker/rust`); skipped otherwise,
//! unless `AEGIS_REQUIRE_DOCKER=1`.

mod common;

use std::process::Command;
use std::time::Duration;

use aegis_gate::{CancelToken, Sandbox, Skip, StepName, StepStatus};
use aegis_lang::Language;
use common::{Repo, profile, statuses};

const IMAGE: &str = "aegis-sandbox-rust";

fn docker_ready() -> bool {
    let ok = Command::new("docker")
        .args(["image", "inspect", IMAGE])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or the {IMAGE} image is missing"
        );
        eprintln!("skipping: docker or the {IMAGE} image is missing");
    }
    ok
}

fn sandbox(repo: &Repo) -> Sandbox {
    let registry = repo.work().join("cargo-registry");
    std::fs::create_dir_all(&registry).unwrap();
    Sandbox {
        docker: "docker".into(),
        image: IMAGE.into(),
        mounts: vec![(registry, "/usr/local/cargo/registry".into())],
        env: vec![],
        network: None,
        read_only_mounts: true,
    }
}

const FAKE_COVERAGE: &str = r#"
[gate]
coverage = ["sh", "-c", "echo TN: > lcov.info"]
[coverage]
format = "lcov"
path = "lcov.info"
"#;

#[test]
fn the_rust_gate_passes_inside_the_container() {
    if !docker_ready() {
        return;
    }
    let repo = Repo::new("rust", &[]);
    repo.replace(
        "src/lib.rs",
        "/// A shopping cart.",
        "/// A shopping cart of prices.",
    );
    let sandbox = sandbox(&repo);
    let report = repo.gate_in(
        &profile(Language::Rust, FAKE_COVERAGE),
        None,
        Duration::from_secs(600),
        CancelToken::new(),
        vec![],
        Some(sandbox),
    );
    assert_eq!(
        statuses(&report)[1..7],
        [
            (StepName::Format, StepStatus::Passed),
            (StepName::Lint, StepStatus::Passed),
            (StepName::Typecheck, StepStatus::Skipped(Skip::NotInProfile)),
            (StepName::ReproTest, StepStatus::Skipped(Skip::NoReproTest)),
            (StepName::Test, StepStatus::Passed),
            (StepName::Coverage, StepStatus::Passed),
        ],
        "{report}"
    );
}

#[test]
fn steps_have_no_network_and_no_host_environment() {
    if !docker_ready() {
        return;
    }
    // `cargo test` sets CARGO_PKG_NAME here; the container must not see
    // it, must see the request's env, and must not reach any host.
    let probe = profile(
        Language::Rust,
        r#"
[gate]
format = ["sh", "-c", "test -z \"$CARGO_PKG_NAME\" && test \"$GATE_X\" = 1 && test \"$(id -u)\" != 0"]
lint = ["bash", "-c", "exec 3<>/dev/tcp/1.1.1.1/443 && echo connected"]
test = ["true"]
coverage = false
repro_test = false
"#,
    );
    let repo = Repo::new("rust", &[]);
    let sandbox = sandbox(&repo);
    let report = repo.gate_in(
        &probe,
        None,
        Duration::from_secs(120),
        CancelToken::new(),
        vec![("GATE_X".into(), "1".into())],
        Some(sandbox),
    );
    assert_eq!(
        report.step(StepName::Format).unwrap().status,
        StepStatus::Passed,
        "{report}"
    );
    let lint = report.step(StepName::Lint).unwrap();
    assert_eq!(
        lint.status,
        StepStatus::Failed,
        "the network was reachable: {report}"
    );
    assert!(
        !lint.detail.lines().any(|l| l == "connected"),
        "{}",
        lint.detail
    );
    assert!(
        lint.detail.contains("Network is unreachable"),
        "{}",
        lint.detail
    );
}

#[test]
fn a_timed_out_step_stops_its_container() {
    if !docker_ready() {
        return;
    }
    let slow = profile(Language::Rust, "[gate]\nformat = [\"sleep\", \"300\"]\n");
    let repo = Repo::new("rust", &[]);
    let sandbox = sandbox(&repo);
    let report = repo.gate_in(
        &slow,
        None,
        Duration::from_secs(5),
        CancelToken::new(),
        vec![],
        Some(sandbox),
    );
    assert!(
        report
            .step(StepName::Format)
            .unwrap()
            .detail
            .starts_with("timed out"),
        "{report}"
    );
    let running = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("name=aegis-gate-{}-", std::process::id()),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&running.stdout).trim(),
        "",
        "a container was left running"
    );
}
