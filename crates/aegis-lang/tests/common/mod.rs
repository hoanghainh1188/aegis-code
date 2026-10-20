//! Helpers for the fixture tests: a throwaway copy of a fixture repo and a
//! clean environment for the commands a profile runs.

#![allow(dead_code)] // each test binary uses a different part

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// Variables the outer `cargo test` / `cargo llvm-cov` set that must not
/// leak into the fixture's build or test run.
const INHERITED: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "LLVM_PROFILE_FILE",
    "CARGO_LLVM_COV",
    "CARGO_LLVM_COV_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
];

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// A fresh copy of `tests/fixtures/<name>`, with its own build dir.
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-fixture-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        copy_dir(&fixture(name), &root.join("repo"));
        Self { root }
    }

    pub fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    pub fn run(&self, argv: &[String]) -> Output {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(self.repo())
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .env("PYTHONDONTWRITEBYTECODE", "1")
            // Never download another Go toolchain for the fixture.
            .env("GOTOOLCHAIN", "local")
            // English, quiet output from the .NET CLI.
            .env("DOTNET_CLI_UI_LANGUAGE", "en")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_NOLOGO", "1");
        for var in INHERITED {
            cmd.env_remove(var);
        }
        cmd.output()
            .unwrap_or_else(|e| panic!("cannot run {argv:?}: {e}"))
    }

    pub fn run_ok(&self, step: &str, argv: &[String]) -> Output {
        let out = self.run(argv);
        assert!(
            out.status.success(),
            "{step} failed: {argv:?}\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            let skip = [
                "target",
                "__pycache__",
                ".pytest_cache",
                ".venv",
                "node_modules",
                "bin",
                "obj",
            ];
            if !skip.iter().any(|s| name == *s) {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
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
