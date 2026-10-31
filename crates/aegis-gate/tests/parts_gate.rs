//! A repo of two languages (#71, ADR 0004 §1): the Rust fixture at the
//! root, the Python fixture under `py/`. The gate checks only the parts a
//! patch changes, each with its own profile in its own directory. The
//! commands are stand-ins that say where they ran, so no toolchain is
//! needed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aegis_gate::{
    AuditPolicy, CancelToken, GatePart, GateReport, GateRequest, Repro, Skip, SonarStep, StepName,
    StepStatus, run_gate,
};
use aegis_lang::{Language, ProfileOverrides, Profiles, ResolvedProfile};

/// Passes only in the root's directory.
const ROOT: &str = r#"
[gate]
format = false
lint = false
repro_test = false
test = ["sh", "-c", "test -f Cargo.toml"]
coverage = false
"#;

/// Passes only in `py/`; the reproduction test needs the fix.
const PY: &str = r#"
audit = ["sh", "-c", "pwd; echo \"audited $1\"", "sh", "{lockfile}"]
[gate]
format = false
lint = ["sh", "-c", "test -f pyproject.toml"]
repro_test = ["sh", "-c", "grep -q FIXED cart/__init__.py && test -f \"$1\"", "sh", "{test}"]
test = ["sh", "-c", "test -f pyproject.toml"]
coverage = false
"#;

fn profile(language: Language, overrides: &str) -> ResolvedProfile {
    let overrides: ProfileOverrides = toml::from_str(overrides).unwrap();
    Profiles::builtin()
        .unwrap()
        .get(language)
        .unwrap()
        .resolve(&overrides)
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[path = "../../../tests/fixture_copy.rs"]
mod fixture_copy;
use fixture_copy::copy_fixture as copy_dir;

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Repo {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-gate-parts-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let repo = Self(root);
        copy_dir(&fixtures.join("rust"), &repo.dir());
        copy_dir(&fixtures.join("python"), &repo.dir().join("py"));
        repo.write("py/requirements.txt", "attrs==23.1.0\n");
        repo.write(".gitignore", "target/\n__pycache__/\n");
        git(&repo.dir(), &["init", "-q", "-b", "main"]);
        repo.commit("base");
        repo
    }

    fn dir(&self) -> PathBuf {
        self.0.join("repo")
    }

    fn write(&self, file: &str, text: &str) {
        let path = self.dir().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn append(&self, file: &str, text: &str) {
        let path = self.dir().join(file);
        let before = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, before + text).unwrap();
    }

    fn commit(&self, message: &str) {
        git(&self.dir(), &["add", "-A"]);
        git(
            &self.dir(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                message,
            ],
        );
    }

    fn gate(&self, root: &str, py: &str, repro: Option<&Repro>, allowed: &[&str]) -> GateReport {
        self.gate_with(root, py, repro, allowed, None, vec![])
    }

    fn gate_with(
        &self,
        root: &str,
        py: &str,
        repro: Option<&Repro>,
        allowed: &[&str],
        sonar: Option<SonarStep>,
        env: Vec<(String, String)>,
    ) -> GateReport {
        let (root, py) = (profile(Language::Rust, root), profile(Language::Python, py));
        let allowed: BTreeSet<String> = allowed.iter().map(|a| (*a).to_owned()).collect();
        let (dir, work) = (self.dir(), self.0.join("work"));
        run_gate(&GateRequest {
            repo: &dir,
            base: "main",
            profile: &root,
            allowed: &allowed,
            repro,
            work_dir: &work,
            env,
            step_timeout: Duration::from_secs(60),
            cancel: CancelToken::new(),
            sandbox: None,
            fetch: None,
            build_cache: None,
            allowed_tests: vec![],
            reference_groups: vec![],
            sonar,
            audit: AuditPolicy::default(),
            parts: vec![GatePart {
                dir: "py".into(),
                profile: &py,
                sandbox: None,
                fetch: None,
                build_cache: None,
            }],
        })
        .unwrap()
    }
}

fn logs(report: &GateReport, step: StepName) -> Vec<String> {
    report
        .step(step)
        .unwrap()
        .logs
        .iter()
        .map(|l| l.display().to_string())
        .collect()
}

