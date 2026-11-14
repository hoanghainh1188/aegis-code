//! The gate with sidecars (#490), on a `docker` that only logs: which
//! steps reach the sidecars, when they start, reset and go away.
#![cfg(unix)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aegis_gate::{
    AuditPolicy, CancelToken, GateRequest, Hardening, Sandbox, SidecarSpec, Sidecars, run_gate,
};
use aegis_lang::Language;
use common::{Repo, profile};

const FAKE_COVERAGE: &str = r#"
[gate]
coverage = ["sh", "-c", "echo TN: > lcov.info"]
[coverage]
format = "lcov"
path = "lcov.info"
"#;

/// Logs every call to `calls`; containers run and start at once, probes
/// and step commands exit 0.
fn fake_docker(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("docker");
    std::fs::write(
        &path,
        "#!/bin/sh\n\
         echo \"$*\" >> \"$(dirname \"$0\")/calls\"\n\
         case \"$1 $3\" in\n\
         *State.Running*) echo true;;\n\
         *State.StartedAt*) echo 2026-11-14T09:00:00.1Z;;\n\
         *State.OOMKilled*) echo false;;\n\
         esac\n\
         exit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn postgres() -> SidecarSpec {
    SidecarSpec {
        name: "postgres".into(),
        image: "postgres:16-alpine".into(),
        user: "70:70".into(),
        env: BTreeMap::new(),
        tmpfs: vec!["/var/lib/postgresql/data".into()],
        ready: vec!["pg_isready".into()],
        ready_timeout_s: 5,
        localhost: false,
        test_env: BTreeMap::from([("DATABASE_URL".into(), "postgres://postgres:5432/t".into())]),
        memory_mb: None,
        cpus: None,
    }
}

#[test]
fn only_the_test_steps_reach_fresh_sidecars() {
    let repo = Repo::new("rust", &[]);
    repo.replace(
        "src/lib.rs",
        "/// A shopping cart.",
        "/// A shopping cart of prices.",
    );
    let fake = repo.work().with_file_name("fake-docker");
    let docker = fake_docker(&fake).display().to_string();
    let sandbox = Sandbox {
        docker: docker.clone(),
        image: "aegis-sandbox-rust".into(),
        mounts: vec![],
        env: vec![],
        network: None,
        read_only_mounts: true,
        hardening: Hardening::default(),
    };
    let profile = profile(Language::Rust, FAKE_COVERAGE);
    let (dir, work) = (repo.dir(), repo.work());
    let allowed = BTreeSet::new();
    let request = GateRequest {
        repo: &dir,
        base: "main",
        profile: &profile,
        allowed: &allowed,
        repro: None,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(60),
        cancel: CancelToken::new(),
        sandbox: Some(sandbox),
        fetch: None,
        build_cache: None,
        allowed_tests: vec![],
        reference_groups: vec![],
        sonar: None,
        audit: AuditPolicy::default(),
        scope: aegis_gate::ScopePolicy::default(),
        parts: vec![],
        sidecars: Some(Sidecars {
            docker,
            specs: vec![postgres()],
        }),
    };
    run_gate(&request).unwrap();

    let calls = std::fs::read_to_string(fake.join("calls")).unwrap();
    let calls: Vec<&str> = calls.lines().collect();
    let at = |what: &str| {
        calls
            .iter()
            .position(|c| c.contains(what))
            .unwrap_or_else(|| panic!("no call with {what:?}: {calls:#?}"))
    };
    let step = |what: &str| calls[at(what)];

    for quiet in ["cargo fmt", "cargo clippy"] {
        assert!(step(quiet).contains("--network none"), "{}", step(quiet));
    }
    let test = step("cargo test");
    assert!(test.contains("--network aegis-sidecar-"), "{test}");
    assert!(
        test.contains("--env DATABASE_URL=postgres://postgres:5432/t"),
        "{test}"
    );
    let coverage = step("echo TN:");
    assert!(coverage.contains("--network aegis-sidecar-"), "{coverage}");

    // Started only when the test step needed them, reset before coverage.
    let created = at("network create --internal aegis-sidecar-");
    assert!(at("cargo clippy") < created && created < at("cargo test"));
    let stop = at("stop --time 5 aegis-sidecar-");
    assert!(
        at("cargo test") < stop && stop < at("echo TN:"),
        "{calls:#?}"
    );
    // And gone when the gate returned.
    let removed = at("rm --force --volumes aegis-sidecar-");
    assert!(removed > at("echo TN:"));
    assert!(
        calls[removed..]
            .iter()
            .any(|c| c.starts_with("network rm aegis-sidecar-"))
    );
}

#[test]
fn sidecars_without_the_sandbox_are_refused() {
    let repo = Repo::new("rust", &[]);
    let profile = profile(Language::Rust, FAKE_COVERAGE);
    let (dir, work) = (repo.dir(), repo.work());
    let allowed = BTreeSet::new();
    let err = run_gate(&GateRequest {
        repo: &dir,
        base: "main",
        profile: &profile,
        allowed: &allowed,
        repro: None,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(60),
        cancel: CancelToken::new(),
        sandbox: None,
        fetch: None,
        build_cache: None,
        allowed_tests: vec![],
        reference_groups: vec![],
        sonar: None,
        audit: AuditPolicy::default(),
        scope: aegis_gate::ScopePolicy::default(),
        parts: vec![],
        sidecars: Some(Sidecars {
            docker: "docker".into(),
            specs: vec![postgres()],
        }),
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("sidecars need the sandbox"),
        "{err}"
    );
}

/// The images of the real-Docker test, or whether it is skipped.
fn docker_ready(images: &[&str]) -> bool {
    let ok = images.iter().all(|image| {
        std::process::Command::new("docker")
            .args(["image", "inspect", image])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or one of {images:?} is missing"
        );
        eprintln!("skipping: docker or one of {images:?} is missing");
    }
    ok
}

/// A Python repo whose patch adds a test that connects to `$PGHOST:5432`.
fn db_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(repo.join("README.md"), "db\n").unwrap();
    let git = |args: &[&str]| common::git(&repo, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "base",
    ]);
    std::fs::write(
        repo.join("tests/test_db.py"),
        "import os\nimport socket\n\n\ndef test_reaches_postgres():\n    \
         host = os.environ[\"PGHOST\"]\n    \
         with socket.create_connection((host, 5432), timeout=10):\n        pass\n",
    )
    .unwrap();
    repo
}

