//! Helpers for the tool tests: a throwaway copy of a fixture repo and the
//! built-in profile for its language.

#![allow(dead_code)] // each test binary uses a different part

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use aegis_aci::Aci;
use aegis_lang::{Language, ProfileOverrides, Profiles, ResolvedProfile};

pub fn profile(language: Language, overrides: &ProfileOverrides) -> ResolvedProfile {
    Profiles::builtin()
        .unwrap()
        .get(language)
        .unwrap()
        .resolve(overrides)
        .unwrap()
}

/// A temporary directory removed on drop; `repo()` is the workspace root.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    pub fn empty() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-aci-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("repo")).unwrap();
        Self { root }
    }

    /// A fresh copy of `tests/fixtures/<name>`.
    pub fn fixture(name: &str) -> Self {
        let scratch = Self::empty();
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        copy_dir(&from, &scratch.repo());
        scratch
    }

    /// The Rust fixture at the root and the Python one under `py/` (#71).
    pub fn two_languages() -> Self {
        let scratch = Self::fixture("rust");
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/python");
        copy_dir(&from, &scratch.repo().join("py"));
        scratch
    }

    pub fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// Next to the repo, outside the workspace.
    pub fn outside(&self) -> PathBuf {
        self.root.join("outside")
    }

    pub fn write(&self, relative: &str, text: &str) {
        let path = self.repo().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.repo().join(relative)).unwrap()
    }

    pub fn aci(&self, profile: ResolvedProfile) -> Aci {
        Aci::new(&self.repo(), profile).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[path = "../../../../tests/fixture_copy.rs"]
mod fixture_copy;
use fixture_copy::copy_fixture as copy_dir;

/// Whether the program can be started. When it is missing the test is
/// skipped locally, but fails when `AEGIS_REQUIRE_PROFILE_TOOLS=1` (CI).
pub fn tool_available(argv: &[&str]) -> bool {
    let ok = Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_PROFILE_TOOLS").as_deref() != Ok("1"),
            "required tool missing: {argv:?}"
        );
        eprintln!("skipping: tool missing: {argv:?}");
    }
    ok
}
