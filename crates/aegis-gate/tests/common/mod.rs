//! A fixture repo under git: the base revision is committed, the patch is
//! left in the working tree, as the gate sees an agent's work.

#![allow(dead_code)] // each test binary uses a different part

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aegis_gate::{
    AuditPolicy, CancelToken, GateReport, GateRequest, ReferenceGroup, Repro, Sandbox, StepName,
    StepStatus, run_gate,
};
use aegis_lang::{Language, ProfileOverrides, Profiles, ResolvedProfile};

pub struct Repo {
    root: PathBuf,
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

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if ["target", "__pycache__", ".pytest_cache", ".venv"]
            .iter()
            .any(|s| name == *s)
        {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

impl Repo {
    /// `tests/fixtures/<name>` with `base_edits` committed as the base.
    pub fn new(name: &str, base_edits: &[(&str, &str, &str)]) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-gate-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        let repo = Self { root };
        copy_dir(&fixture, &repo.dir());
        repo.write(
            ".gitignore",
            "target/\n__pycache__/\n.pytest_cache/\nlcov.info\ncoverage.xml\n",
        );
        for (file, from, to) in base_edits {
            repo.replace(file, from, to);
        }
        git(&repo.dir(), &["init", "-q", "-b", "main"]);
        git(&repo.dir(), &["add", "-A"]);
        git(
            &repo.dir(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                "base",
            ],
        );
        repo
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join("repo")
    }

    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    pub fn write(&self, file: &str, text: &str) {
        let path = self.dir().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir().join(file)).unwrap()
    }

    pub fn replace(&self, file: &str, from: &str, to: &str) {
        let text = self.read(file);
        assert!(text.contains(from), "`{from}` not in {file}");
        self.write(file, &text.replacen(from, to, 1));
    }

    pub fn gate(&self, profile: &ResolvedProfile, repro: Option<&Repro>) -> GateReport {
        self.gate_with(
            profile,
            repro,
            Duration::from_secs(600),
            CancelToken::new(),
            vec![],
        )
    }

    pub fn gate_with(
        &self,
        profile: &ResolvedProfile,
        repro: Option<&Repro>,
        step_timeout: Duration,
        cancel: CancelToken,
        env: Vec<(String, String)>,
    ) -> GateReport {
        self.gate_in(profile, repro, step_timeout, cancel, env, None)
    }

    pub fn gate_in(
        &self,
        profile: &ResolvedProfile,
        repro: Option<&Repro>,
        step_timeout: Duration,
        cancel: CancelToken,
        env: Vec<(String, String)>,
        sandbox: Option<Sandbox>,
    ) -> GateReport {
        self.gate_full(
            profile,
            repro,
            step_timeout,
            cancel,
            env,
            sandbox,
            &[],
            &[],
            AuditPolicy::default(),
        )
    }

    /// The gate with the repo's own audit policy (#159).
    pub fn gate_auditing(&self, profile: &ResolvedProfile, audit: AuditPolicy) -> GateReport {
        self.gate_full(
            profile,
            None,
            Duration::from_secs(600),
            CancelToken::new(),
            vec![],
            None,
            &[],
            &[],
            audit,
        )
    }

    /// The gate with `tests` declared as allowed test changes (#105).
    pub fn gate_allowing(&self, profile: &ResolvedProfile, tests: &[&str]) -> GateReport {
        self.gate_full(
            profile,
            None,
            Duration::from_secs(600),
            CancelToken::new(),
            vec![],
            None,
            tests,
            &[],
            AuditPolicy::default(),
        )
    }

    /// The gate with `groups` declared as allowed reference changes (#147),
    /// each a directory and a test name pattern.
    pub fn gate_grouping(&self, profile: &ResolvedProfile, groups: &[(&str, &str)]) -> GateReport {
        let groups: Vec<ReferenceGroup> = groups
            .iter()
            .map(|(dir, tests)| ReferenceGroup {
                dir: (*dir).to_owned(),
                tests: (*tests).to_owned(),
            })
            .collect();
        self.gate_full(
            profile,
            None,
            Duration::from_secs(600),
            CancelToken::new(),
            vec![],
            None,
            &[],
            &groups,
            AuditPolicy::default(),
        )
    }

    /// Commits what is in the tree on `main`: more of the base.
    pub fn commit_base(&self, message: &str) {
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

    #[allow(clippy::too_many_arguments)]
    fn gate_full(
        &self,
        profile: &ResolvedProfile,
        repro: Option<&Repro>,
        step_timeout: Duration,
        cancel: CancelToken,
        env: Vec<(String, String)>,
        sandbox: Option<Sandbox>,
        allowed_tests: &[&str],
        reference_groups: &[ReferenceGroup],
        audit: AuditPolicy,
    ) -> GateReport {
        let allowed = BTreeSet::new();
        let dir = self.dir();
        let work = self.work();
        run_gate(&GateRequest {
            repo: &dir,
            base: "main",
            profile,
            allowed: &allowed,
            repro,
            work_dir: &work,
            env,
            step_timeout,
            cancel,
            sandbox,
            fetch: None,
            build_cache: None,
            allowed_tests: allowed_tests.iter().map(|t| (*t).to_owned()).collect(),
            reference_groups: reference_groups.to_vec(),
            sonar: None,
            audit,
            parts: vec![],
        })
        .unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn profile(language: Language, overrides: &str) -> ResolvedProfile {
    let overrides: ProfileOverrides = toml::from_str(overrides).unwrap();
    Profiles::builtin()
        .unwrap()
        .get(language)
        .unwrap()
        .resolve(&overrides)
        .unwrap()
}

/// Status of every step, in order, for one-line assertions.
pub fn statuses(report: &GateReport) -> Vec<(StepName, StepStatus)> {
    report
        .steps
        .iter()
        .map(|s| (s.name, s.status.clone()))
        .collect()
}

/// Whether every program can be started. When one is missing the test is
/// skipped locally, but fails when `AEGIS_REQUIRE_PROFILE_TOOLS=1` (CI).
pub fn tools_available(checks: &[&[&str]]) -> bool {
    let missing: Vec<String> = checks
        .iter()
        .filter(|argv| {
            !Command::new(argv[0])
                .args(&argv[1..])
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .map(|argv| argv.join(" "))
        .collect();
    if missing.is_empty() {
        return true;
    }
    assert!(
        std::env::var("AEGIS_REQUIRE_PROFILE_TOOLS").as_deref() != Ok("1"),
        "required tools missing: {missing:?}"
    );
    eprintln!("skipping: tools missing: {missing:?}");
    false
}
