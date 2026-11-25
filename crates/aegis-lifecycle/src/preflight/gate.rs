//! The baseline gate of the pre-flight check: the repo's own gate on its
//! base, with no patch.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use aegis_gate::{
    AuditPolicy, CancelToken, GateRequest, Hardening, Sandbox, ScopePolicy, SecretPolicy, StepName,
    StepReport, StepStatus, WORKDIR, run_gate,
};
use aegis_lang::RepoProfiles;

use super::{CheckItem, CheckStatus, PreflightReport};
use crate::RunConfig;

pub(super) async fn run_baseline_gate(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    use_sandbox: bool,
    fetch: bool,
    report: &mut PreflightReport,
) {
    let root = profiles.root();
    // One per repo: repos of a tenant share its state dir.
    let temp_work_dir = cfg
        .run
        .state_dir
        .join("preflight-check")
        .join(&cfg.repo.key);
    let _ = std::fs::remove_dir_all(&temp_work_dir);
    if let Err(e) = std::fs::create_dir_all(&temp_work_dir) {
        report.add(CheckItem {
            category: "Gate",
            name: "Baseline Execution".into(),
            status: CheckStatus::Fail,
            detail: format!("Failed to create gate working directory: {e}"),
            is_blocking: true,
        });
        return;
    }

    // With `fetch`, the job's own sandbox, fetch and build cache (#599): the
    // base's dependencies come through the egress proxy into the repo's
    // cache, which its jobs then use.
    let (sandbox, fetch_step, build_cache) = match (use_sandbox, fetch) {
        (false, _) => (None, None, None),
        (true, false) => (Some(build_sandbox(cfg, root, &temp_work_dir)), None, None),
        (true, true) => {
            let part = &profiles.parts()[0];
            match crate::run::gate_sandbox(cfg, part) {
                Ok(sandbox) => (
                    sandbox,
                    crate::run::gate_fetch(cfg, root),
                    crate::run::gate_build_cache(cfg, part),
                ),
                Err(e) => {
                    report.add(CheckItem {
                        category: "Gate",
                        name: "Baseline Execution".into(),
                        status: CheckStatus::Fail,
                        detail: format!("The sandbox cannot be set up: {e}"),
                        is_blocking: true,
                    });
                    return;
                }
            }
        }
    };

    let repo_path = cfg.repo.path.clone();
    let base_rev = cfg.repo.base.clone();
    let root_profile = root.clone();
    let sidecars = if use_sandbox {
        cfg.repo.gate_sidecars(&cfg.run)
    } else {
        None
    };
    let declared: Vec<String> = cfg.repo.sidecars.iter().map(|s| s.name.clone()).collect();
    let work_dir = temp_work_dir.clone();
    let timeout = Duration::from_secs(cfg.run.step_timeout_s);
    let allowed = BTreeSet::new();

    let start_gate = Instant::now();
    let gate_result = tokio::task::spawn_blocking(move || {
        let gate_request = GateRequest {
            repo: &repo_path,
            base: &base_rev,
            profile: &root_profile,
            allowed: &allowed,
            repro: None,
            work_dir: &work_dir,
            env: vec![],
            step_timeout: timeout,
            cancel: CancelToken::new(),
            sandbox,
            fetch: fetch_step,
            build_cache,
            allowed_tests: vec![],
            reference_groups: vec![],
            sonar: None,
            audit: AuditPolicy::default(),
            scope: ScopePolicy::default(),
            secrets: SecretPolicy::default(),
            parts: vec![],
            sidecars,
            dependency_policy: aegis_gate::DependencyPolicy::Locked,
            test_mode: aegis_gate::TestMode::Reproduce,
        };
        run_gate(&gate_request)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))
    .and_then(|r| r.map_err(|e| format!("Gate error: {e}")));

    let elapsed = start_gate.elapsed();

    match gate_result {
        Ok(gate_report) => {
            for step in &gate_report.steps {
                match step.name {
                    StepName::Format => report.add(style_item("Baseline Format", step)),
                    StepName::Lint => report.add(style_item("Baseline Lint", step)),
                    _ => {}
                }
            }
            let test = gate_report.steps.iter().find(|s| s.name == StepName::Test);
            report.add(test_item(test, elapsed));
            // Scan logs for network/DB connection failure indicators
            let detected_network_err = test
                .filter(|t| t.status != StepStatus::Passed || t.fails_on_base)
                .is_some_and(|t| {
                    t.logs.iter().any(|log| {
                        std::fs::read_to_string(log).is_ok_and(|c| scan_for_network_issues(&c))
                    })
                });

            // Report external network/database dependency detection
            if detected_network_err {
                report.add(CheckItem {
                    category: "Isolation",
                    name: "Network & DB Isolation".into(),
                    status: CheckStatus::Fail,
                    detail: network_hint(&declared),
                    is_blocking: true,
                });
            } else {
                report.add(CheckItem {
                    category: "Isolation",
                    name: "Network & DB Isolation".into(),
                    status: CheckStatus::Pass,
                    detail:
                        "Zero blocked network/database connections detected in air-gapped sandbox"
                            .into(),
                    is_blocking: false,
                });
            }

            // SLA duration check
            let max_sla = Duration::from_secs(cfg.run.step_timeout_s);
            if elapsed > max_sla {
                report.add(CheckItem {
                    category: "SLA",
                    name: "Execution Duration".into(),
                    status: CheckStatus::Fail,
                    detail: format!(
                        "Baseline gate took {:.1}s, exceeding SLA timeout of {:.1}s",
                        elapsed.as_secs_f64(),
                        max_sla.as_secs_f64()
                    ),
                    is_blocking: true,
                });
            } else if elapsed > Duration::from_secs(600) {
                report.add(CheckItem {
                    category: "SLA",
                    name: "Execution Duration".into(),
                    status: CheckStatus::Warn,
                    detail: format!(
                        "Baseline gate took {:.1}s (> 10 minutes); consider optimizing tests for pilot",
                        elapsed.as_secs_f64()
                    ),
                    is_blocking: false,
                });
            } else {
                report.add(CheckItem {
                    category: "SLA",
                    name: "Execution Duration".into(),
                    status: CheckStatus::Pass,
                    detail: format!(
                        "Total execution time {:.1}s satisfies SLA",
                        elapsed.as_secs_f64()
                    ),
                    is_blocking: false,
                });
            }
        }
        Err(e) => {
            report.add(CheckItem {
                category: "Gate",
                name: "Baseline Gate".into(),
                status: CheckStatus::Fail,
                detail: format!("Gate execution error: {e}"),
                is_blocking: true,
            });
        }
    }

    // Clean up temporary work directory
    let _ = std::fs::remove_dir_all(&temp_work_dir);
}

