//! Test-name matching and the small formatting helpers the steps share.

use super::*;

/// `^(a|b)$` matching exactly `names`, regex characters escaped.
pub(super) fn exact_regex(names: &[&str]) -> String {
    let escaped: Vec<String> = names
        .iter()
        .map(|n| {
            n.chars()
                .flat_map(|c| {
                    let special = "\\.+*?()|[]{}^$#&-~".contains(c);
                    special.then_some('\\').into_iter().chain([c])
                })
                .collect()
        })
        .collect();
    format!("^({})$", escaped.join("|"))
}

/// Whether `id`, as read from test output, is the test named `test`. A
/// parametrized pytest case `<test>[<params>]` is one of its cases (#133).
pub(super) fn is_test(id: &str, test: &str) -> bool {
    let id = without_params(id);
    id == test || id.ends_with(&format!("::{test}"))
}

/// Whether `id` is the test named `test` or one inside it, when `test` names
/// a file or a class (#136).
pub(crate) fn is_in(id: &str, test: &str) -> bool {
    let id = without_params(id);
    is_test(id, test) || id.starts_with(&format!("{test}::")) || id.contains(&format!("::{test}::"))
}

/// The test's own name, without the binary or file before `::`: what the
/// harness's update command selects.
pub(super) fn harness_name(id: &str) -> &str {
    let id = without_params(id);
    id.rsplit("::").next().unwrap_or(id)
}

/// Whether `text` matches `pattern`, where `*` matches any run of
/// characters.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(mut rest) = parts.next().and_then(|first| text.strip_prefix(first)) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// Whether `path` is under the directory `dir`.
pub(super) fn is_inside(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

pub(super) fn without_params(id: &str) -> &str {
    match id.split_once('[') {
        Some((name, _)) if id.ends_with(']') => name,
        _ => id,
    }
}

/// Directories between a part's `dir` and the repo root.
pub(super) fn depth(dir: &str) -> usize {
    Path::new(dir).components().count()
}

pub(super) fn skip_of(reason: SkipReason) -> Skip {
    match reason {
        SkipReason::NotInProfile => Skip::NotInProfile,
        SkipReason::DisabledByRepo => Skip::DisabledByRepo,
    }
}

pub(super) fn exit_text(exit: Exit) -> String {
    match exit {
        Exit::Code(c) => format!("exit code {c}"),
        Exit::Signal => "killed by a signal".into(),
        Exit::TimedOut => "timed out".into(),
        Exit::Cancelled => "cancelled".into(),
        Exit::OutOfMemory { limit_mb } => out_of_memory(limit_mb),
    }
}

pub(super) fn list(tests: &BTreeSet<String>) -> String {
    let mut lines: Vec<String> = tests
        .iter()
        .take(MAX_LISTED_TESTS)
        .map(|t| format!("  {t}"))
        .collect();
    if tests.len() > MAX_LISTED_TESTS {
        lines.push(format!("  … and {} more", tests.len() - MAX_LISTED_TESTS));
    }
    lines.join("\n")
}

/// `` `a`, `b` ``, or `none` for no paths.
pub(super) fn quoted<S: AsRef<str>>(paths: &[S]) -> String {
    if paths.is_empty() {
        return "none".into();
    }
    paths
        .iter()
        .map(|p| format!("`{}`", p.as_ref()))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn report(
    name: StepName,
    status: StepStatus,
    ran: &[&Ran],
    detail: String,
) -> StepReport {
    StepReport {
        name,
        status,
        duration: ran.iter().map(|r| r.duration).sum(),
        detail,
        logs: ran.iter().map(|r| r.log.clone()).collect(),
        fails_on_base: false,
    }
}

/// `step`, passed because the base revision fails it too (#487).
pub(super) fn as_on_base(step: StepReport, on_base: bool) -> StepReport {
    StepReport {
        fails_on_base: on_base,
        ..step
    }
}

/// `repo/path` when it is a plain relative path to a regular file that
/// stays inside `repo`: no `..`, no symlink anywhere on the way.
pub(super) fn regular_file_in(repo: &Path, path: &str) -> Result<PathBuf, GateError> {
    let invalid = || {
        GateError::InvalidRequest(format!(
            "repro file `{path}` must be a regular file inside the repo, not a link"
        ))
    };
    let p = Path::new(path);
    if p.is_absolute() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(invalid());
    }
    let full = repo.join(p);
    let meta = std::fs::symlink_metadata(&full).map_err(|_| invalid())?;
    let real = full.canonicalize().map_err(|_| invalid())?;
    if !meta.is_file() || real != full {
        return Err(invalid());
    }
    Ok(full)
}

/// A log as text (logs are capped, see `command::MAX_LOG_BYTES`).
pub(super) fn read_log(path: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).into_owned()
}
