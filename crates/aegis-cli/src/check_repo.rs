//! `aegis-local check-repo` (#450): the pre-flight check of one repo's
//! run configuration ([`aegis_lifecycle::run_preflight`], shared with the
//! server's repo page, #599), printed as a table.

use std::path::PathBuf;

use aegis_lifecycle::{
    CheckItem, CheckStatus, PreflightOptions, PreflightReport, RunConfig, run_preflight,
};

/// Options passed from CLI arguments.
#[derive(Debug, Clone, Default)]
pub struct CheckRepoOptions {
    pub config_path: PathBuf,
    pub no_sandbox: bool,
    pub skip_baseline_tests: bool,
}

/// Runs every check, prints the report, and says whether the repo is ready.
pub async fn execute_check_repo(opts: &CheckRepoOptions) -> Result<bool, String> {
    let report = check_repo(opts).await;
    print_summary(&report);
    Ok(!report.has_blocking_failures())
}

async fn check_repo(opts: &CheckRepoOptions) -> PreflightReport {
    let mut report = PreflightReport::default();
    let config = RunConfig::load(&opts.config_path);
    report.add(CheckItem {
        category: "Config",
        name: "RunConfig".into(),
        status: if config.is_ok() {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        detail: match &config {
            Ok(_) => format!("Loaded successfully from {}", opts.config_path.display()),
            Err(e) => format!("Failed to parse config: {e}"),
        },
        is_blocking: true,
    });
    if let Ok(cfg) = config {
        let options = PreflightOptions {
            no_sandbox: opts.no_sandbox,
            skip_baseline_tests: opts.skip_baseline_tests,
            fetch: false,
        };
        run_preflight(&cfg, &options, &mut report).await;
    }
    report
}

/// Prints `report` as a table, with what blocks and what to do.
fn print_summary(report: &PreflightReport) {
    println!();
    println!("================================================================================");
    println!(
        " AEGIS REPO PRE-FLIGHT CHECK REPORT: {} ({})",
        report.repo_key, report.language
    );
    println!("================================================================================");
    for item in &report.items {
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
    println!("--------------------------------------------------------------------------------");
    let passed = report
        .items
        .iter()
        .filter(|i| i.status == CheckStatus::Pass)
        .count();
    let failed = report
        .items
        .iter()
        .filter(|i| i.status == CheckStatus::Fail)
        .count();
    let warnings = report
        .items
        .iter()
        .filter(|i| i.status == CheckStatus::Warn)
        .count();

    println!(
        "Summary: {} passed, {} failed, {} warnings (Total duration: {:.1}s)",
        passed,
        failed,
        warnings,
        report.total_duration.as_secs_f64()
    );

    if report.has_blocking_failures() {
        println!();
        println!("RESULT: NOT READY FOR PILOT (Blocking issues detected)");
        println!("Remediation recommendations:");
        for item in &report.items {
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
    println!("================================================================================");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_config_that_does_not_load_blocks() {
        let report = check_repo(&CheckRepoOptions {
            config_path: PathBuf::from("/nonexistent/aegis-check.toml"),
            ..CheckRepoOptions::default()
        })
        .await;
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].status, CheckStatus::Fail);
        assert!(report.has_blocking_failures());
    }
}
