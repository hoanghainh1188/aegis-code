//! `run_tests` (#137): one test with the profile's `gate.repro_test`
//! command, on the machine (cargo; pytest when installed) and, through the
//! binary, in the gate's container (a fake `docker` that records what it
//! was asked to run).

mod common;

use std::path::Path;
use std::time::Duration;

use aegis_aci::{AciError, TestSettings};
use aegis_gate::Exit;
use aegis_lang::{Language, ProfileOverrides};
use aegis_orchestrator::{McpServerSpec, McpTools, ToolExecutor};
use common::{Scratch, profile, tool_available};
use serde_json::json;

fn settings(scratch: &Scratch) -> TestSettings {
    TestSettings {
        work_dir: scratch.outside().join("tests"),
        ..TestSettings::default()
    }
}

const FAILING: &str = "use fixture_rust::{cart, Priced};\n\n#[test]\nfn total_is_wrong() {\n    \
                       assert_eq!(cart![1, 2].total(), 4);\n}\n";

#[test]
fn a_rust_test_passes_or_fails_by_name_with_its_logs_outside_the_repo() {
    let repo = Scratch::fixture("rust");
    repo.write("tests/total.rs", FAILING);
    let mut aci = repo
        .aci(profile(Language::Rust, &ProfileOverrides::default()))
        .with_test_settings(settings(&repo));

    let passed = aci.run_tests("tests::adds_up_prices").unwrap();
    assert_eq!(passed.exit, Exit::Code(0), "{passed}");
    assert!(
        passed
            .to_string()
            .starts_with("PASSED `tests::adds_up_prices`"),
        "{passed}"
    );

    let failed = aci.run_tests("total_is_wrong").unwrap();
    assert_eq!(failed.exit, Exit::Code(101), "{failed}");
    let text = failed.to_string();
    assert!(
        text.starts_with("FAILED `total_is_wrong` (exit code 101"),
        "{text}"
    );
    assert!(
        failed.failing.iter().any(|t| t.ends_with("total_is_wrong")),
        "{text}"
    );
    assert!(text.contains("--- end of output ---\n"), "{text}");
    assert!(repo.outside().join("tests/run_tests-2.log").is_file());
    // The logs and TMPDIR stay outside the repo.
    assert!(!repo.repo().join("run_tests-1.log").exists());
}

#[test]
fn names_that_are_empty_or_look_like_flags_are_refused() {
    let repo = Scratch::fixture("rust");
    let mut aci = repo
        .aci(profile(Language::Rust, &ProfileOverrides::default()))
        .with_test_settings(settings(&repo));
    assert_eq!(
        aci.run_tests("  ").unwrap_err(),
        AciError::InvalidInput("`test` is empty".into())
    );
    let flag = aci.run_tests("--include-ignored").unwrap_err();
    assert!(flag.to_string().contains("starts with `-`"), "{flag}");
}

#[test]
fn a_pytest_test_reports_the_failing_case() {
    if !tool_available(&["python", "-m", "pytest", "--version"]) {
        return;
    }
    let repo = Scratch::fixture("python");
    repo.write(
        "tests/test_total.py",
        "from cart import cart_of\n\n\ndef test_total_is_wrong():\n    \
         assert cart_of(1, 2).total == 4\n",
    );
    let mut aci = repo
        .aci(profile(Language::Python, &ProfileOverrides::default()))
        .with_test_settings(settings(&repo));

    let passed = aci
        .run_tests("tests/test_cart.py::test_adds_up_prices")
        .unwrap();
    assert_eq!(passed.exit, Exit::Code(0), "{passed}");

    let failed = aci
        .run_tests("tests/test_total.py::test_total_is_wrong")
        .unwrap();
    assert_eq!(failed.exit, Exit::Code(1), "{failed}");
    assert_eq!(
        failed.failing.iter().collect::<Vec<_>>(),
        ["tests/test_total.py::test_total_is_wrong"],
        "{failed}"
    );
    assert!(failed.tail.contains("1 failed"), "{failed}");
}

/// A `docker` that writes its arguments to `calls.log`, then prints
/// libtest output (`run`) or sleeps (`run` of a test named `slow`).
fn fake_docker(dir: &Path) -> String {
    let path = dir.join("docker");
    let log = dir.join("calls.log");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{log}'\n\
             case \"$*\" in\n  *' slow') sleep 30 ;;\n  run*) \
             echo 'test tests::adds_up_prices ... ok'; \
             echo 'test result: ok. 1 passed; 0 failed'; exit 0 ;;\nesac\n",
            log = log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.display().to_string()
}

fn sandboxed(repo: &Scratch, docker: &str, timeout_s: u64) -> McpServerSpec {
    let deps = repo.outside().join("deps");
    std::fs::create_dir_all(&deps).unwrap();
    McpServerSpec {
        name: "aci".into(),
        program: env!("CARGO_BIN_EXE_aegis-aci").to_owned(),
        args: [
            "--repo",
            &repo.repo().display().to_string(),
            "--language",
            "rust",
            "--format-image",
            "aegis-sandbox-rust",
            "--docker",
            docker,
            "--format-mount",
            &format!("{}:/usr/local/cargo/registry", deps.display()),
            "--test-env",
            "CARGO_BUILD_JOBS=2",
            "--test-timeout",
            &timeout_s.to_string(),
            "--test-dir",
            &repo.outside().join("tests").display().to_string(),
        ]
        .map(str::to_owned)
        .to_vec(),
        cwd: None,
        env: vec![],
        tools: Some(vec!["run_tests".into()]),
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn in_the_sandbox_the_test_runs_like_a_gate_step() {
    let repo = Scratch::fixture("rust");
    let docker = fake_docker(&repo.outside().join("bin"));
    let tools = McpTools::connect(&[sandboxed(&repo, &docker, 60)])
        .await
        .unwrap();
    let out = tools
        .call("run_tests", &json!({"test": "tests::adds_up_prices"}))
        .await;
    tools.shutdown().await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.starts_with("PASSED `tests::adds_up_prices`"),
        "{}",
        out.content
    );
    let calls = std::fs::read_to_string(repo.outside().join("bin/calls.log")).unwrap();
    let root = repo.repo().canonicalize().unwrap();
    for expected in [
        "run --rm --name aegis-gate-",
        "--network none --cap-drop ALL --security-opt no-new-privileges",
        &format!("--volume {}:/work --workdir /work", root.display()),
        "/deps:/usr/local/cargo/registry:ro",
        "--env CARGO_BUILD_JOBS=2 aegis-sandbox-rust cargo test --workspace -- --exact \
         tests::adds_up_prices",
    ] {
        assert!(calls.contains(expected), "{expected}\n{calls}");
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_test_over_the_time_limit_is_stopped_with_its_container() {
    let repo = Scratch::fixture("rust");
    let docker = fake_docker(&repo.outside().join("bin"));
    let tools = McpTools::connect(&[sandboxed(&repo, &docker, 1)])
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let out = tools.call("run_tests", &json!({"test": "slow"})).await;
    tools.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(
        out.content.starts_with("TIMED OUT `slow` after 1s"),
        "{}",
        out.content
    );
    let calls = std::fs::read_to_string(repo.outside().join("bin/calls.log")).unwrap();
    assert!(calls.contains("\nkill aegis-gate-"), "{calls}");
}
