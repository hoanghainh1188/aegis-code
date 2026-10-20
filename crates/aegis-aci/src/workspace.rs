//! The repo the agent works on. Every path a tool takes is relative to its
//! root and is refused when it leaves it: no `..`, no absolute paths, no
//! symlink that resolves outside, nothing under `.git`.

use std::path::{Component, Path, PathBuf};

use aegis_lang::{MAX_SOURCE_BYTES, ResolvedProfile};

use crate::AciError;

/// Never searched, opened or edited (compared ignoring ASCII case, for
/// case-insensitive file systems).
const HIDDEN_DIRS: &[&str] = &[".git"];

/// Only plain names and `.`: no `..`, root or prefix, nothing hidden.
fn plain_inside(path: &Path) -> bool {
    path.components().all(|c| match c {
        Component::CurDir => true,
        Component::Normal(name) => !HIDDEN_DIRS.iter().any(|h| name.eq_ignore_ascii_case(h)),
        _ => false,
    })
}

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    profile: ResolvedProfile,
    /// Directory names skipped when walking: `.git` and the profile's
    /// build caches (`target`, `.venv`…).
    skipped_dirs: Vec<String>,
}

impl Workspace {
    pub fn new(root: &Path, profile: ResolvedProfile) -> Result<Self, AciError> {
        let root = root.canonicalize().map_err(|e| AciError::io(root, &e))?;
        if !root.is_dir() {
            return Err(AciError::NotADirectory(root.display().to_string()));
        }
        let skipped_dirs = HIDDEN_DIRS
            .iter()
            .map(|d| (*d).to_owned())
            .chain(
                profile
                    .profile
                    .sandbox
                    .cache_dirs
                    .iter()
                    .filter(|d| !d.contains('/'))
                    .cloned(),
            )
            .collect();
        Ok(Self {
            root,
            profile,
            skipped_dirs,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn profile(&self) -> &ResolvedProfile {
        &self.profile
    }

    /// Absolute path of an existing file or directory, `""` or `"."` for
    /// the root. Checked both as written and after symlinks are resolved,
    /// so a link cannot lead out of the root or into `.git`.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, AciError> {
        let outside = || AciError::OutsideWorkspace(relative.to_owned());
        let path = Path::new(relative.trim());
        if path.is_absolute() || !plain_inside(path) {
            return Err(outside());
        }
        let joined = self.root.join(path);
        let real = joined.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => AciError::NotFound(relative.to_owned()),
            _ => AciError::io(&joined, &e),
        })?;
        match real.strip_prefix(&self.root) {
            Ok(inside) if plain_inside(inside) => Ok(real),
            _ => Err(outside()),
        }
    }

    /// Path relative to the root with `/` separators, as tools print it.
    pub fn display(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Files under `dir` in path order, and how many directories could not
    /// be read. Symlinks are not followed, and skipped directories are not
    /// entered.
    pub fn files_under(&self, dir: &Path) -> Result<(Vec<PathBuf>, usize), AciError> {
        if !dir.is_dir() {
            return Err(AciError::NotADirectory(self.display(dir)));
        }
        let mut files = Vec::new();
        let mut unreadable = 0;
        let mut pending = vec![dir.to_path_buf()];
        while let Some(current) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                unreadable += 1;
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    unreadable += 1;
                    continue;
                };
                if kind.is_dir() {
                    let name = entry.file_name();
                    if !self
                        .skipped_dirs
                        .iter()
                        .any(|s| name.eq_ignore_ascii_case(s))
                    {
                        pending.push(entry.path());
                    }
                } else if kind.is_file() {
                    files.push(entry.path());
                }
            }
        }
        files.sort();
        Ok((files, unreadable))
    }

    /// Contents of a text file, or `None` when it is binary, too large or
    /// cannot be read (a search counts it as skipped).
    pub fn read_text(&self, path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > MAX_SOURCE_BYTES as u64 {
            return None;
        }
        String::from_utf8(std::fs::read(path).ok()?).ok()
    }

    /// Contents of a file the agent named; every failure is reported.
    pub fn read_named_text(&self, path: &Path) -> Result<String, AciError> {
        if !path.is_file() {
            return Err(AciError::NotAFile(self.display(path)));
        }
        let meta = std::fs::metadata(path).map_err(|e| AciError::io(path, &e))?;
        let not_text = || AciError::NotText {
            path: self.display(path),
            limit: MAX_SOURCE_BYTES,
        };
        if meta.len() > MAX_SOURCE_BYTES as u64 {
            return Err(not_text());
        }
        let bytes = std::fs::read(path).map_err(|e| AciError::io(path, &e))?;
        String::from_utf8(bytes).map_err(|_| not_text())
    }
}
