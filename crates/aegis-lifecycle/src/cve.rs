//! CVE jobs (#501, ADR 0015): the platform raises one package to the
//! version that fixes an advisory, with the profile's `dependencies.upgrade`
//! command, then the gate checks the upgraded tree under
//! `DependencyPolicy::Upgrade`. When it passes, the PR opens with no agent
//! turn; when it fails (a breaking change), the agent adapts the code and
//! must keep the upgrade as it is.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use aegis_gate::{CancelToken, Runner, Upgrade, same_package};
use aegis_lang::{Placeholder, ProfilePart, read_lockfile};
use aegis_pr::ChangeNote;
use tokio_util::sync::CancellationToken;

use crate::answer::{FinalAnswer, Status};
use crate::run::{AbortOnDrop, CancelOnDrop, fetch_step, sandbox};
use crate::{LifecycleError, RunConfig};

/// What a CVE job raises: given by a maintainer, never read from an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CveTarget {
    /// As the ecosystem names it: `serde`, `requests`, `@scope/name`,
    /// `golang.org/x/net`, `org.example:lib`.
    pub package: String,
    pub version: String,
    /// `GHSA-…`, `CVE-…`, `RUSTSEC-…`, `PYSEC-…`.
    pub advisory: String,
}

/// A value the upgrade command gets as one argument: no leading `-` (it
/// would read as an option), and only the characters package names,
/// versions and advisory ids use.
fn plain(value: &str, extra: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(c))
}

impl CveTarget {
    pub fn check(&self) -> Result<(), String> {
        if !plain(&self.package, "@/:+", 214) {
            return Err(format!(
                "package `{}` is not a plain package name",
                self.package
            ));
        }
        if !plain(&self.version, "+", 64) {
            return Err(format!("version `{}` is not a plain version", self.version));
        }
        if !plain(&self.advisory, ":", 64) {
            return Err(format!(
                "advisory `{}` is not a plain advisory id",
                self.advisory
            ));
        }
        Ok(())
    }

    /// The job's `issue_ref` on the server (#502):
    /// `cve:<advisory> <package> <version>`. None of them holds a space.
    pub fn to_ref(&self) -> String {
        format!("cve:{} {} {}", self.advisory, self.package, self.version)
    }

    /// The target of a [`Self::to_ref`] string, checked again: the row is
    /// data, not trusted.
    pub fn from_ref(text: &str) -> Result<Self, String> {
        let bad = || format!("`{text}` is not `cve:<advisory> <package> <version>`");
        let rest = text.strip_prefix("cve:").ok_or_else(bad)?;
        let parts: Vec<&str> = rest.split(' ').collect();
        let [advisory, package, version] = parts.as_slice() else {
            return Err(bad());
        };
        let target = Self {
            package: (*package).to_owned(),
            version: (*version).to_owned(),
            advisory: (*advisory).to_owned(),
        };
        target.check()?;
        Ok(target)
    }

    /// Where the advisory is described.
    pub fn url(&self) -> String {
        format!("https://osv.dev/vulnerability/{}", self.advisory)
    }

    pub fn title(&self) -> String {
        format!(
            "Raise {} to {} ({})",
            self.package, self.version, self.advisory
        )
    }
}

