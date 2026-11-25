//! The checks before the gate: git, manifests, Docker, fetch hosts.

use std::process::Command;

use aegis_lang::{Language, RepoProfiles};

use super::{CheckItem, CheckStatus, PreflightReport};
use crate::{RepoConfig, RunConfig};

pub(super) fn check_git_repo(repo: &RepoConfig, report: &mut PreflightReport) {
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
pub(super) fn check_lockfile(repo: &RepoConfig, report: &mut PreflightReport) {
    let expected = match repo.language {
        Language::Rust => vec!["Cargo.lock"],
        Language::Python => vec!["poetry.lock", "Pipfile.lock", "uv.lock", "requirements.txt"],
        Language::Go => vec!["go.sum"],
        Language::Java => vec!["pom.xml", "build.gradle", "gradle.lockfile"],
        Language::TypeScript => vec!["package-lock.json", "pnpm-lock.yaml", "yarn.lock"],
        Language::CSharp => vec!["packages.lock.json"],
        Language::Php => vec!["composer.lock"],
        // Without a lockfile the gate compares the resolved tree (#80).
        Language::Kotlin => vec!["gradle.lockfile", "build.gradle.kts", "build.gradle"],
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
pub(super) fn check_docker_environment(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    report: &mut PreflightReport,
) -> bool {
    let docker_cmd = &cfg.run.docker;
    if !check_docker_daemon(docker_cmd, report) {
        return false;
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

/// Checks that `docker_cmd` reaches a daemon. `docker version` asks for
/// `/_ping` and `/version` only, which `aegis-docker-proxy` passes (#321);
/// `docker info` asks for `/info`, which it refuses, so inside the server
/// image (#474) `info` would always fail.
pub(super) fn check_docker_daemon(docker_cmd: &str, report: &mut PreflightReport) -> bool {
    let out = Command::new(docker_cmd)
        .args(["version", "--format", "{{.Server.Version}}"])
        .output();
    let (status, detail) = match out {
        Ok(out) if out.status.success() => (
            CheckStatus::Pass,
            format!(
                "Docker daemon reachable (server {})",
                String::from_utf8_lossy(&out.stdout).trim()
            ),
        ),
        Ok(out) => (
            CheckStatus::Fail,
            format!(
                "Docker daemon error: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ),
        Err(e) => (
            CheckStatus::Fail,
            format!("Failed to invoke `{docker_cmd}`: {e}"),
        ),
    };
    let ok = status == CheckStatus::Pass;
    report.add(CheckItem {
        category: "Docker",
        name: "Docker Daemon".into(),
        status,
        detail,
        is_blocking: true,
    });
    ok
}

/// Checks whether fetch_hosts are configured for external registries.
pub(super) fn check_fetch_hosts(profiles: &RepoProfiles, report: &mut PreflightReport) {
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A docker that answers like `aegis-docker-proxy` (#321): `version`
    /// works, `info` is refused.
    #[cfg(unix)]
    fn proxied_docker(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("docker");
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             if [ \"$1\" = version ]; then echo 29.0.1; exit 0; fi\n\
             echo \"Error response from daemon: refused by aegis-docker-proxy: $1\" >&2\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn the_daemon_check_passes_through_the_docker_proxy() {
        let dir = std::env::temp_dir().join(format!("aegis_check_daemon_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let docker = proxied_docker(&dir);

        let mut report = PreflightReport::default();
        assert!(check_docker_daemon(&docker, &mut report));
        assert_eq!(report.items[0].status, CheckStatus::Pass);
        assert!(
            report.items[0].detail.contains("29.0.1"),
            "{:?}",
            report.items[0]
        );

        let mut report = PreflightReport::default();
        assert!(!check_docker_daemon(
            &dir.join("missing").display().to_string(),
            &mut report
        ));
        assert!(report.has_blocking_failures());

        let _ = std::fs::remove_dir_all(&dir);
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
            secrets: Default::default(),
            triage_lane: Default::default(),
            feature_lane: Default::default(),
            sidecars: vec![],
            locale: Default::default(),
            app: None,
            fork: crate::ForkConfig {
                platform: crate::ForkPlatform::GitHub,
                owner: "owner".into(),
                name: "repo".into(),
                api: "https://api.github.com".into(),
                token_env: "GITHUB_TOKEN".into(),
                push_to_local: None,
                entra: None,
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
}
