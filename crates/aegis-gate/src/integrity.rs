//! The steps run the patch's code (build scripts, tests, conftest). The
//! last check makes sure none of it rewrote the files that were checked,
//! so what the gate passed is what goes into the pull request.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hasher};
use std::path::Path;

/// Content fingerprint per file; `None` when it is missing or unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot(BTreeMap<String, Option<(u64, u64)>>);

fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    hasher.write(&bytes);
    Some((bytes.len() as u64, hasher.finish()))
}

impl Snapshot {
    /// `files` relative to `repo`; `skip` (the coverage report, which a
    /// step writes) is left out.
    pub fn take(repo: &Path, files: &[String], skip: Option<&str>) -> Self {
        Self(
            files
                .iter()
                .filter(|f| Some(f.as_str()) != skip)
                .map(|f| (f.clone(), fingerprint(&repo.join(f))))
                .collect(),
        )
    }

    /// Files whose content differs now.
    pub fn changed_since(&self, repo: &Path) -> Vec<String> {
        self.0
            .iter()
            .filter(|(f, before)| fingerprint(&repo.join(f)) != **before)
            .map(|(f, _)| f.clone())
            .collect()
    }
}
