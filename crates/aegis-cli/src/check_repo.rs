//! Pre-flight repository verification (`aegis check-repo`, #450).
//!
//! Validates that a target repository and its environment satisfy the technical
//! prerequisites for Aegis before onboarding into a live team pilot (#158):
//! 1. RunConfig, RepoConfig, and Language Profile resolution.
//! 2. Git repository state, base branch, and clean working directory.
//! 3. Manifest and lockfile presence (critical for air-gapped reproducibility).
//! 4. Docker daemon accessibility and sandbox container image availability.
//! 5. Pre-gate fetch configuration and private registry hosts.
//! 6. Baseline gate execution in Docker sandbox (`--network none`):
//!    - Format and lint checks
//!    - Baseline unit test suite passes with zero existing regressions
//!    - Execution duration complies with SLA timeout
//!    - Detection of external DB / network dependencies blocked by isolation.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use aegis_gate::{
    AuditPolicy, CancelToken, GateRequest, Hardening, Sandbox, ScopePolicy, StepName, StepReport,
    StepStatus, WORKDIR, run_gate,
};
use aegis_lang::{Language, Profiles, RepoProfiles};
use aegis_lifecycle::{RepoConfig, RunConfig};

/// Status of an individual pre-flight verification item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Warn,
    Skip,
}

impl CheckStatus {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Pass => " [PASS] ",
            Self::Fail => " [FAIL] ",
            Self::Warn => " [WARN] ",
            Self::Skip => " [SKIP] ",
        }
    }
}

/// A recorded check result.
#[derive(Debug, Clone)]
pub struct CheckItem {
    pub category: &'static str,
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub is_blocking: bool,
}

/// Aggregated report of the repository pre-flight check.
#[derive(Debug, Default)]
pub struct PreflightReport {
    pub repo_key: String,
    pub language: String,
    pub items: Vec<CheckItem>,
    pub total_duration: Duration,
}

impl PreflightReport {
    pub fn add(&mut self, item: CheckItem) {
        self.items.push(item);
    }

    pub fn has_blocking_failures(&self) -> bool {
        self.items
            .iter()
            .any(|i| i.status == CheckStatus::Fail && i.is_blocking)
    }

    /// Renders a human-readable summary table to stdout.
    pub fn print_summary(&self) {
        println!();
        println!(
            "================================================================================"
        );
        println!(
            " AEGIS REPO PRE-FLIGHT CHECK REPORT: {} ({})",
            self.repo_key, self.language
        );
        println!(
            "================================================================================"
        );
        for item in &self.items {
            let badge = item.status.badge();
            let blocking = if item.is_blocking && item.status == CheckStatus::Fail {
                " [BLOCKING]"
            } else {
                ""
            };
            println!(
                "{badge} [{:<9}] {:<22} : {}{}",
                item.category, item.name, item.detail, blocking
            );
        }
        println!(
            "--------------------------------------------------------------------------------"
        );
        let passed = self
            .items
            .iter()
            .filter(|i| i.status == CheckStatus::Pass)
            .count();
        let failed = self
            .items
            .iter()
            .filter(|i| i.status == CheckStatus::Fail)
            .count();
        let warnings = self
            .items
            .iter()
            .filter(|i| i.status == CheckStatus::Warn)
            .count();

        println!(
            "Summary: {} passed, {} failed, {} warnings (Total duration: {:.1}s)",
            passed,
            failed,
            warnings,
            self.total_duration.as_secs_f64()
        );

        if self.has_blocking_failures() {
            println!();
            println!("RESULT: NOT READY FOR PILOT (Blocking issues detected)");
            println!("Remediation recommendations:");
            for item in &self.items {
                if item.status == CheckStatus::Fail && item.is_blocking {
                    println!("  - {}: {}", item.name, item.detail);
                }
            }
        } else if warnings > 0 {
            println!();
            println!("RESULT: READY FOR PILOT WITH WARNINGS (Review items marked with [WARN])");
        } else {
            println!();
            println!("RESULT: READY FOR ONBOARDING & PILOT (All technical pre-requisites met)");
        }
        println!(
            "================================================================================"
        );
        println!();
    }
}

/// Options passed from CLI arguments.
#[derive(Debug, Clone, Default)]
pub struct CheckRepoOptions {
    pub config_path: PathBuf,
    pub no_sandbox: bool,
    pub skip_baseline_tests: bool,
}

/// Executes all repository pre-flight checks and outputs the report.
pub async fn execute_check_repo(opts: &CheckRepoOptions) -> Result<bool, String> {
    let report = check_repo(opts).await;
    report.print_summary();
    Ok(!report.has_blocking_failures())
}

