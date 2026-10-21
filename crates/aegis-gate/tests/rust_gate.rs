//! "Xong khi" of #11 for Rust: the gate runs the Rust profile's steps on
//! the fixture repo with sample patches (rustfmt, clippy and cargo needed).

mod common;

use std::time::{Duration, Instant};

use aegis_gate::{CancelToken, Repro, Skip, StepName, StepStatus};
use aegis_lang::Language;
use common::{Repo, profile, statuses, tools_available};

/// The coverage tool is not needed to test the step: a stand-in writes the
/// report.
const FAKE_COVERAGE: &str = r#"
[gate]
coverage = ["sh", "-c", "echo TN: > lcov.info"]
[coverage]
format = "lcov"
path = "lcov.info"
"#;

const BUG: (&str, &str, &str) = (
    "src/lib.rs",
    "self.items.len() >= MAX_ITEMS",
    "self.items.len() > MAX_ITEMS",
);

const REPRO_TEST: &str = "use fixture_rust::{cart, CartError};

#[test]
fn fourth_item_is_refused() {
    let mut cart = cart![1, 2, 3];
    assert_eq!(cart.add(4), Err(CartError::Full));
}
";

fn ready() -> bool {
    tools_available(&[
        &["cargo", "--version"],
        &["cargo", "fmt", "--version"],
        &["cargo", "clippy", "--version"],
    ])
}

fn repro() -> Repro {
    Repro {
        test: "fourth_item_is_refused".into(),
        files: vec!["tests/fourth_item.rs".into()],
    }
}

#[test]
fn a_fix_with_its_reproduction_test_passes_every_step() {
    if !ready() {
        return;
    }
    let repo = Repo::new("rust", &[BUG]);
    repo.replace(BUG.0, BUG.2, BUG.1);
    repo.write("tests/fourth_item.rs", REPRO_TEST);

    let report = repo.gate(&profile(Language::Rust, FAKE_COVERAGE), Some(&repro()));
    assert_eq!(
        statuses(&report),
        [
            (
                StepName::Dependencies,
                StepStatus::Skipped(Skip::NoDependencyChange)
            ),
            (StepName::Format, StepStatus::Passed),
            (StepName::Lint, StepStatus::Passed),
            (StepName::Typecheck, StepStatus::Skipped(Skip::NotInProfile)),
            (StepName::ReproTest, StepStatus::Passed),
            (StepName::Test, StepStatus::Passed),
            (StepName::Coverage, StepStatus::Passed),
            (StepName::Sonar, StepStatus::Skipped(Skip::NotConfigured)),
            (StepName::Integrity, StepStatus::Passed),
        ],
        "{report}"
    );
    assert!(report.passed());
    // The base checkout is gone and the patch is untouched.
    assert!(!repo.work().join("base").exists());
    assert!(repo.dir().join("tests/fourth_item.rs").exists());
    assert!(
        report
            .to_string()
            .starts_with("Gate passed.\n  dependencies skipped")
    );
}

#[test]
fn a_new_test_failure_fails_the_gate_and_names_the_test() {
    if !ready() {
        return;
    }
    let repo = Repo::new("rust", &[]);
    repo.replace(BUG.0, BUG.1, BUG.2);

    let report = repo.gate(&profile(Language::Rust, FAKE_COVERAGE), None);
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Failed, "{report}");
    assert!(
        test.detail.starts_with("1 tests fail that pass on the base revision:\n  fixture_rust::tests::refuses_a_fourth_item"),
        "{}",
        test.detail
    );
    assert_eq!(test.logs.len(), 2);
    assert_eq!(
        report.step(StepName::Coverage).unwrap().status,
        StepStatus::Skipped(Skip::NotRun)
    );
    assert!(!report.passed());
    assert!(report.to_string().starts_with("Gate failed at test."));
}

