//! Running the scanner on the tree: the CLI scanner once, or the .NET
//! scanner around the profile's build. Only the scanner commands get the
//! token and, in containers, a network; the .NET build in between runs
//! like any other step.
//!
//! The scanner reads its settings from a file the gate writes, never the
//! repo's `sonar-project.properties`: a patch could otherwise exclude its
//! own files from the analysis or send the token elsewhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aegis_lang::{Profile, SonarScanner, join_dir};

use crate::GateError;
use crate::command::{Ran, Runner};
use crate::sandbox::{Hardening, Sandbox};

use super::SonarStep;

/// The gate's own directory in the tree: settings, the CLI scanner's
/// working directory. Recreated for each run and removed after it.
pub(crate) const DIR: &str = ".aegis-gate-sonar";
/// Where the .NET scanner keeps its state, in the tree.
const DOTNET_DIR: &str = ".sonarqube";
/// The scanner cache inside the scanner's container.
const CACHE_IN_CONTAINER: &str = "/aegis-sonar-cache";
const TOKEN_ENV: &str = "SONAR_TOKEN";

/// The analysis as a pull request: what the server calls it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullRequest {
    pub key: String,
    pub branch: String,
    pub base: String,
}

impl PullRequest {
    /// Named after the tree under check, so a rerun of the same patch
    /// replaces its analysis instead of adding one.
    pub(crate) fn of(tree: &str, base: &str) -> Self {
        let short: String = tree.chars().take(12).collect();
        Self {
            key: format!("aegis-{short}"),
            branch: format!("aegis/{short}"),
            base: base.to_owned(),
        }
    }
}

/// What one scan ran and where the scanner left its task id.
pub(crate) struct Scan {
    pub ran: Vec<Ran>,
    /// The first command that did not succeed, if any.
    pub failed: Option<Ran>,
    pub report_task: PathBuf,
}

/// Removes the scanner's directories from the tree when dropped.
pub(crate) struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for dir in &self.0 {
            let _ = remove(dir);
        }
    }
}

/// Removes `path` whatever it is; a symlink is removed, not followed.
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Fresh, empty scanner directories in `repo`: whatever the patch put
/// there (a forged `report-task.txt`) is gone.
pub(crate) fn prepare(repo: &Path) -> Result<Cleanup, GateError> {
    let dirs = vec![repo.join(DIR), repo.join(DOTNET_DIR)];
    for dir in &dirs {
        remove(dir).map_err(|e| GateError::io(dir, &e))?;
    }
    let own = &dirs[0];
    std::fs::create_dir(own).map_err(|e| GateError::io(own, &e))?;
    Ok(Cleanup(dirs))
}

/// A `.properties` value: backslashes escaped; line breaks refused.
fn escape(key: &str, value: &str) -> Result<String, GateError> {
    if key.contains(['\n', '\r', '=', ':', ' ']) || value.contains(['\n', '\r']) {
        return Err(GateError::InvalidRequest(format!(
            "Sonar property `{key}` must fit on one line, its key without `=`, `:` or spaces"
        )));
    }
    Ok(format!("{key}={}", value.replace('\\', "\\\\")))
}

/// Whether `key` holds paths: `…Path(s)`, sources, tests, inclusions and
/// exclusions.
fn holds_paths(key: &str) -> bool {
    let last = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
    last.ends_with("path")
        || last.ends_with("paths")
        || matches!(
            last.as_str(),
            "sources" | "tests" | "inclusions" | "exclusions"
        )
}

/// The `[sonar.properties]` of every part of the repo (#199), for the one
/// analysis of the whole tree, the root's first. A part's paths are
/// relative to its directory, so they are given from the repo root and
/// joined to the other parts' under the same key; any other value is the
/// first part's that sets it.
pub(crate) fn repo_properties<'p>(
    parts: impl IntoIterator<Item = (&'p str, &'p Profile)>,
) -> BTreeMap<String, String> {
    let mut merged: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (dir, profile) in parts {
        for (key, value) in &profile.sonar.properties {
            if !holds_paths(key) {
                merged
                    .entry(key.clone())
                    .or_insert_with(|| vec![value.clone()]);
                continue;
            }
            let values = merged.entry(key.clone()).or_default();
            // A part's sources must not narrow the root's default, the tree.
            if values.is_empty() && !dir.is_empty() && key == "sonar.sources" {
                values.push(".".into());
            }
            for path in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let path = match path.starts_with('/') {
                    true => path.to_owned(),
                    false => join_dir(dir, path),
                };
                if !values.contains(&path) {
                    values.push(path);
                }
            }
        }
    }
    merged
        .into_iter()
        .map(|(key, values)| (key, values.join(",")))
        .collect()
}

