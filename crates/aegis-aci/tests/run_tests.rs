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
    // The root part's directory is `""`: the tree is mounted as `<root>/`.
    // No language server ran before (#268), so this is the test's own run.
    let root = repo.repo().canonicalize().unwrap();
    // Removed with its volumes; a test that passed is not asked about its
    // memory.
    assert!(!calls.contains("\ninspect "), "{calls}");
    assert!(
        calls.contains("\nrm --force --volumes aegis-gate-"),
        "{calls}"
    );
    for expected in [
        "run --name aegis-gate-",
        "--network none --cap-drop ALL --security-opt no-new-privileges",
        // The run's limits (the defaults here) and the Rust profile's
        // writable dirs over a read-only image (#320).
        "--memory 8192m --memory-swap 8192m --cpus 4",
        "--read-only --tmpfs /tmp:rw,exec,nosuid,nodev,size=1024m",
        "--mount type=volume,destination=/home/aegis \
         --mount type=volume,destination=/usr/local/cargo",
        &format!("--volume {}/:/work --workdir /work", root.display()),
        "/deps:/usr/local/cargo/registry:ro",
        "--env CARGO_BUILD_JOBS=2 aegis-sandbox-rust cargo test --workspace -- --exact \
         tests::adds_up_prices",
    ] {
        assert!(calls.contains(expected), "{expected}\n{calls}");
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_runs_limits_bound_the_test_container() {
    let repo = Scratch::fixture("rust");
    let docker = fake_docker(&repo.outside().join("bin"));
    let mut spec = sandboxed(&repo, &docker, 60);
    spec.args.extend(
        [
            "--sandbox-memory-mb",
            "3072",
            "--sandbox-cpus",
            "2",
            "--sandbox-pids",
            "512",
            "--sandbox-tmp-mb",
            "128",
        ]
        .map(str::to_owned),
    );
    let tools = McpTools::connect(&[spec]).await.unwrap();
    let out = tools
        .call("run_tests", &json!({"test": "tests::adds_up_prices"}))
        .await;
    tools.shutdown().await;
    assert!(!out.is_error, "{}", out.content);
    let calls = std::fs::read_to_string(repo.outside().join("bin/calls.log")).unwrap();
    for expected in [
        "--pids-limit 512 --memory 3072m --memory-swap 3072m --cpus 2",
        "/tmp:rw,exec,nosuid,nodev,size=128m",
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
    // Stopped and removed, volumes and all.
    assert!(
        calls.contains("\nrm --force --volumes aegis-gate-"),
        "{calls}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_test_of_a_part_runs_in_its_image_with_its_dependency_dirs() {
    // #198: the part's `.venv` and caches, as the gate mounts them.
    let repo = Scratch::two_languages();
    let docker = fake_docker(&repo.outside().join("bin"));
    let venv = repo.outside().join("parts/py/deps/.venv");
    let uv = repo.outside().join("parts/py/uv");
    let parts = repo.outside().join("parts.toml");
    std::fs::write(
        &parts,
        format!(
            "[[part]]\ndir = \"py\"\nlanguage = \"python\"\n\
             mounts = [{{ host = '{}', path = \"/work/.venv\" }}, \
             {{ host = '{}', path = \"/home/aegis/.cache/uv\" }}]\n",
            venv.display(),
            uv.display()
        ),
    )
    .unwrap();
    let mut spec = sandboxed(&repo, &docker, 60);
    spec.args
        .extend(["--parts".to_owned(), parts.display().to_string()]);
    let tools = McpTools::connect(&[spec]).await.unwrap();
    let out = tools
        .call(
            "run_tests",
            &json!({"test": "py/tests/test_cart.py::test_adds_up_prices"}),
        )
        .await;
    tools.shutdown().await;
    assert!(!out.is_error, "{}", out.content);
    let calls = std::fs::read_to_string(repo.outside().join("bin/calls.log")).unwrap();
    let test = calls
        .lines()
        .find(|l| l.starts_with("run --name aegis-gate-"))
        .unwrap_or_else(|| panic!("{calls}"));
    let root = repo.repo().canonicalize().unwrap();
    for expected in [
        format!("--volume {}/py:/work --workdir /work", root.display()),
        format!("--volume {}:/work/.venv:ro", venv.display()),
        format!("--volume {}:/home/aegis/.cache/uv:ro", uv.display()),
        "aegis-sandbox-python python -m pytest".to_owned(),
        // The part's profile's writable dirs, not the root's (#320).
        "--mount type=volume,destination=/home/aegis --volume".to_owned(),
    ] {
        assert!(test.contains(&expected), "{expected}\n{test}");
    }
    // The root's mounts are the root's own.
    assert!(!test.contains("/usr/local/cargo/registry"), "{test}");
    assert!(!test.contains("destination=/usr/local/cargo"), "{test}");
}

/// [`fake_docker`] that also answers `inspect`: sidecars and their
/// containers run (#491).
#[cfg(unix)]
fn sidecar_docker(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("docker");
    let log = dir.join("calls.log");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{log}'\n\
             case \"$1 $3\" in\n\
             *State.Running*) echo true; exit 0 ;;\n\
             *State.StartedAt*) echo 2026-11-14T09:00:00.1Z; exit 0 ;;\n\
             *State.OOMKilled*) echo false; exit 0 ;;\n\
             esac\n\
             case \"$*\" in\n  'run --name aegis-gate-'*) \
             echo 'test tests::adds_up_prices ... ok'; \
             echo 'test result: ok. 1 passed; 0 failed'; exit 0 ;;\nesac\nexit 0\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn run_tests_reaches_fresh_sidecars_that_go_with_the_session() {
    let repo = Scratch::fixture("rust");
    let bin = repo.outside().join("bin");
    let docker = sidecar_docker(&bin);
    let sidecars = repo.outside().join("sidecars.toml");
    std::fs::write(
        &sidecars,
        "[[sidecar]]\nname = \"postgres\"\nimage = \"postgres:16-alpine\"\nuser = \"70:70\"\n\
         tmpfs = [\"/var/lib/postgresql/data\"]\nready = [\"pg_isready\"]\n\
         test_env = { DATABASE_URL = \"postgres://postgres:5432/t\" }\n",
    )
    .unwrap();
    let mut spec = sandboxed(&repo, &docker, 60);
    spec.args
        .extend(["--sidecars".to_owned(), sidecars.display().to_string()]);
    let tools = McpTools::connect(&[spec]).await.unwrap();
    for _ in 0..2 {
        let out = tools
            .call("run_tests", &json!({"test": "tests::adds_up_prices"}))
            .await;
        assert!(!out.is_error, "{}", out.content);
    }
    tools.shutdown().await;
    let log = bin.join("calls.log");
    // The session's end removes them; the ACI does it as it exits.
    let mut calls = String::new();
    for _ in 0..100 {
        calls = std::fs::read_to_string(&log).unwrap();
        if calls.contains("network rm aegis-sidecar-") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let lines: Vec<&str> = calls.lines().collect();
    let at = |what: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(what))
            .unwrap_or_else(|| panic!("no {what:?} in\n{calls}"))
    };
    let tests: Vec<&&str> = lines
        .iter()
        .filter(|l| l.starts_with("run --name aegis-gate-"))
        .collect();
    assert_eq!(tests.len(), 2, "{calls}");
    for test in &tests {
        assert!(test.contains("--network aegis-sidecar-"), "{test}");
        assert!(
            test.contains("--env DATABASE_URL=postgres://postgres:5432/t"),
            "{test}"
        );
    }
    // Started before the first run, reset before the second, gone at the end.
    assert!(at("network create --internal aegis-sidecar-") < at("run --name aegis-gate-"));
    assert!(at("run -d --name aegis-sidecar-") < at("run --name aegis-gate-"));
    let second = lines
        .iter()
        .rposition(|l| l.starts_with("run --name aegis-gate-"))
        .unwrap();
    let stop = at("stop --time 5 aegis-sidecar-");
    assert!(
        at("run --name aegis-gate-") < stop && stop < second,
        "{calls}"
    );
    assert!(
        at("rm --force --volumes aegis-sidecar-") > second,
        "{calls}"
    );
    assert!(at("network rm aegis-sidecar-") > second, "{calls}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sidecars_need_the_sandbox() {
    let repo = Scratch::fixture("rust");
    std::fs::create_dir_all(repo.outside()).unwrap();
    let sidecars = repo.outside().join("sidecars.toml");
    std::fs::write(
        &sidecars,
        "[[sidecar]]\nname = \"postgres\"\nimage = \"postgres:16-alpine\"\nuser = \"70:70\"\n\
         ready = [\"pg_isready\"]\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_aegis-aci"))
        .args([
            "--repo",
            &repo.repo().display().to_string(),
            "--language",
            "rust",
        ])
        .args(["--sidecars", &sidecars.display().to_string()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--sidecars needs --format-image"), "{err}");
    std::fs::write(
        &sidecars,
        "[[sidecar]]\nname = \"pg\"\nimage = \"x\"\nuser = \"0:0\"\nready = [\"true\"]\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_aegis-aci"))
        .args([
            "--repo",
            &repo.repo().display().to_string(),
            "--language",
            "rust",
        ])
        .args([
            "--format-image",
            "img",
            "--sidecars",
            &sidecars.display().to_string(),
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("non-root"), "{err}");
}

/// Needs Docker and the images `aegis-sandbox-python` and
/// `postgres:16-alpine`; skipped otherwise unless `AEGIS_REQUIRE_DOCKER=1`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn run_tests_reaches_a_real_postgres() {
    let images = ["aegis-sandbox-python", "postgres:16-alpine"];
    let ready = images.iter().all(|image| {
        std::process::Command::new("docker")
            .args(["image", "inspect", image])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !ready {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or one of {images:?} is missing"
        );
        eprintln!("skipping: docker or one of {images:?} is missing");
        return;
    }
    let repo = Scratch::fixture("python");
    std::fs::write(
        repo.repo().join("tests/test_db.py"),
        "import os\nimport socket\n\n\ndef test_reaches_postgres():\n    \
         with socket.create_connection((os.environ[\"PGHOST\"], 5432), timeout=10):\n        \
         pass\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.outside()).unwrap();
    let sidecars = repo.outside().join("sidecars.toml");
    std::fs::write(
        &sidecars,
        "[[sidecar]]\nname = \"postgres\"\nimage = \"postgres:16-alpine\"\nuser = \"70:70\"\n\
         env = { POSTGRES_PASSWORD = \"test\" }\n\
         tmpfs = [\"/var/lib/postgresql/data\", \"/var/run/postgresql\"]\n\
         ready = [\"pg_isready\", \"-h\", \"localhost\"]\n\
         test_env = { PGHOST = \"postgres\" }\n",
    )
    .unwrap();
    let spec = |with_sidecars: bool| {
        let mut args: Vec<String> = [
            "--repo",
            &repo.repo().display().to_string(),
            "--language",
            "python",
            "--format-image",
            "aegis-sandbox-python",
            "--test-dir",
            &repo.outside().join("tests").display().to_string(),
        ]
        .map(str::to_owned)
        .to_vec();
        if with_sidecars {
            args.extend(["--sidecars".to_owned(), sidecars.display().to_string()]);
        } else {
            args.extend(["--test-env".to_owned(), "PGHOST=postgres".to_owned()]);
        }
        McpServerSpec {
            name: "aci".into(),
            program: env!("CARGO_BIN_EXE_aegis-aci").to_owned(),
            args,
            cwd: None,
            env: std::env::var("DOCKER_HOST")
                .map(|h| vec![("DOCKER_HOST".to_owned(), h)])
                .unwrap_or_default(),
            tools: Some(vec!["run_tests".into()]),
        }
    };
    let test = json!({"test": "tests/test_db.py::test_reaches_postgres"});
    let tools = McpTools::connect(&[spec(true)]).await.unwrap();
    for _ in 0..2 {
        let out = tools.call("run_tests", &test).await;
        assert!(out.content.starts_with("PASS"), "{}", out.content);
    }
    tools.shutdown().await;
    let tools = McpTools::connect(&[spec(false)]).await.unwrap();
    let out = tools.call("run_tests", &test).await;
    tools.shutdown().await;
    assert!(out.content.starts_with("FAIL"), "{}", out.content);
}
