//! Pre-flight check of a repository (#450): whether a repo and its
//! environment meet what aegis needs before its first job, run by
//! `aegis-local check-repo` and by the server's repo page (#599):
//! 1. language profile resolution;
//! 2. git repository state, base branch, and clean working directory;
//! 3. manifest and lockfile presence (air-gapped reproducibility);
//! 4. Docker daemon access and the sandbox image;
//! 5. the fetch step's registry hosts;
//! 6. the baseline gate in the sandbox (`--network none`): format and
//!    lint, the unit tests passing on the base, the time against the step
//!    timeout, and connections to a database or the network the isolation
//!    blocked.

mod checks;
mod gate;

use std::time::{Duration, Instant};

use aegis_lang::Profiles;
use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::RunConfig;

/// Status of an individual pre-flight verification item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, Serialize)]
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
    /// Each item as soon as it is known: the server's page shows them as
    /// the checks run (#599).
    progress: Option<UnboundedSender<CheckItem>>,
}

impl PreflightReport {
    /// A report that also sends each item to `progress` when added.
    pub fn with_progress(progress: UnboundedSender<CheckItem>) -> Self {
        Self {
            progress: Some(progress),
            ..Self::default()
        }
    }

    pub fn add(&mut self, item: CheckItem) {
        if let Some(progress) = &self.progress {
            // A receiver gone (the page's task ended) stops nothing.
            let _ = progress.send(item.clone());
        }
        self.items.push(item);
    }

    pub fn has_blocking_failures(&self) -> bool {
        self.items
            .iter()
            .any(|i| i.status == CheckStatus::Fail && i.is_blocking)
    }
}

/// What to check.
#[derive(Debug, Clone, Copy, Default)]
pub struct PreflightOptions {
    /// The gate runs on the host, not in Docker.
    pub no_sandbox: bool,
    pub skip_baseline_tests: bool,
    /// The baseline gate fetches the base's dependencies through the
    /// egress proxy into the repo's build cache, as a job does (#599, the
    /// server's clone has none yet); without it they must be cached.
    pub fetch: bool,
}

/// Runs every check of `cfg`'s repo into `report`; a blocking failure
/// early on stops the rest.
pub async fn run_preflight(cfg: &RunConfig, opts: &PreflightOptions, report: &mut PreflightReport) {
    let start = Instant::now();
    run_checks(cfg, opts, report).await;
    report.total_duration += start.elapsed();
}

async fn run_checks(cfg: &RunConfig, opts: &PreflightOptions, report: &mut PreflightReport) {
    report.repo_key = cfg.repo.key.clone();
    report.language = cfg.repo.language.to_string();

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

    let repo_profiles = match cfg.repo.profiles(&builtin_profiles) {
        Ok(p) => {
            report.add(CheckItem {
                category: "Profile",
                name: "Profile Resolution".into(),
                status: CheckStatus::Pass,
                detail: format!(
                    "Resolved profile for '{}' (root: {}, parts: {})",
                    cfg.repo.language,
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
    checks::check_git_repo(&cfg.repo, report);

    // 3. Lockfile and dependency manifest presence
    checks::check_lockfile(&cfg.repo, report);

    // 4. Docker daemon and sandbox image availability
    let docker_available = if !opts.no_sandbox && cfg.run.sandbox {
        checks::check_docker_environment(cfg, &repo_profiles, report)
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
    checks::check_fetch_hosts(&repo_profiles, report);

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
        gate::run_baseline_gate(
            cfg,
            &repo_profiles,
            docker_available && !opts.no_sandbox,
            opts.fetch,
            report,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