/// Runs every check; a blocking failure early on stops the rest.
async fn check_repo(opts: &CheckRepoOptions) -> PreflightReport {
    let start_time = Instant::now();
    let mut report = PreflightReport::default();
    run_checks(opts, &mut report).await;
    report.total_duration = start_time.elapsed();
    report
}

async fn run_checks(opts: &CheckRepoOptions, report: &mut PreflightReport) {
    // 1. Config loading & profile resolution
    let run_config = match RunConfig::load(&opts.config_path) {
        Ok(c) => {
            report.add(CheckItem {
                category: "Config",
                name: "RunConfig".into(),
                status: CheckStatus::Pass,
                detail: format!("Loaded successfully from {}", opts.config_path.display()),
                is_blocking: true,
            });
            c
        }
        Err(e) => {
            report.add(CheckItem {
                category: "Config",
                name: "RunConfig".into(),
                status: CheckStatus::Fail,
                detail: format!("Failed to parse config: {e}"),
                is_blocking: true,
            });
            return;
        }
    };

    report.repo_key = run_config.repo.key.clone();
    report.language = run_config.repo.language.to_string();

    let builtin_profiles = match Profiles::builtin() {
        Ok(p) => p,
        Err(e) => {
            report.add(CheckItem {
                category: "Profile",
                name: "Language Profiles".into(),
                status: CheckStatus::Fail,
                detail: format!("Failed to load builtin language profiles: {e}"),
                is_blocking: true,
            });
            return;
        }
    };

    let repo_profiles = match run_config.repo.profiles(&builtin_profiles) {
        Ok(p) => {
            report.add(CheckItem {
                category: "Profile",
                name: "Profile Resolution".into(),
                status: CheckStatus::Pass,
                detail: format!(
                    "Resolved profile for '{}' (root: {}, parts: {})",
                    run_config.repo.language,
                    p.root().profile.language,
                    p.parts().len()
                ),
                is_blocking: true,
            });
            p
        }
        Err(e) => {
            report.add(CheckItem {
                category: "Profile",
                name: "Profile Resolution".into(),
                status: CheckStatus::Fail,
                detail: format!("Failed to resolve profile overrides: {e}"),
                is_blocking: true,
            });
            return;
        }
    };

    // 2. Git repository verification
    check_git_repo(&run_config.repo, report);

    // 3. Lockfile and dependency manifest presence
    check_lockfile(&run_config.repo, report);

    // 4. Docker daemon and sandbox image availability
    let docker_available = if !opts.no_sandbox && run_config.run.sandbox {
        check_docker_environment(&run_config, &repo_profiles, report)
    } else {
        report.add(CheckItem {
            category: "Sandbox",
            name: "Docker Sandbox".into(),
            status: CheckStatus::Skip,
            detail: "Sandbox execution disabled (running in host mode)".into(),
            is_blocking: false,
        });
        false
    };

    // 5. Fetch hosts verification
    check_fetch_hosts(&repo_profiles, report);

    // 6. Baseline build and unit test execution in sandbox
    if opts.skip_baseline_tests {
        report.add(CheckItem {
            category: "Gate",
            name: "Baseline Gate".into(),
            status: CheckStatus::Skip,
            detail: "Skipped as requested via --skip-tests".into(),
            is_blocking: false,
        });
    } else {
        run_baseline_gate(
            &run_config,
            &repo_profiles,
            docker_available && !opts.no_sandbox,
            report,
        )
        .await;
    }
}

