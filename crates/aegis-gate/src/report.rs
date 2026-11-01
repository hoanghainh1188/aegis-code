//! What the gate returns: one entry per step of design §6.1, in order, and
//! a short text for the agent. Full logs stay in files.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Lines of a log shown when a step fails, and the width of each.
const TAIL_LINES: usize = 30;
const MAX_LINE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepName {
    Scope,
    Dependencies,
    Format,
    Lint,
    Typecheck,
    ReproTest,
    Test,
    Coverage,
    Sonar,
    /// The steps did not rewrite the files they checked.
    Integrity,
}

impl StepName {
    pub const ALL: [Self; 10] = [
        Self::Scope,
        Self::Dependencies,
        Self::Format,
        Self::Lint,
        Self::Typecheck,
        Self::ReproTest,
        Self::Test,
        Self::Coverage,
        Self::Sonar,
        Self::Integrity,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Scope => "scope",
            Self::Dependencies => "dependencies",
            Self::Format => "format",
            Self::Lint => "lint",
            Self::Typecheck => "typecheck",
            Self::ReproTest => "repro_test",
            Self::Test => "test",
            Self::Coverage => "coverage",
            Self::Sonar => "sonar",
            Self::Integrity => "integrity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    NotInProfile,
    DisabledByRepo,
    NoDependencyChange,
    NoReproTest,
    /// The profile's command does not run on this repo (#228); the repo
    /// can give its own.
    NotForRepo,
    /// The request names no Sonar server.
    NotConfigured,
    /// An earlier step failed.
    NotRun,
    Cancelled,
}

impl Skip {
    fn text(self) -> &'static str {
        match self {
            Self::NotInProfile => "skipped: the profile has no such step",
            Self::DisabledByRepo => "skipped: turned off by the repo",
            Self::NoDependencyChange => "skipped: no manifest or lockfile changed",
            Self::NoReproTest => "skipped: no reproduction test given",
            Self::NotForRepo => {
                "skipped: the profile's command does not run on this repo; the repo can give its own"
            }
            Self::NotConfigured => "skipped: not configured",
            Self::NotRun => "not run",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Passed,
    Failed,
    Skipped(Skip),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub name: StepName,
    pub status: StepStatus,
    pub duration: Duration,
    /// Why it failed, for the agent: the reason, failing tests, log tail.
    pub detail: String,
    /// Full logs of the commands it ran.
    pub logs: Vec<PathBuf>,
}

impl StepReport {
    pub fn skipped(name: StepName, skip: Skip) -> Self {
        Self {
            name,
            status: StepStatus::Skipped(skip),
            duration: Duration::ZERO,
            detail: String::new(),
            logs: vec![],
        }
    }
}

/// Reference outputs the gate rewrote because the agent declared that their
/// expected output had to change (#105, #147); the pull request lists them
/// for the reviewer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatedReferences {
    /// Test names the update command was given.
    pub tests: Vec<String>,
    /// Files it wrote.
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    pub steps: Vec<StepReport>,
    /// Git tree id of exactly the files the gate checked; the pull request
    /// commits this tree, not whatever the working tree holds later.
    pub tree: String,
    /// Full commit id of the base revision the patch was compared with;
    /// the pull request's commit goes on top of it.
    pub base: String,
    /// Empty unless the test step passed by updating references.
    pub references: UpdatedReferences,
    /// Directories of the parts the gate checked, the rest of the repo
    /// (`""`) first (#71): their coverage reports are this run's.
    pub parts: Vec<String>,
    /// Scope warnings detected by the scope step (#359) to flag reviewers on the PR.
    pub scope_warnings: Vec<String>,
}

impl GateReport {
    pub fn passed(&self) -> bool {
        self.steps.iter().all(|s| match s.status {
            StepStatus::Passed => true,
            StepStatus::Failed => false,
            StepStatus::Skipped(skip) => !matches!(skip, Skip::NotRun | Skip::Cancelled),
        })
    }

    pub fn cancelled(&self) -> bool {
        self.steps
            .iter()
            .any(|s| s.status == StepStatus::Skipped(Skip::Cancelled))
    }

    pub fn step(&self, name: StepName) -> Option<&StepReport> {
        self.steps.iter().find(|s| s.name == name)
    }

    fn failed(&self) -> Option<&StepReport> {
        self.steps.iter().find(|s| s.status == StepStatus::Failed)
    }
}

/// Bytes read from the end of a log to find its last lines.
const TAIL_BYTES: u64 = 64 * 1024;

/// The last `bytes` of a file, as text.
fn read_end(path: &Path, bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(bytes)));
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// The last lines of a log, each cut to a readable width.
pub fn tail(log: &Path) -> String {
    let text = read_end(log, TAIL_BYTES);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..]
        .iter()
        .map(|l| match l.char_indices().nth(MAX_LINE_CHARS) {
            Some((i, _)) => format!("{}…", &l[..i]),
            None => (*l).to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl fmt::Display for GateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.cancelled(), self.failed()) {
            (true, _) => f.write_str("Gate cancelled.")?,
            (false, Some(step)) => write!(f, "Gate failed at {}.", step.name.id())?,
            (false, None) => f.write_str("Gate passed.")?,
        }
        for step in &self.steps {
            let status = match step.status {
                StepStatus::Passed => format!("passed ({}s)", step.duration.as_secs()),
                StepStatus::Failed => format!("FAILED ({}s)", step.duration.as_secs()),
                StepStatus::Skipped(skip) => skip.text().to_owned(),
            };
            write!(f, "\n  {:<13}{status}", step.name.id())?;
        }
        if let Some(step) = self.failed() {
            write!(f, "\n--- {} ---\n{}", step.name.id(), step.detail)?;
        }
        Ok(())
    }
}
