//! #5: the gate's steps in the profile's container, with no network.
//! Needs Docker and the image built from `docker/rust`
//! (`docker build -t aegis-sandbox-rust docker/rust`); skipped otherwise,
//! unless `AEGIS_REQUIRE_DOCKER=1`.

mod common;

use std::process::Command;
use std::time::Duration;

use aegis_gate::{CancelToken, Hardening, Limits, Sandbox, Skip, StepName, StepStatus};
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
        hardening: Hardening::new(
            Limits::default(),
            &profile(Language::Rust, "").profile.sandbox,
        ),
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
    // it, must see the request's env, and must not reach any host. The
    // probe only connects in the repo (the base copy has no `.git`), so a
    // failure there is not excused by the base failing too.
    let probe = profile(
        Language::Rust,
        r#"
[gate]
format = ["sh", "-c", "test -z \"$CARGO_PKG_NAME\" && test \"$GATE_X\" = 1 && test \"$(id -u)\" != 0"]
lint = ["bash", "-c", "test ! -e .git || exec 3<>/dev/tcp/1.1.1.1/443 && echo connected"]
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
    // Its container is the only one running `sleep 300`; the other tests'
    // may be there meanwhile.
    let left: Vec<String> = containers()
        .into_iter()
        .filter(|c| c.contains("sleep 300"))
        .collect();
    assert_eq!(left, Vec::<String>::new(), "a container was left behind");
}

/// Name and command of this process's containers still there, running or
/// not: without `--rm` each is removed once it ends (#320).
fn containers() -> Vec<String> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--all",
            "--no-trunc",
            "--filter",
            &format!("name=aegis-gate-{}-", std::process::id()),
            "--format",
            "{{.Names}} {{.Command}}",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_step_over_its_memory_says_so_even_when_a_child_was_killed() {
    // #320: `tail` is killed, the shell exits 3. The exit code alone would
    // read as a plain failure, as `cargo` exits 101 when `rustc` is killed.
    if !docker_ready() {
        return;
    }
    let hungry = profile(
        Language::Rust,
        "[gate]\nformat = [\"sh\", \"-c\", \"head -c 512m /dev/zero | tail > /dev/null; exit 3\"]\n",
    );
    let repo = Repo::new("rust", &[]);
    let mut sandbox = sandbox(&repo);
    sandbox.hardening.limits = Limits {
        memory_mb: 64,
        ..Limits::default()
    };
    let report = repo.gate_in(
        &hungry,
        None,
        Duration::from_secs(120),
        CancelToken::new(),
        vec![],
        Some(sandbox),
    );
    let format = report.step(StepName::Format).unwrap();
    assert_eq!(format.status, StepStatus::Failed, "{report}");
    assert!(
        format.detail.contains("out of memory") && format.detail.contains("limit of 64 MB"),
        "{report}"
    );
}

#[test]
fn a_step_that_succeeds_stays_a_success_if_a_child_ran_out_of_memory() {
    // What a step runs may let a child go over the limit on purpose.
    if !docker_ready() {
        return;
    }
    let tolerant = profile(
        Language::Rust,
        "[gate]\nformat = [\"sh\", \"-c\", \"head -c 512m /dev/zero | tail > /dev/null; exit 0\"]\n",
    );
    let repo = Repo::new("rust", &[]);
    let mut sandbox = sandbox(&repo);
    sandbox.hardening.limits = Limits {
        memory_mb: 64,
        ..Limits::default()
    };
    let report = repo.gate_in(
        &tolerant,
        None,
        Duration::from_secs(120),
        CancelToken::new(),
        vec![],
        Some(sandbox),
    );
    let format = report.step(StepName::Format).unwrap();
    assert_eq!(format.status, StepStatus::Passed, "{report}");
}

#[test]
fn the_image_is_written_only_where_the_profile_says() {
    if !docker_ready() {
        return;
    }
    // HOME and CARGO_HOME (the profile's `writable`) and /tmp take writes,
    // /usr/bin does not.
    let check = profile(
        Language::Rust,
        "[gate]\nformat = [\"sh\", \"-c\", \"touch /home/aegis/x /usr/local/cargo/x /tmp/x \
         && ! touch /usr/bin/x 2>/dev/null\"]\n",
    );
    let repo = Repo::new("rust", &[]);
    let report = repo.gate_in(
        &check,
        None,
        Duration::from_secs(120),
        CancelToken::new(),
        vec![],
        Some(sandbox(&repo)),
    );
    let format = report.step(StepName::Format).unwrap();
    assert_eq!(format.status, StepStatus::Passed, "{report}");
}