#[test]
fn a_failure_the_base_already_has_does_not_fail_the_gate() {
    if !ready() {
        return;
    }
    let repo = Repo::new("rust", &[BUG]);
    repo.replace(
        "src/lib.rs",
        "/// A shopping cart.",
        "/// A shopping cart of prices.",
    );

    let report = repo.gate(&profile(Language::Rust, FAKE_COVERAGE), None);
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Passed, "{report}");
    assert!(
        test.detail.contains("as on the base revision"),
        "{}",
        test.detail
    );
    assert!(report.passed(), "{report}");
}

#[test]
fn a_reproduction_test_that_passes_without_the_fix_fails() {
    if !ready() {
        return;
    }
    let repo = Repo::new("rust", &[]);
    repo.write("tests/fourth_item.rs", REPRO_TEST);

    let report = repo.gate(&profile(Language::Rust, FAKE_COVERAGE), Some(&repro()));
    let step = report.step(StepName::ReproTest).unwrap();
    assert_eq!(step.status, StepStatus::Failed, "{report}");
    assert!(
        step.detail.contains("also passes on the base revision"),
        "{}",
        step.detail
    );
}

#[test]
fn unformatted_code_stops_the_gate_at_format() {
    if !ready() {
        return;
    }
    let repo = Repo::new("rust", &[]);
    repo.replace(
        "src/lib.rs",
        "pub const MAX_ITEMS: usize = 3;",
        "pub const MAX_ITEMS : usize=3;",
    );

    let report = repo.gate(&profile(Language::Rust, FAKE_COVERAGE), None);
    let after: Vec<StepStatus> = report.steps[2..].iter().map(|s| s.status.clone()).collect();
    assert_eq!(
        report.step(StepName::Format).unwrap().status,
        StepStatus::Failed
    );
    assert!(
        after
            .iter()
            .all(|s| *s == StepStatus::Skipped(Skip::NotRun)),
        "{report}"
    );
}

#[test]
fn dependency_changes_are_checked_before_anything_runs() {
    let rust = profile(Language::Rust, FAKE_COVERAGE);
    let failed = |repo: &Repo| {
        let report = repo.gate(&rust, None);
        let deps = report.step(StepName::Dependencies).unwrap().clone();
        assert_eq!(deps.status, StepStatus::Failed, "{report}");
        assert_eq!(
            report.step(StepName::Format).unwrap().status,
            StepStatus::Skipped(Skip::NotRun)
        );
        deps.detail
    };

    let repo = Repo::new("rust", &[]);
    repo.write(
        ".cargo/config.toml",
        "[source.crates-io]\nreplace-with = \"evil\"\n",
    );
    assert!(failed(&repo).contains("protected build configuration: .cargo/config.toml"));

    let repo = Repo::new("rust", &[]);
    repo.replace(
        "Cargo.toml",
        "[dependencies]\n",
        "[dependencies]\nleft-pad = \"1\"\n",
    );
    assert!(failed(&repo).contains("Cargo.toml changed but no lockfile did"));

    let repo = Repo::new("rust", &[]);
    let lock = repo.read("Cargo.lock");
    repo.write(
        "Cargo.lock",
        &format!(
            "{lock}\n[[package]]\nname = \"left-pad\"\nversion = \"1.0.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
             checksum = \"{}\"\n",
            "0".repeat(64)
        ),
    );
    let detail = failed(&repo);
    assert!(
        detail.contains("Cargo.lock") && detail.contains("left-pad"),
        "{detail}"
    );
}

#[test]
fn steps_time_out_and_can_be_cancelled() {
    let slow = profile(Language::Rust, "[gate]\nformat = [\"sleep\", \"30\"]\n");
    let repo = Repo::new("rust", &[]);

    let started = Instant::now();
    let report = repo.gate_with(
        &slow,
        None,
        Duration::from_secs(1),
        CancelToken::new(),
        vec![],
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    let format = report.step(StepName::Format).unwrap();
    assert_eq!(format.status, StepStatus::Failed);
    assert!(format.detail.starts_with("timed out"), "{}", format.detail);

    let cancel = CancelToken::new();
    let later = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        later.cancel();
    });
    let started = Instant::now();
    let report = repo.gate_with(&slow, None, Duration::from_secs(60), cancel, vec![]);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(report.cancelled() && !report.passed(), "{report}");
    assert_eq!(
        report.step(StepName::Sonar).unwrap().status,
        StepStatus::Skipped(Skip::Cancelled)
    );
    assert!(report.to_string().starts_with("Gate cancelled."));
}