/// The paths `git status` reports changed or added in `tree`, ignored
/// files left out.
fn changed_paths(tree: &Path) -> Result<Vec<String>, LifecycleError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .map_err(|e| LifecycleError::io(tree, &e))?;
    if !out.status.success() {
        return Err(LifecycleError::Config(format!(
            "git status in the job's tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut paths = Vec::new();
    let mut entries = text.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let (status, path) = entry.split_at(3.min(entry.len()));
        // A rename lists its old path next; both changed.
        if (status.starts_with('R') || status.starts_with('C'))
            && let Some(old) = entries.next()
        {
            paths.push(old.to_owned());
        }
        paths.push(path.to_owned());
    }
    Ok(paths)
}

/// What the upgrade wrote: its manifests and lockfiles, or why it did not
/// do what the job needs.
fn collect(
    part: &ProfilePart,
    tree: &Path,
    target: &CveTarget,
) -> Result<Result<Upgrade, String>, LifecycleError> {
    let deps = &part.profile.profile.dependencies;
    let matches = |patterns: &[String], path: &str| {
        let name = path.rsplit('/').next().unwrap_or(path);
        patterns.iter().any(|p| aegis_lang::glob_match(p, name))
    };
    let mut files = BTreeMap::new();
    let mut other = Vec::new();
    for path in changed_paths(tree)? {
        let in_part = part.relative(&path).is_some();
        if in_part && (matches(&deps.manifests, &path) || matches(&deps.lockfiles, &path)) {
            let text = std::fs::read_to_string(tree.join(&path))
                .map_err(|e| LifecycleError::io(&tree.join(&path), &e))?;
            files.insert(path, text);
        } else {
            other.push(path);
        }
    }
    if !other.is_empty() {
        return Ok(Err(format!(
            "the upgrade changed files beyond the manifests and lockfiles: {}",
            other.join(", ")
        )));
    }
    if files.is_empty() {
        return Ok(Err(format!(
            "the upgrade changed nothing: `{}` may already be at {}, or not be a dependency",
            target.package, target.version
        )));
    }
    let lockfiles: Vec<(&String, &String)> = files
        .iter()
        .filter(|(p, _)| matches(&deps.lockfiles, p))
        .collect();
    let locked = lockfiles.iter().any(|(path, text)| {
        let name = path.rsplit('/').next().unwrap_or(path);
        read_lockfile(part.profile.profile.language, name, text).is_ok_and(|deps| {
            deps.iter()
                .any(|d| same_package(&d.name, &target.package) && d.version == target.version)
        })
    });
    if !lockfiles.is_empty() && !locked {
        return Ok(Err(format!(
            "after the upgrade no lockfile locks `{}` at {}",
            target.package, target.version
        )));
    }
    Ok(Ok(Upgrade {
        package: target.package.clone(),
        version: target.version.clone(),
        files,
    }))
}

/// Runs the part's upgrade command in `tree`: through the egress proxy in
/// a container when the gate is sandboxed, as the fetch step does, on the
/// machine otherwise. `Ok(Err(reason))` when it did not raise the package.
pub(crate) async fn upgrade(
    cfg: &RunConfig,
    part: &ProfilePart,
    tree: &Path,
    job_dir: &Path,
    target: &CveTarget,
    cancel: &CancellationToken,
) -> Result<Result<Upgrade, String>, LifecycleError> {
    let Some(command) = part.profile.upgrade.command() else {
        return Ok(Err(format!(
            "the {} profile has no upgrade command, or the repo turned it off",
            part.profile.profile.language
        )));
    };
    let argv = command.render(&[
        (Placeholder::Package, &target.package),
        (Placeholder::Version, &target.version),
    ])?;
    let dir = tree.join(&part.dir);
    let log = job_dir.join("upgrade.log");
    let timeout = Duration::from_secs(cfg.run.step_timeout_s);
    let inner = CancelToken::new();
    let _stop = CancelOnDrop(inner.clone());
    let _watcher = AbortOnDrop(tokio::spawn({
        let (outer, inner) = (cancel.clone(), inner.clone());
        async move {
            outer.cancelled().await;
            inner.cancel();
        }
    }));
    let fetch = fetch_step(cfg, &part.profile);
    let sandbox = sandbox(cfg, part)?;
    let (log_path, run_dir) = (log.clone(), dir.clone());
    let ran = tokio::task::spawn_blocking(move || -> Result<_, aegis_gate::GateError> {
        match (fetch, sandbox) {
            (Some(fetch), Some(sandbox)) => {
                let session = fetch.egress.start()?;
                let ran = session.fetch(&sandbox, &argv, &run_dir, &log_path, timeout, inner)?;
                Ok((ran, session.log()))
            }
            _ => {
                let runner = Runner {
                    env: vec![],
                    timeout,
                    cancel: inner,
                    sandbox: None,
                };
                Ok((runner.run(&argv, &run_dir, &log_path, &[])?, String::new()))
            }
        }
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("the upgrade did not finish: {e}")))??;
    let (ran, proxy) = ran;
    if !ran.exit.success() {
        let text = std::fs::read_to_string(&ran.log).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        let mut why = format!(
            "raising `{}` to {} failed:\n{}",
            target.package,
            target.version,
            tail.join("\n")
        );
        if !proxy.is_empty() {
            why.push_str(&format!("\n\nproxy:\n{proxy}"));
        }
        return Ok(Err(why));
    }
    collect(part, tree, target)
}

/// The answer the gate checks first: the upgrade, no reproduction test
/// (the dependency step and the audit check the fix).
pub(crate) fn answer(target: &CveTarget, upgrade: &Upgrade) -> FinalAnswer {
    FinalAnswer {
        status: Status::Fixed,
        cause: format!(
            "{}: `{}` is raised to {}, the version that fixes it. The platform ran the \
             ecosystem's upgrade command; the manifests and lockfiles below are its output.",
            target.advisory, target.package, target.version
        ),
        changes: upgrade
            .files
            .keys()
            .map(|path| ChangeNote {
                location: path.clone(),
                note: format!("raised `{}` to {}", target.package, target.version),
            })
            .collect(),
        repro: None,
        allowed_test_changes: vec![],
        allowed_reference_changes: vec![],
        stop_reason: None,
        review_replies: vec![],
        suspicious_input: vec![],
    }
}

/// What the agent reads when the upgraded tree fails the checks; the
/// failure follows it.
pub(crate) fn first_message(target: &CveTarget, upgrade: &Upgrade) -> String {
    let files: Vec<&str> = upgrade.files.keys().map(String::as_str).collect();
    format!(
        "This job fixes the security advisory {advisory} by raising the dependency `{package}` \
         to {version}. The platform has already done the upgrade, in {files}.\n\n\
         The upgraded code fails the checks (the failure follows). Change the code, and its \
         tests where the new version requires it, so that it builds and passes with \
         `{package}` {version}. Do not edit {files} or any other manifest or lockfile, and do \
         not change any dependency: the checks require the upgrade exactly as the platform \
         made it. No reproduction test is needed. Then reply with the final JSON answer; list \
         the code you changed in `changes`.",
        advisory = target.advisory,
        package = target.package,
        version = target.version,
        files = files.join(", "),
    )
}

#[cfg(test)]
mod tests;
