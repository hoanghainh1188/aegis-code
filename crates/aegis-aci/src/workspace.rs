//! The repo the agent works on. Every path a tool takes is relative to its
//! root and is refused when it leaves it: no `..`, no absolute paths, no
//! symlink that resolves outside, nothing under `.git`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use aegis_gate::{Hardening, Limits};
use aegis_lang::{MAX_SOURCE_BYTES, ProfilePart, RepoProfiles, ResolvedProfile};

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

/// Where `edit.format` runs when the gate runs in containers (#104): the
/// profile's image, so the machine needs no formatter and the repo's
/// formatter config runs with no network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatContainer {
    pub docker: String,
    pub image: String,
    /// (host directory, path in the container), read-only: the gate's
    /// dependency dirs, so the formatter is the version the gate checks
    /// with (a repo's pinned one in its `.venv`).
    pub mounts: Vec<(String, String)>,
    /// The run's `[run.sandbox_limits]` (#320).
    pub limits: Limits,
    /// `limits` with what the profile of `image` sets, and where its tools
    /// write ([`Self::for_profile`]).
    hardening: Hardening,
}

impl FormatContainer {
    pub fn new(
        docker: String,
        image: String,
        mounts: Vec<(String, String)>,
        limits: Limits,
    ) -> Self {
        Self {
            docker,
            image,
            mounts,
            limits,
            hardening: Hardening {
                limits,
                ..Hardening::default()
            },
        }
    }

    /// Locked down as the gate's containers of `profile` are.
    fn for_profile(mut self, profile: &ResolvedProfile) -> Self {
        self.hardening = Hardening::new(self.limits, &profile.profile.sandbox);
        self
    }

    pub fn hardening(&self) -> &Hardening {
        &self.hardening
    }
}

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    /// The repo's profile, and its directories' own (#71).
    profiles: RepoProfiles,
    container: Option<FormatContainer>,
    /// Each part's dependency dirs, as its gate steps mount them (#198):
    /// (host directory, path in the container), by the part's `dir`.
    part_mounts: BTreeMap<String, Vec<(String, String)>>,
    /// Directory names skipped when walking: `.git` and the profiles'
    /// build caches (`target`, `.venv`…).
    skipped_dirs: Vec<String>,
}

impl Workspace {
    pub fn new(root: &Path, profiles: impl Into<RepoProfiles>) -> Result<Self, AciError> {
        let profiles = profiles.into();
        let root = root.canonicalize().map_err(|e| AciError::io(root, &e))?;
        if !root.is_dir() {
            return Err(AciError::NotADirectory(root.display().to_string()));
        }
        let mut skipped_dirs: Vec<String> = HIDDEN_DIRS
            .iter()
            .map(|d| (*d).to_owned())
            .chain(
                profiles
                    .parts()
                    .iter()
                    .flat_map(|p| p.profile.profile.sandbox.cache_dirs.iter())
                    .filter(|d| !d.contains('/'))
                    .cloned(),
            )
            .collect();
        skipped_dirs.dedup();
        Ok(Self {
            root,
            profiles,
            container: None,
            part_mounts: BTreeMap::new(),
            skipped_dirs,
        })
    }

    /// Runs `edit.format` in `container` instead of on the machine.
    pub fn with_format_container(self, container: FormatContainer) -> Self {
        Self {
            container: Some(container.for_profile(self.profiles.root())),
            ..self
        }
    }

    /// Mounts `mounts[dir]` in the containers of the part at `dir`.
    pub fn with_part_mounts(self, mounts: BTreeMap<String, Vec<(String, String)>>) -> Self {
        Self {
            part_mounts: mounts,
            ..self
        }
    }

    /// What the containers of the part at `dir` mount, read-only: its
    /// dependency dirs and caches (the root's are the format container's).
    pub fn part_mounts(&self, dir: &str) -> &[(String, String)] {
        self.part_mounts.get(dir).map_or(&[], Vec::as_slice)
    }

    pub fn format_container(&self) -> Option<&FormatContainer> {
        self.container.as_ref()
    }

    /// Where `part`'s formatter runs in a container: the root's container,
    /// or one of the part's image with the part's mounts (#71, #198).
    pub fn format_container_of(&self, part: &ProfilePart) -> Option<FormatContainer> {
        let root = self.container.as_ref()?;
        Some(match part.dir.is_empty() {
            true => root.clone(),
            false => FormatContainer::new(
                root.docker.clone(),
                part.profile.image.clone(),
                self.part_mounts(&part.dir).to_vec(),
                root.limits,
            )
            .for_profile(&part.profile),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The repo's own profile, for the whole tree.
    pub fn profile(&self) -> &ResolvedProfile {
        self.profiles.root()
    }

    pub fn profiles(&self) -> &RepoProfiles {
        &self.profiles
    }

    /// The part of the repo holding `path`.
    pub fn part_of(&self, path: &Path) -> &ProfilePart {
        self.profiles.part_of(&self.display(path))
    }

    /// The profile `path` is read with as source (its grammar), if any.
    pub fn profile_for(&self, path: &Path) -> Option<&ResolvedProfile> {
        self.profiles.for_file(&self.display(path))
    }

    /// The part a test given to `run_tests` belongs to, and its name there:
    /// a test named by a path inside a part's directory (`web/a.test.ts`)
    /// is that part's; any other the root's.
    pub fn part_of_test<'t>(&self, test: &'t str) -> (&ProfilePart, &'t str) {
        let part = self.profiles.part_of(test);
        match part.relative(test) {
            Some(rest) if !part.dir.is_empty() && !rest.is_empty() => (part, rest),
            _ => (&self.profiles.parts()[0], test),
        }
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

    /// Absolute path for a new file at `relative`; missing parent
    /// directories are left for the caller to create. Refused when the
    /// path leaves the root (also through a symlinked directory) or already
    /// exists.
    pub fn resolve_new(&self, relative: &str) -> Result<PathBuf, AciError> {
        let outside = || AciError::OutsideWorkspace(relative.to_owned());
        let path = Path::new(relative.trim());
        if path.is_absolute() || !plain_inside(path) || path.file_name().is_none() {
            return Err(outside());
        }
        let joined = self.root.join(path);
        if std::fs::symlink_metadata(&joined).is_ok() {
            return Err(AciError::InvalidInput(format!(
                "`{relative}` already exists; use `edit` to change it"
            )));
        }
        // The deepest directory that exists must resolve inside the root.
        let mut existing = joined.parent().map(Path::to_path_buf).ok_or_else(outside)?;
        let mut missing = Vec::new();
        while std::fs::symlink_metadata(&existing).is_err() {
            missing.push(existing.file_name().ok_or_else(outside)?.to_owned());
            existing = existing
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(outside)?;
        }
        let real = existing
            .canonicalize()
            .map_err(|e| AciError::io(&existing, &e))?;
        match real.strip_prefix(&self.root) {
            Ok(inside) if plain_inside(inside) && real.is_dir() => {}
            _ => return Err(outside()),
        }
        let mut dir = real;
        for name in missing.into_iter().rev() {
            dir.push(name);
        }
        Ok(dir.join(path.file_name().ok_or_else(outside)?))
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