/// The first lines of a step's detail, on one line.
fn summary(detail: &str) -> String {
    detail
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Format or lint on the base. The gate judges a patch by the findings it
/// adds, so a base that fails is a warning, not a blocker; but it is not a
/// pass either, though the gate says passed (#487).
fn style_item(name: &str, step: &StepReport) -> CheckItem {
    let took = step.duration.as_secs_f64();
    let (status, detail) = match &step.status {
        StepStatus::Passed if step.fails_on_base => (
            CheckStatus::Warn,
            format!(
                "Fails on the base branch ({took:.1}s); patches are judged by the findings they add"
            ),
        ),
        StepStatus::Passed => (CheckStatus::Pass, format!("Passed ({took:.1}s)")),
        StepStatus::Skipped(why) => (CheckStatus::Skip, format!("Skipped: {why:?}")),
        StepStatus::Failed => (
            CheckStatus::Warn,
            format!("Failed ({took:.1}s): {}", summary(&step.detail)),
        ),
    };
    CheckItem {
        category: "Gate",
        name: name.into(),
        status,
        detail,
        is_blocking: false,
    }
}

/// The test suite on the base: it must pass there. A suite the gate lets
/// through because the base fails the same tests fails here (#487): every
/// patch would start from failing tests.
fn test_item(test: Option<&StepReport>, elapsed: Duration) -> CheckItem {
    let (status, detail) = match test {
        None => (CheckStatus::Fail, "The gate ran no test step".to_owned()),
        Some(t) if t.status == StepStatus::Passed && !t.fails_on_base => (
            CheckStatus::Pass,
            format!("Test suite passed cleanly in {:.1}s", elapsed.as_secs_f64()),
        ),
        Some(t) if t.status == StepStatus::Passed => (
            CheckStatus::Fail,
            format!("Tests fail on the base branch: {}", summary(&t.detail)),
        ),
        Some(StepReport {
            status: StepStatus::Skipped(why),
            ..
        }) => (
            CheckStatus::Fail,
            format!("The test step did not run: {why:?}"),
        ),
        Some(t) => (
            CheckStatus::Fail,
            format!("Baseline tests failed: {}", summary(&t.detail)),
        ),
    };
    CheckItem {
        category: "Gate",
        name: "Baseline Unit Tests".into(),
        status,
        detail,
        is_blocking: true,
    }
}

/// What to do about tests that could not connect: declare the services
/// they need, or check the ones declared (#490).
fn network_hint(declared: &[String]) -> String {
    if declared.is_empty() {
        "Tests could not connect to a service (PostgreSQL/Redis/MySQL or the network). \
         Declare the services they need as `[[repo.sidecars]]` (docs/runbook/repos.md)"
            .into()
    } else {
        format!(
            "Tests could not connect to a service, with sidecars {} declared. Check their \
             `test_env` and host names, or `localhost = true` for tests that use localhost",
            declared.join(", ")
        )
    }
}

/// Scans test failure output for signatures of network or database access attempts blocked by `--network none`.
fn scan_for_network_issues(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    let signatures = [
        "connection refused",
        "econnrefused",
        "dial tcp",
        "127.0.0.1:5432",
        "localhost:5432",
        "127.0.0.1:3306",
        "localhost:3306",
        "127.0.0.1:6379",
        "localhost:6379",
        "network is unreachable",
        "failed to connect to",
        "could not connect to server",
        "name or service not known",
        "getaddrinfo",
    ];
    signatures.iter().any(|&sig| lower.contains(sig))
}

/// Constructs a Sandbox instance matching the configuration for root profile.
fn build_sandbox(cfg: &RunConfig, root: &aegis_lang::ResolvedProfile, temp_dir: &Path) -> Sandbox {
    let cache_dir = temp_dir.join("cache");
    let mut mounts = Vec::new();
    for dir in root
        .profile
        .sandbox
        .cache_dirs
        .iter()
        .filter(|d| d.starts_with('/'))
    {
        let host = cache_dir.join(dir.trim_start_matches('/').replace('/', "_"));
        let _ = std::fs::create_dir_all(&host);
        mounts.push((host, dir.clone()));
    }
    for dir in &root.profile.dependencies.dirs {
        let host = cache_dir.join("deps").join(dir.replace('/', "_"));
        let _ = std::fs::create_dir_all(&host);
        mounts.push((host, format!("{WORKDIR}/{dir}")));
    }

    Sandbox {
        docker: cfg.run.docker.clone(),
        image: root.image.clone(),
        mounts,
        env: cfg.run.sandbox_env.clone().into_iter().collect(),
        network: None,
        read_only_mounts: true,
        hardening: Hardening::new(cfg.run.sandbox_limits, &root.profile.sandbox)
            .with_runtime(cfg.run.sandbox_runtime.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::super::{PreflightOptions, run_preflight};
    use super::*;

    #[test]
    fn test_scan_for_network_issues() {
        assert!(scan_for_network_issues(
            "Error: connect ECONNREFUSED 127.0.0.1:5432"
        ));
        assert!(scan_for_network_issues(
            "dial tcp 127.0.0.1:6379: connect: connection refused"
        ));
        assert!(scan_for_network_issues(
            "Fatal error: Could not connect to server at localhost:5432"
        ));
        assert!(scan_for_network_issues("Failed to connect to redis"));
        assert!(!scan_for_network_issues(
            "assertion `left == right` failed: expected 42, got 0"
        ));
        assert!(!scan_for_network_issues("test my_func ... ok"));
    }

    /// A docker for the Python profile's gate: the daemon answers, the
    /// image is there, `ruff check` runs `lint` and `pytest` runs `test`
    /// (shell), anything else passes. `run` is what `docker run` does
    /// before the command; `started` is the container's start time.
    #[cfg(unix)]
    fn gate_docker(dir: &Path, run: &str, started: &str, lint: &str, test: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("docker");
        let script = format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             version|info) echo 29.0.1; exit 0;;\n\
             image|rm) exit 0;;\n\
             inspect) case \"$3\" in *StartedAt*) {started};; *) echo false;; esac; exit 0;;\n\
             esac\n\
             {run}\n\
             case \"$*\" in\n\
             *'ruff check'*) {lint};;\n\
             *pytest*) {test};;\n\
             esac\n\
             exit 0\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// A Python repo on `main` and a configuration that checks it with
    /// `docker` in place of Docker.
    #[cfg(unix)]
    fn python_repo(dir: &Path, docker: &str) -> PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("tests")).unwrap();
        std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
        std::fs::write(
            repo.join("tests/test_calc.py"),
            "from calc import add\n\ndef test_add():\n    assert add(1, 2) == 3\n",
        )
        .unwrap();
        std::fs::write(repo.join("requirements.txt"), "pytest\n").unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let pricing = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/pricing.toml");
        let config = dir.join("check.toml");
        std::fs::write(
            &config,
            format!(
                r#"[provider]
kind = "claude_api"
api_key_env = "ANTHROPIC_API_KEY"
[models.light]
id = "claude-haiku-5-5"
[models.core]
id = "claude-sonnet-5-5"
[models.expert]
id = "claude-opus-5-5"
[run]
state_dir = "{state}"
pricing = "{pricing}"
step_timeout_s = 60
sandbox = true
docker = "{docker}"
[budget]
max_cost_usd = 1.0
[repo]
path = "{repo}"
language = "python"
base = "main"
key = "calc"
[repo.fork]
owner = "bot"
name = "calc"
token_env = "UNUSED"
[identity]
name = "aegis-bot"
email = "aegis-bot@users.noreply.github.com"
"#,
                state = dir.join("state").display(),
                pricing = pricing.display(),
                repo = repo.display(),
            ),
        )
        .unwrap();
        config
    }

    /// The report of `check-repo` on the Python repo with a docker
    /// built by [`gate_docker`].
    #[cfg(unix)]
    async fn check_with(
        name: &str,
        run: &str,
        started: &str,
        lint: &str,
        test: &str,
    ) -> PreflightReport {
        let dir = std::env::temp_dir().join(format!("aegis-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let docker = gate_docker(&dir, run, started, lint, test);
        let config_path = python_repo(&dir, &docker);
        let cfg = RunConfig::load(&config_path).unwrap();
        let mut report = PreflightReport::default();
        run_preflight(&cfg, &PreflightOptions::default(), &mut report).await;
        let _ = std::fs::remove_dir_all(&dir);
        report
    }

    fn item<'a>(report: &'a PreflightReport, name: &str) -> &'a CheckItem {
        report
            .items
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("no item {name}: {report:#?}"))
    }

    const STARTED: &str = "echo 2026-11-14T09:00:00.1Z";
    const PASSES: &str = "exit 0";
    const FAILING_TEST: &str =
        "echo 'FAILED tests/test_calc.py::test_add - assert 3 == 4'; echo '1 failed'; exit 1";

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repo_whose_gate_passes_is_ready() {
        let report = check_with("ready", "", STARTED, PASSES, PASSES).await;
        assert!(!report.has_blocking_failures(), "{report:#?}");
        assert_eq!(
            item(&report, "Baseline Unit Tests").status,
            CheckStatus::Pass
        );
        assert_eq!(item(&report, "Baseline Lint").status, CheckStatus::Pass);
    }

    /// #487: the gate lets a failure the base has through; on the base
    /// itself, that is the failure.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn tests_that_fail_on_the_base_block() {
        let report = check_with("failing-tests", "", STARTED, PASSES, FAILING_TEST).await;
        let tests = item(&report, "Baseline Unit Tests");
        assert_eq!(tests.status, CheckStatus::Fail, "{report:#?}");
        assert!(tests.detail.contains("test_add"), "{tests:?}");
        assert!(report.has_blocking_failures());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lint_that_fails_on_the_base_is_a_warning() {
        let report = check_with("failing-lint", "", STARTED, "echo 'E501 x'; exit 1", PASSES).await;
        let lint = item(&report, "Baseline Lint");
        assert_eq!(lint.status, CheckStatus::Warn, "{report:#?}");
        assert!(!report.has_blocking_failures(), "{report:#?}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn containers_that_never_start_block() {
        let refused = "echo 'docker: Error response from daemon: refused' >&2; exit 125";
        let report = check_with("refused", refused, "exit 1", PASSES, PASSES).await;
        let gate = item(&report, "Baseline Gate");
        assert_eq!(gate.status, CheckStatus::Fail, "{report:#?}");
        assert!(gate.detail.contains("did not start"), "{gate:?}");
        assert!(report.has_blocking_failures());
    }
}