/// Every property of the analysis: the parts' (see [`repo_properties`]),
/// the configuration's, then the gate's own, which nothing overrides.
pub(crate) fn properties(
    step: &SonarStep,
    parts: &BTreeMap<String, String>,
    pr: Option<&PullRequest>,
    cache: Option<&str>,
) -> BTreeMap<String, String> {
    let mut p: BTreeMap<String, String> = parts.clone();
    p.extend(step.properties.clone());
    p.entry("sonar.sources".into())
        .or_insert_with(|| ".".into());
    let own = format!("{DIR}/**,{DOTNET_DIR}/**");
    let exclusions = match p.get("sonar.exclusions") {
        Some(e) if !e.trim().is_empty() => format!("{e},{own}"),
        _ => own,
    };
    p.insert("sonar.exclusions".into(), exclusions);
    p.extend(forced(step, pr, cache));
    p
}

/// What decides where the analysis goes and what it is compared with.
fn forced(
    step: &SonarStep,
    pr: Option<&PullRequest>,
    cache: Option<&str>,
) -> Vec<(String, String)> {
    let mut p = vec![
        ("sonar.host.url".into(), step.url.clone()),
        ("sonar.projectKey".into(), step.project_key.clone()),
        ("sonar.projectBaseDir".into(), ".".into()),
        ("sonar.working.directory".into(), format!("{DIR}/work")),
    ];
    if let Some(org) = &step.organization {
        p.push(("sonar.organization".into(), org.clone()));
    }
    if let Some(cache) = cache {
        p.push(("sonar.userHome".into(), cache.to_owned()));
    }
    if let Some(pr) = pr {
        p.push(("sonar.pullrequest.key".into(), pr.key.clone()));
        p.push(("sonar.pullrequest.branch".into(), pr.branch.clone()));
        p.push(("sonar.pullrequest.base".into(), pr.base.clone()));
    }
    p
}

/// The runner of the scanner commands: in a container, the scanner's
/// image on a network that reaches the server, with the cache mounted.
fn scanner_runner(step: &SonarStep, runner: &Runner) -> (Runner, Option<String>) {
    let Some(sandbox) = &runner.sandbox else {
        let cache = step.cache.as_ref().map(|c| c.display().to_string());
        return (runner.clone(), cache);
    };
    let mounts: Vec<(PathBuf, String)> = step
        .cache
        .iter()
        .map(|c| (c.clone(), CACHE_IN_CONTAINER.to_owned()))
        .collect();
    let cache = step.cache.as_ref().map(|_| CACHE_IN_CONTAINER.to_owned());
    let scanner = Sandbox {
        image: step.image.clone().unwrap_or_else(|| sandbox.image.clone()),
        mounts,
        network: Some(step.network.clone()),
        // The scanner runs no code of the patch; it writes its cache, and
        // its image (not one of ours) where it likes.
        read_only_mounts: false,
        hardening: Hardening {
            writable: Vec::new(),
            read_only_rootfs: false,
            ..sandbox.hardening.clone()
        },
        ..sandbox.clone()
    };
    (
        Runner {
            sandbox: Some(scanner),
            ..runner.clone()
        },
        cache,
    )
}