/// The test step's status and whether it passed only as on the base.
fn db_gate(root: &Path, sidecars: Option<Vec<SidecarSpec>>) -> aegis_gate::StepStatus {
    let repo = db_repo(root);
    let work = root.join("work");
    let profile = profile(Language::Python, "");
    let sandbox = Sandbox {
        docker: "docker".into(),
        image: "aegis-sandbox-python".into(),
        mounts: vec![],
        env: vec![],
        network: None,
        read_only_mounts: true,
        hardening: Hardening::new(Default::default(), &profile.profile.sandbox),
    };
    let allowed = BTreeSet::new();
    let report = run_gate(&GateRequest {
        repo: &repo,
        base: "main",
        profile: &profile,
        allowed: &allowed,
        repro: None,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(300),
        cancel: CancelToken::new(),
        sandbox: Some(sandbox),
        fetch: None,
        build_cache: None,
        allowed_tests: vec![],
        reference_groups: vec![],
        sonar: None,
        audit: AuditPolicy::default(),
        scope: aegis_gate::ScopePolicy::default(),
        parts: vec![],
        sidecars: sidecars.map(|specs| Sidecars {
            docker: "docker".into(),
            specs,
        }),
    })
    .unwrap();
    let test = report.step(aegis_gate::StepName::Test).unwrap();
    assert!(!test.fails_on_base, "{report}");
    test.status.clone()
}

fn real_postgres(host: &str, localhost: bool) -> SidecarSpec {
    SidecarSpec {
        name: "postgres".into(),
        image: "postgres:16-alpine".into(),
        user: "70:70".into(),
        env: BTreeMap::from([("POSTGRES_PASSWORD".into(), "test".into())]),
        tmpfs: vec![
            "/var/lib/postgresql/data".into(),
            "/var/run/postgresql".into(),
        ],
        ready: vec!["pg_isready".into(), "-h".into(), "localhost".into()],
        ready_timeout_s: 60,
        localhost,
        test_env: BTreeMap::from([("PGHOST".into(), host.into())]),
        memory_mb: None,
        cpus: None,
    }
}

/// Needs Docker and the images `aegis-sandbox-python` (`docker build -t
/// aegis-sandbox-python docker/python`) and `postgres:16-alpine`.
#[test]
fn a_test_reaches_postgres_by_name_or_on_localhost() {
    if !docker_ready(&["aegis-sandbox-python", "postgres:16-alpine"]) {
        return;
    }
    let root = std::env::temp_dir().join(format!("aegis-sidecar-db-{}", std::process::id()));
    let run = |name: &str, sidecars| {
        let dir = root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        db_gate(&dir, sidecars)
    };
    use aegis_gate::StepStatus;
    assert_eq!(
        run("name", Some(vec![real_postgres("postgres", false)])),
        StepStatus::Passed
    );
    assert_eq!(
        run("localhost", Some(vec![real_postgres("localhost", true)])),
        StepStatus::Passed
    );
    // Without it the test has nothing to connect to.
    assert_eq!(run("none", None), StepStatus::Failed);
    let left = std::process::Command::new("docker")
        .args(["ps", "-aq", "--filter", "name=aegis-sidecar-"])
        .output()
        .unwrap();
    assert!(left.stdout.is_empty(), "sidecars left behind");
    let _ = std::fs::remove_dir_all(&root);
}