#[test]
fn a_patch_in_one_directory_is_checked_only_by_its_profile() {
    let repo = Repo::new();
    repo.append("py/cart/__init__.py", "# FIXED\n");
    // The root's tests would fail: they must not run.
    let report = repo.gate(&ROOT.replace("test -f Cargo.toml", "false"), PY, None, &[]);
    assert!(report.passed(), "{report}");
    let test = logs(&report, StepName::Test);
    assert_eq!(test.len(), 1, "{test:?}");
    assert!(test[0].contains("/logs/py/"), "{test:?}");
    assert_eq!(
        report.step(StepName::Lint).unwrap().status,
        StepStatus::Passed
    );
}

#[test]
fn each_changed_part_runs_its_steps_in_its_own_directory() {
    let repo = Repo::new();
    repo.append("src/lib.rs", "// touched\n");
    repo.append("py/cart/__init__.py", "# FIXED\n");
    let report = repo.gate(ROOT, PY, None, &[]);
    assert!(report.passed(), "{report}");
    assert_eq!(logs(&report, StepName::Test).len(), 2);
    // Lint is off at the root and runs in `py/`.
    assert_eq!(
        report.step(StepName::Lint).unwrap().status,
        StepStatus::Passed
    );
    // The tree sent is the whole patch.
    let tree = Command::new("git")
        .arg("-C")
        .arg(repo.dir())
        .args(["ls-tree", "-r", "--name-only", &report.tree])
        .output()
        .unwrap();
    let files = String::from_utf8_lossy(&tree.stdout);
    assert!(files.contains("src/lib.rs") && files.contains("py/cart/__init__.py"));
}

#[test]
fn a_failing_part_is_named_and_the_others_are_not_run() {
    let repo = Repo::new();
    repo.append("src/lib.rs", "// touched\n");
    repo.append("py/cart/__init__.py", "# FIXED\n");
    let report = repo.gate(&ROOT.replace("test -f Cargo.toml", "false"), PY, None, &[]);
    assert!(!report.passed());
    let test = report.step(StepName::Test).unwrap();
    assert_eq!(test.status, StepStatus::Failed, "{report}");
    assert!(
        test.detail.contains("the repo root (Rust)"),
        "{}",
        test.detail
    );
    // The root goes first; `py/` is not checked after it failed.
    assert_eq!(
        report.step(StepName::Lint).unwrap().status,
        StepStatus::Skipped(Skip::NotRun)
    );
}

#[test]
fn the_reproduction_test_runs_in_the_part_that_holds_it() {
    let repo = Repo::new();
    repo.append("py/cart/__init__.py", "# FIXED\n");
    repo.write("py/tests/test_fourth.py", "def test_fourth():\n    pass\n");
    let repro = Repro {
        test: "py/tests/test_fourth.py".into(),
        files: vec!["py/tests/test_fourth.py".into()],
    };
    let report = repo.gate(ROOT, PY, Some(&repro), &[]);
    assert_eq!(
        report.step(StepName::ReproTest).unwrap().status,
        StepStatus::Passed,
        "{report}"
    );
}

#[test]
fn a_part_s_lockfile_is_checked_by_its_profile() {
    let repo = Repo::new();
    repo.append("py/requirements.txt", "requests==2.31.0\n");
    let report = repo.gate(ROOT, PY, None, &[]);
    let deps = report.step(StepName::Dependencies).unwrap();
    assert_eq!(deps.status, StepStatus::Failed, "{report}");
    assert!(
        deps.detail.contains("py/requirements.txt") && deps.detail.contains("requests"),
        "{}",
        deps.detail
    );
}

#[test]
fn a_part_s_changed_lockfile_is_audited_in_its_directory() {
    let repo = Repo::new();
    repo.append("py/requirements.txt", "requests==2.31.0\n");
    let report = repo.gate(ROOT, PY, None, &["requests"]);
    let deps = report.step(StepName::Dependencies).unwrap();
    assert_eq!(deps.status, StepStatus::Passed, "{report}");
    let log = std::fs::read_to_string(&deps.logs[0]).unwrap();
    assert!(log.contains("/repo/py\n"), "{log}");
    assert!(log.contains("audited requirements.txt"), "{log}");
}

#[test]
fn a_build_file_the_root_reads_fails_in_a_part_that_does_not() {
    // `py/native/Cargo.lock` is Rust's, which the root's workspace may
    // build; the Python part cannot check it (#200).
    for file in ["py/native/Cargo.lock", "py/native/.cargo/config.toml"] {
        let repo = Repo::new();
        repo.write(file, "# new\n");
        let report = repo.gate(ROOT, PY, None, &[]);
        let deps = report.step(StepName::Dependencies).unwrap();
        assert_eq!(deps.status, StepStatus::Failed, "{report}");
        assert!(
            deps.detail.contains(&format!("{file} (Rust)"))
                && deps.detail.contains("part of its own"),
            "{}",
            deps.detail
        );
    }
}