#[test]
fn commands_get_a_clean_environment_plus_the_request_env() {
    // `cargo test` sets CARGO_PKG_NAME for this process; the step must not
    // see it, but must see what the request adds.
    let check = profile(
        Language::Rust,
        "[gate]\nformat = [\"sh\", \"-c\", \"test -z \\\"$CARGO_PKG_NAME\\\" && test \\\"$GATE_X\\\" = 1 && test -n \\\"$PATH\\\"\"]\nlint = false\ntest = [\"true\"]\ncoverage = false\nrepro_test = false\n",
    );
    let repo = Repo::new("rust", &[]);
    let report = repo.gate_with(
        &check,
        None,
        Duration::from_secs(60),
        CancelToken::new(),
        vec![("GATE_X".into(), "1".into())],
    );
    assert_eq!(
        report.step(StepName::Format).unwrap().status,
        StepStatus::Passed,
        "{report}"
    );
    assert_eq!(
        report.step(StepName::Lint).unwrap().status,
        StepStatus::Skipped(Skip::DisabledByRepo)
    );
    assert!(report.passed(), "{report}");
}

#[test]
fn work_dir_inside_the_repo_is_refused() {
    let repo = Repo::new("rust", &[]);
    let rust = profile(Language::Rust, "");
    let allowed = Default::default();
    let dir = repo.dir();
    let inside = dir.join("work");
    let err = aegis_gate::run_gate(&aegis_gate::GateRequest {
        repo: &dir,
        base: "main",
        profile: &rust,
        allowed: &allowed,
        repro: None,
        work_dir: &inside,
        env: vec![],
        step_timeout: Duration::from_secs(1),
        cancel: CancelToken::new(),
        sandbox: None,
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("work_dir must be outside the repo"),
        "{err}"
    );
}

fn git(repo: &Repo, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.dir())
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn with_left_pad(repo: &Repo, path: &str) {
    let lock = repo.read("Cargo.lock");
    repo.write(
        path,
        &format!(
            "{lock}\n[[package]]\nname = \"left-pad\"\nversion = \"1.0.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
             checksum = \"{}\"\n",
            "0".repeat(64)
        ),
    );
}

#[test]
fn hidden_lockfile_changes_are_still_seen() {
    let rust = profile(Language::Rust, FAKE_COVERAGE);
    let deps_failed = |repo: &Repo| {
        let report = repo.gate(&rust, None);
        let deps = report.step(StepName::Dependencies).unwrap();
        assert_eq!(deps.status, StepStatus::Failed, "{report}");
        deps.detail.clone()
    };

    // Flags on the repo's own index do not hide a change.
    let repo = Repo::new("rust", &[]);
    with_left_pad(&repo, "Cargo.lock");
    git(&repo, &["update-index", "--skip-worktree", "Cargo.lock"]);
    assert!(deps_failed(&repo).contains("left-pad"));

    // Nor does an ignore rule over a whole directory.
    let repo = Repo::new("rust", &[]);
    repo.write(".git/info/exclude", "vendor/\n");
    with_left_pad(&repo, "vendor/Cargo.lock");
    assert!(deps_failed(&repo).contains("git ignores files the build reads: vendor/Cargo.lock"));

    // A nested repository is refused outright.
    let repo = Repo::new("rust", &[]);
    let nested = repo.dir().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ],
    ] {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&nested)
            .args(args)
            .status()
            .unwrap();
        assert!(ok.success());
    }
    assert!(deps_failed(&repo).contains("submodule or nested repository: nested"));
}

