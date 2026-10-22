//! The repo's build cache (ADR 0003: each job has its own `target/`, started
//! from the repo's cache; #104). A cold typst build takes minutes in the
//! container, for the patched tree and again for the base copy. Both start
//! from a copy-on-write clone of the cache, so they share nothing on disk.
//! Only the base copy writes the cache back: it builds upstream code, never
//! the patch, so a patch cannot leave anything for a later job.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::GateError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCache {
    /// One subdirectory per cache directory, by name.
    pub dir: PathBuf,
    /// The profile's cache directories inside the tree (`target`).
    pub dirs: Vec<String>,
}

/// `src` to `dst`, sharing blocks where the file system can (APFS clones,
/// Linux reflinks), a plain copy otherwise.
fn clone_dir(src: &Path, dst: &Path) -> Result<(), GateError> {
    let attempts: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["-c", "-R"], &["-R"]]
    } else {
        &[&["-R", "--reflink=auto"], &["-R"]]
    };
    for flags in attempts {
        let _ = std::fs::remove_dir_all(dst);
        let ok = Command::new("cp")
            .args(*flags)
            .arg(src)
            .arg(dst)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Ok(());
        }
    }
    Err(GateError::Spawn {
        program: "cp".into(),
        message: format!("could not copy {} to {}", src.display(), dst.display()),
    })
}

impl BuildCache {
    /// Starts `tree`'s cache directories from the cache, where the tree has
    /// none yet.
    pub fn seed(&self, tree: &Path) -> Result<(), GateError> {
        for name in &self.dirs {
            let (from, to) = (self.dir.join(name), tree.join(name));
            if from.is_dir() && !to.exists() {
                clone_dir(&from, &to)?;
            }
        }
        Ok(())
    }

    /// Makes `tree`'s cache directories the cache: only ever the base copy's.
    pub fn refresh_from(&self, tree: &Path) -> Result<(), GateError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| GateError::io(&self.dir, &e))?;
        for name in &self.dirs {
            let from = tree.join(name);
            if !from.is_dir() {
                continue;
            }
            let fresh = self.dir.join(format!(".{name}.new"));
            clone_dir(&from, &fresh)?;
            let current = self.dir.join(name);
            let _ = std::fs::remove_dir_all(&current);
            std::fs::rename(&fresh, &current).map_err(|e| GateError::io(&current, &e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_starts_from_the_cache_and_the_cache_from_a_tree() {
        let root = std::env::temp_dir().join(format!("aegis-build-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cache = BuildCache {
            dir: root.join("cache"),
            dirs: vec!["target".into()],
        };
        let base = root.join("base");
        std::fs::create_dir_all(base.join("target/debug")).unwrap();
        std::fs::write(base.join("target/debug/lib.rlib"), "built").unwrap();
        cache.refresh_from(&base).unwrap();

        let job = root.join("job");
        std::fs::create_dir_all(&job).unwrap();
        cache.seed(&job).unwrap();
        assert_eq!(
            std::fs::read_to_string(job.join("target/debug/lib.rlib")).unwrap(),
            "built"
        );
        // The job's copy is its own: writing it leaves the cache as it was.
        std::fs::write(job.join("target/debug/lib.rlib"), "patched").unwrap();
        assert_eq!(
            std::fs::read_to_string(cache.dir.join("target/debug/lib.rlib")).unwrap(),
            "built"
        );
        // A tree that has the directory already keeps it.
        cache.seed(&job).unwrap();
        assert_eq!(
            std::fs::read_to_string(job.join("target/debug/lib.rlib")).unwrap(),
            "patched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