#[test]
fn a_part_s_build_file_outside_it_is_left_to_the_root() {
    // A `requirements.txt` at the root is not in `py/`, and the root's
    // Rust profile does not read it: nothing to check.
    let repo = Repo::new();
    repo.write("requirements.txt", "requests==2.31.0\n");
    let report = repo.gate(ROOT, PY, None, &[]);
    assert!(report.passed(), "{report}");
    assert_eq!(
        report.step(StepName::Dependencies).unwrap().status,
        StepStatus::Skipped(Skip::NoDependencyChange)
    );
}

/// Records the analysis settings and which coverage reports exist when it
/// runs, then leaves a task for the gate to wait for.
const FAKE_SCANNER: &str = r#"
cp .aegis-gate-sonar/project.properties "$FAKE_RECORD/settings"
ls lcov.info py/coverage.xml > "$FAKE_RECORD/reports" 2>&1
mkdir -p .aegis-gate-sonar/work
printf 'projectKey=proj\nceTaskId=T1\n' > .aegis-gate-sonar/work/report-task.txt
"#;

/// A Sonar server whose analysis passes.
fn sonar_server(rt: &tokio::runtime::Runtime) -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    rt.block_on(async {
        let server = MockServer::start().await;
        let task = serde_json::json!({"task": {"id": "T1", "status": "SUCCESS",
            "componentKey": "proj", "analysisId": "A1"}});
        let gate = serde_json::json!({"projectStatus": {"status": "OK", "conditions": []}});
        let issues = serde_json::json!({"paging": {"pageIndex": 1, "pageSize": 100, "total": 0},
            "issues": []});
        for (at, body) in [
            ("/api/ce/task", task),
            ("/api/qualitygates/project_status", gate),
            ("/api/issues/search", issues),
        ] {
            Mock::given(method("GET"))
                .and(path(at))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        server
    })
}

#[test]
fn sonar_analyses_the_repo_once_with_every_part_s_reports() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let server = sonar_server(&rt);
    let repo = Repo::new();
    let (scanner, record) = (repo.0.join("scanner.sh"), repo.0.join("record"));
    std::fs::write(&scanner, FAKE_SCANNER).unwrap();
    std::fs::create_dir_all(&record).unwrap();
    repo.append("src/lib.rs", "// touched\n");
    repo.append("py/cart/__init__.py", "# FIXED\n");
    // Each part's coverage step writes its own report.
    let root = ROOT.replace(
        "coverage = false",
        "coverage = [\"sh\", \"-c\", \"echo TN: > lcov.info\"]",
    ) + "[coverage]\nformat = \"lcov\"\npath = \"lcov.info\"\n";
    let py = PY.replace(
        "coverage = false",
        "coverage = [\"sh\", \"-c\", \"echo '<coverage/>' > coverage.xml\"]",
    ) + "[coverage]\nformat = \"cobertura\"\npath = \"coverage.xml\"\n";
    let step = SonarStep {
        url: server.uri(),
        token: "squ_test".into(),
        organization: None,
        project_key: "proj".into(),
        pull_request: false,
        scanner: vec!["sh".into(), scanner.display().to_string()],
        image: None,
        network: "bridge".into(),
        cache: None,
        properties: Default::default(),
        wait: Duration::from_secs(5),
        poll: Duration::from_millis(20),
    };
    let env = vec![("FAKE_RECORD".to_owned(), record.display().to_string())];

    let report = repo.gate_with(&root, &py, None, &[], Some(step), env);

    let sonar = report.step(StepName::Sonar).unwrap();
    assert_eq!(sonar.status, StepStatus::Passed, "{report}");
    let settings = std::fs::read_to_string(record.join("settings")).unwrap();
    assert!(
        settings.contains("sonar.rust.lcov.reportPaths=lcov.info\n"),
        "{settings}"
    );
    assert!(
        settings.contains("sonar.python.coverage.reportPaths=py/coverage.xml\n"),
        "{settings}"
    );
    // It ran after both parts' coverage, so both reports were there.
    let reports = std::fs::read_to_string(record.join("reports")).unwrap();
    assert_eq!(reports, "lcov.info\npy/coverage.xml\n");
    assert_eq!(sonar.logs.len(), 1, "one analysis: {:?}", sonar.logs);
}