/// Runs the scanner as the profile says; stops at the first command that
/// does not succeed.
pub(crate) fn scan(
    step: &SonarStep,
    profile: &Profile,
    parts: &BTreeMap<String, String>,
    pr: Option<&PullRequest>,
    runner: &Runner,
    repo: &Path,
    logs: &Path,
) -> Result<Scan, GateError> {
    let (scanner, cache) = scanner_runner(step, runner);
    let props = properties(step, parts, pr, cache.as_deref());
    let secret = [(TOKEN_ENV, step.token.as_str())];
    let mut ran = Vec::new();
    match profile.sonar.scanner {
        SonarScanner::Cli => {
            let lines = props
                .iter()
                .map(|(k, v)| escape(k, v))
                .collect::<Result<Vec<_>, _>>()?;
            let settings = repo.join(DIR).join("project.properties");
            std::fs::write(&settings, lines.join("\n") + "\n")
                .map_err(|e| GateError::io(&settings, &e))?;
            let mut argv = step.scanner.clone();
            argv.push(format!("-Dproject.settings={DIR}/project.properties"));
            // Also on the command line, which wins over any file.
            for (k, v) in forced(step, pr, cache.as_deref()) {
                argv.push(format!("-D{k}={v}"));
            }
            ran.push(scanner.run_with_secrets(
                &argv,
                repo,
                &logs.join("sonar.log"),
                &[],
                &secret,
            )?);
        }
        SonarScanner::Dotnet => {
            let mut begin: Vec<String> = ["dotnet", "sonarscanner", "begin"]
                .map(str::to_owned)
                .into();
            begin.push(format!("/k:{}", step.project_key));
            for (k, v) in &props {
                // The .NET scanner sets these itself.
                if matches!(
                    k.as_str(),
                    "sonar.projectKey" | "sonar.projectBaseDir" | "sonar.working.directory"
                ) {
                    continue;
                }
                escape(k, v)?;
                begin.push(format!("/d:{k}={v}"));
            }
            let begun = scanner.run_with_secrets(
                &begin,
                repo,
                &logs.join("sonar-begin.log"),
                &[],
                &secret,
            )?;
            let ok = begun.exit.success();
            ran.push(begun);
            if ok {
                let build = profile
                    .sonar
                    .build
                    .as_ref()
                    .expect("checked with the profile")
                    .render(&[])?;
                // The build is the patch's code: the gate's own runner.
                let built = runner.run(&build, repo, &logs.join("sonar-build.log"), &[])?;
                let ok = built.exit.success();
                ran.push(built);
                if ok {
                    let end: Vec<String> =
                        ["dotnet", "sonarscanner", "end"].map(str::to_owned).into();
                    ran.push(scanner.run_with_secrets(
                        &end,
                        repo,
                        &logs.join("sonar-end.log"),
                        &[],
                        &secret,
                    )?);
                }
            }
        }
    }
    let failed = ran.iter().find(|r| !r.exit.success()).cloned();
    let report_task = match profile.sonar.scanner {
        SonarScanner::Cli => repo.join(DIR).join("work").join("report-task.txt"),
        SonarScanner::Dotnet => repo
            .join(DOTNET_DIR)
            .join("out")
            .join(".sonar")
            .join("report-task.txt"),
    };
    Ok(Scan {
        ran,
        failed,
        report_task,
    })
}

/// `ceTaskId` from the scanner's `report-task.txt`.
pub(crate) fn task_id(report_task: &Path) -> Result<String, GateError> {
    let text = std::fs::read_to_string(report_task).map_err(|e| {
        GateError::Sonar(format!(
            "the scanner left no task to wait for at `{}`: {e}",
            report_task.display()
        ))
    })?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("ceTaskId="))
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| GateError::Sonar(format!("no ceTaskId in `{}`", report_task.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_with_line_breaks_are_refused_and_backslashes_escaped() {
        assert_eq!(escape("a.b", r"C:\x").unwrap(), r"a.b=C:\\x");
        assert!(escape("a.b", "x\nsonar.host.url=evil").is_err());
        assert!(escape("a=b", "x").is_err());
    }

    fn profile(language: aegis_lang::Language, extra: &[(&str, &str)]) -> Profile {
        let mut profile = aegis_lang::Profiles::builtin()
            .unwrap()
            .get(language)
            .unwrap()
            .clone();
        for (k, v) in extra {
            profile.sonar.properties.insert((*k).into(), (*v).into());
        }
        profile
    }

    #[test]
    fn every_part_s_properties_join_with_paths_from_the_repo_root() {
        use aegis_lang::Language;
        let root = profile(Language::Rust, &[("sonar.python.version", "3.12")]);
        let py = profile(
            Language::Python,
            &[
                ("sonar.python.version", "3.11"),
                ("sonar.exclusions", "**/migrations/**, /abs/x.py"),
            ],
        );
        let web = profile(Language::Rust, &[]);
        let merged = repo_properties([("", &root), ("py", &py), ("tools/web", &web)]);
        let get = |k: &str| merged.get(k).map(String::as_str);
        assert_eq!(
            get("sonar.rust.lcov.reportPaths"),
            Some("lcov.info,tools/web/lcov.info")
        );
        assert_eq!(
            get("sonar.python.coverage.reportPaths"),
            Some("py/coverage.xml")
        );
        assert_eq!(
            get("sonar.exclusions"),
            Some("py/**/migrations/**,/abs/x.py")
        );
        // Not a path: the first part that sets it, the root first.
        assert_eq!(get("sonar.python.version"), Some("3.12"));
        // A part's sources do not narrow the root's default of the tree.
        let narrow = profile(Language::Python, &[("sonar.sources", "src")]);
        let merged = repo_properties([("", &web), ("py", &narrow)]);
        assert_eq!(merged["sonar.sources"], ".,py/src");
        // One profile: its properties as they are.
        assert_eq!(repo_properties([("", &root)]), root.sonar.properties);
    }

    #[test]
    fn task_id_is_read_from_the_report() {
        let dir = std::env::temp_dir().join(format!("aegis-sonar-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("report-task.txt");
        std::fs::write(&file, "projectKey=p\nceTaskId=AX12\nceTaskUrl=u\n").unwrap();
        assert_eq!(task_id(&file).unwrap(), "AX12");
        std::fs::write(&file, "projectKey=p\n").unwrap();
        assert!(task_id(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