/// Checks that repo.path is a valid git repository and checks working tree status.
fn check_git_repo(repo: &RepoConfig, report: &mut PreflightReport) {
    if !repo.path.exists() {
        report.add(CheckItem {
            category: "Git",
            name: "Repository Path".into(),
            status: CheckStatus::Fail,
            detail: format!("Directory does not exist: {}", repo.path.display()),
            is_blocking: true,
        });
        return;
    }

    let git_dir = repo.path.join(".git");
    if !git_dir.exists() {
        report.add(CheckItem {
            category: "Git",
            name: "Git Repository".into(),
            status: CheckStatus::Fail,
            detail: format!(
                "{} is not a git repository (missing .git)",
                repo.path.display()
            ),
            is_blocking: true,
        });
        return;
    }

    // Check base branch resolution using git rev-parse
    let rev_check = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("rev-parse")
        .arg("--verify")
        .arg(&repo.base)
        .output();

    match rev_check {
        Ok(out) if out.status.success() => {
            let commit_sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let short_sha = if commit_sha.len() >= 7 {
                &commit_sha[..7]
            } else {
                &commit_sha
            };
            report.add(CheckItem {
                category: "Git",
                name: "Base Revision".into(),
                status: CheckStatus::Pass,
                detail: format!("Base branch '{}' verified at commit {short_sha}", repo.base),
                is_blocking: true,
            });
        }
        _ => {
            report.add(CheckItem {
                category: "Git",
                name: "Base Revision".into(),
                status: CheckStatus::Fail,
                detail: format!(
                    "Base revision '{}' could not be resolved in {}",
                    repo.base,
                    repo.path.display()
                ),
                is_blocking: true,
            });
        }
    }

    // Check for dirty working tree
    let status_check = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("status")
        .arg("--porcelain")
        .output();

    if let Ok(out) = status_check {
        let text = String::from_utf8_lossy(&out.stdout);
        let modified_lines = text.lines().count();
        if modified_lines > 0 {
            report.add(CheckItem {
                category: "Git",
                name: "Working Tree".into(),
                status: CheckStatus::Warn,
                detail: format!(
                    "Working tree contains {modified_lines} uncommitted changes; recommend a clean tree for pilot"
                ),
                is_blocking: false,
            });
        } else {
            report.add(CheckItem {
                category: "Git",
                name: "Working Tree".into(),
                status: CheckStatus::Pass,
                detail: "Clean working directory".into(),
                is_blocking: false,
            });
        }
    }
}

/// Checks that the repo has an explicit lockfile for reproducible, air-gapped builds.
fn check_lockfile(repo: &RepoConfig, report: &mut PreflightReport) {
    let expected = match repo.language {
        Language::Rust => vec!["Cargo.lock"],
        Language::Python => vec!["poetry.lock", "Pipfile.lock", "uv.lock", "requirements.txt"],
        Language::Go => vec!["go.sum"],
        Language::Java => vec!["pom.xml", "build.gradle", "gradle.lockfile"],
        Language::TypeScript => vec!["package-lock.json", "pnpm-lock.yaml", "yarn.lock"],
        Language::CSharp => vec!["packages.lock.json"],
    };

    let mut found = None;
    for file in &expected {
        let p = repo.path.join(file);
        if p.exists() {
            found = Some(file.to_string());
            break;
        }
    }

    if let Some(lockfile) = found {
        report.add(CheckItem {
            category: "Manifest",
            name: "Lockfile".into(),
            status: CheckStatus::Pass,
            detail: format!("Found dependency lockfile `{lockfile}`"),
            is_blocking: true,
        });
    } else {
        let expected_list = expected.join(", ");
        report.add(CheckItem {
            category: "Manifest",
            name: "Lockfile".into(),
            status: CheckStatus::Fail,
            detail: format!(
                "Missing lockfile (expected one of: {expected_list}). Air-gapped builds require pinned dependencies."
            ),
            is_blocking: true,
        });
    }
}

/// Checks Docker daemon connectivity and required image existence.
fn check_docker_environment(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    report: &mut PreflightReport,
) -> bool {
    let docker_cmd = &cfg.run.docker;

    // Verify Docker CLI and daemon
    let version_check = Command::new(docker_cmd).arg("info").output();
    match version_check {
        Ok(out) if out.status.success() => {
            report.add(CheckItem {
                category: "Docker",
                name: "Docker Daemon".into(),
                status: CheckStatus::Pass,
                detail: "Docker daemon reachable and responsive".into(),
                is_blocking: true,
            });
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            report.add(CheckItem {
                category: "Docker",
                name: "Docker Daemon".into(),
                status: CheckStatus::Fail,
                detail: format!("Docker daemon error: {}", stderr.trim()),
                is_blocking: true,
            });
            return false;
        }
        Err(e) => {
            report.add(CheckItem {
                category: "Docker",
                name: "Docker Daemon".into(),
                status: CheckStatus::Fail,
                detail: format!("Failed to invoke `{docker_cmd}`: {e}"),
                is_blocking: true,
            });
            return false;
        }
    }

    // Check profile sandbox image
    let root_image = &profiles.root().image;
    let inspect_check = Command::new(docker_cmd)
        .arg("image")
        .arg("inspect")
        .arg(root_image)
        .output();

    match inspect_check {
        Ok(out) if out.status.success() => {
            report.add(CheckItem {
                category: "Docker",
                name: "Sandbox Image".into(),
                status: CheckStatus::Pass,
                detail: format!("Profile image `{root_image}` exists locally"),
                is_blocking: true,
            });
            true
        }
        _ => {
            report.add(CheckItem {
                category: "Docker",
                name: "Sandbox Image".into(),
                status: CheckStatus::Fail,
                detail: format!(
                    "Sandbox image `{root_image}` not found locally. Build or pull it before running gate."
                ),
                is_blocking: true,
            });
            false
        }
    }
}