#[test]
fn a_step_that_rewrites_the_patch_fails_the_gate() {
    let rewrites = profile(
        Language::Rust,
        "[gate]\nformat = false\nlint = [\"sh\", \"-c\", \"echo '// sneaked in' >> src/lib.rs\"]\ntest = [\"true\"]\ncoverage = false\nrepro_test = false\n",
    );
    let repo = Repo::new("rust", &[]);
    let report = repo.gate(&rewrites, None);
    let integrity = report.step(StepName::Integrity).unwrap();
    assert_eq!(integrity.status, StepStatus::Failed, "{report}");
    assert!(
        integrity.detail.contains("src/lib.rs"),
        "{}",
        integrity.detail
    );
    assert!(!report.passed());
}

#[test]
fn a_crashing_test_counts_as_a_new_failure() {
    if !ready() {
        return;
    }
    let repo = Repo::new("rust", &[BUG]);
    repo.write(
        "tests/crash.rs",
        "#[test]\nfn crashes() {\n    std::process::abort();\n}\n",
    );

    let report = repo.gate(&profile(Language::Rust, FAKE_COVERAGE), None);
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Failed, "{report}");
    assert!(
        test.detail.contains("crash::<did not finish>"),
        "{}",
        test.detail
    );
}

#[test]
fn a_reproduction_test_must_fail_as_a_test_on_the_base() {
    if !ready() {
        return;
    }
    // The test needs a function the base does not have: it fails to build
    // there, which shows nothing about the issue.
    let repo = Repo::new("rust", &[]);
    repo.replace(
        "src/lib.rs",
        "pub type Total = u32;",
        "pub type Total = u32;\n\npub fn zero() -> Total {\n    0\n}",
    );
    repo.write(
        "tests/fourth_item.rs",
        "#[test]\nfn fourth_item_is_refused() {\n    assert_eq!(fixture_rust::zero(), 0);\n}\n",
    );

    let report = repo.gate(&profile(Language::Rust, FAKE_COVERAGE), Some(&repro()));
    let step = report.step(StepName::ReproTest).unwrap();
    assert_eq!(step.status, StepStatus::Failed, "{report}");
    assert!(
        step.detail
            .contains("did not run as a failing test on the base"),
        "{}",
        step.detail
    );
}

#[test]
fn repro_files_must_be_regular_files_in_the_repo() {
    let repo = Repo::new("rust", &[]);
    let rust = profile(
        Language::Rust,
        "[gate]\nformat = false\nlint = false\nrepro_test = [\"true\", \"{test}\"]\n",
    );
    let allowed = Default::default();
    let dir = repo.dir();
    let work = repo.work();
    let run = |files: Vec<String>| {
        aegis_gate::run_gate(&aegis_gate::GateRequest {
            repo: &dir,
            base: "main",
            profile: &rust,
            allowed: &allowed,
            repro: Some(&Repro {
                test: "t".into(),
                files,
            }),
            work_dir: &work,
            env: vec![],
            step_timeout: Duration::from_secs(60),
            cancel: CancelToken::new(),
            sandbox: None,
        })
    };
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/hosts", repo.dir().join("link.rs")).unwrap();
    for bad in ["../outside.rs", "/etc/hosts", "link.rs"] {
        let err = run(vec![bad.into()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("must be a regular file inside the repo"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn the_log_keeps_the_order_of_stdout_and_stderr() {
    let mixed = profile(
        Language::Rust,
        "[gate]\nformat = [\"sh\", \"-c\", \"for i in 1 2 3 4 5; do echo out$i; echo err$i >&2; done; exit 1\"]\n",
    );
    let repo = Repo::new("rust", &[]);
    let report = repo.gate(&mixed, None);
    let format = report.step(StepName::Format).unwrap();
    let expected: Vec<String> = (1..=5)
        .flat_map(|i| [format!("out{i}"), format!("err{i}")])
        .collect();
    let lines: Vec<&str> = format.detail.lines().skip(2).collect();
    assert_eq!(lines, expected, "{}", format.detail);
}