/// Checks whether fetch_hosts are configured for external registries.
fn check_fetch_hosts(profiles: &RepoProfiles, report: &mut PreflightReport) {
    let root_profile = &profiles.root().profile;
    let fetch_hosts = &root_profile.dependencies.fetch_hosts;

    if fetch_hosts.is_empty() {
        report.add(CheckItem {
            category: "Egress",
            name: "Fetch Hosts".into(),
            status: CheckStatus::Warn,
            detail: "No fetch_hosts configured. If dependencies are not pre-cached, fetch will fail in --network none."
                .into(),
            is_blocking: false,
        });
    } else {
        let hosts_str = fetch_hosts.join(", ");
        report.add(CheckItem {
            category: "Egress",
            name: "Fetch Hosts".into(),
            status: CheckStatus::Pass,
            detail: format!("Configured allow-list: [{hosts_str}]"),
            is_blocking: false,
        });
    }
}

/// Runs the baseline gate (format, lint, and workspace test suite) and checks for SLA/network issues.
async fn run_baseline_gate(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    use_sandbox: bool,
    report: &mut PreflightReport,
) {
    let root = profiles.root();
    let temp_work_dir = cfg.run.state_dir.join("preflight-check");
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

    let sandbox = if use_sandbox {
        Some(build_sandbox(cfg, root, &temp_work_dir))
    } else {
        None
    };

    let repo_path = cfg.repo.path.clone();
    let base_rev = cfg.repo.base.clone();
    let root_profile = root.clone();
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
            fetch: None,
            build_cache: None,
            allowed_tests: vec![],
            reference_groups: vec![],
            sonar: None,
            audit: AuditPolicy::default(),
            scope: ScopePolicy::default(),
            parts: vec![],
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
                    detail: "Detected connection refused / external DB dependency (PostgreSQL/Redis/MySQL). Ephemeral DB sidecar (#454) required."
                        .into(),
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
        hardening: Hardening::new(cfg.run.sandbox_limits, &root.profile.sandbox),
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn test_preflight_report_blocking_logic() {
        let mut report = PreflightReport::default();
        report.add(CheckItem {
            category: "Config",
            name: "RunConfig".into(),
            status: CheckStatus::Pass,
            detail: "ok".into(),
            is_blocking: true,
        });
        report.add(CheckItem {
            category: "Git",
            name: "Working Tree".into(),
            status: CheckStatus::Warn,
            detail: "dirty".into(),
            is_blocking: false,
        });
        assert!(!report.has_blocking_failures());

        report.add(CheckItem {
            category: "Manifest",
            name: "Lockfile".into(),
            status: CheckStatus::Fail,
            detail: "missing Cargo.lock".into(),
            is_blocking: true,
        });
        assert!(report.has_blocking_failures());
    }

    #[test]
    fn test_check_lockfile_detection() {
        let temp_dir = std::env::temp_dir().join("aegis_test_lockfile_check");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let repo = RepoConfig {
            path: temp_dir.clone(),
            language: Language::Rust,
            base: "main".into(),
            key: "test-repo".into(),
            conventions: None,
            overrides: Default::default(),
            dirs: vec![],
            allowed_packages: None,
            allowed_package_names: vec![],
            licenses: None,
            ignored_advisories: vec![],
            hidden_test: None,
            patch_scope: None,
            fork: aegis_lifecycle::ForkConfig {
                platform: aegis_lifecycle::ForkPlatform::GitHub,
                owner: "owner".into(),
                name: "repo".into(),
                api: "https://api.github.com".into(),
                token_env: "GITHUB_TOKEN".into(),
                push_to_local: None,
            },
        };

        let mut report = PreflightReport::default();
        check_lockfile(&repo, &mut report);
        assert!(report.has_blocking_failures());

        // Create Cargo.lock
        std::fs::write(temp_dir.join("Cargo.lock"), "# lockfile").unwrap();
        let mut report2 = PreflightReport::default();
        check_lockfile(&repo, &mut report2);
        assert!(!report2.has_blocking_failures());
        assert_eq!(report2.items[0].status, CheckStatus::Pass);

        let _ = std::fs::remove_dir_all(&temp_dir);
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
        let report = check_repo(&CheckRepoOptions {
            config_path,
            no_sandbox: false,
            skip_baseline_tests: false,
        })
        .await;
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
