//! The git calls of the gate: which files the patch changed, what a file
//! was on the base revision, and a copy of the base revision to run the
//! same commands on.
//!
//! Git runs with no global or system config, no prompts, hooks, fsmonitor
//! or external diff. The changes are read through a scratch index built
//! from the base tree, so flags on the repo's own index (`skip-worktree`,
//! `assume-unchanged`) cannot hide a file. The base copy is written from
//! the object store with `cat-file`, so no checkout, filter or attribute
//! of the repo runs.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::GateError;

const SAFE: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.attributesFile=/dev/null",
    "-c",
    "core.excludesFile=/dev/null",
];

/// Mode of a submodule or nested repository in a tree.
const GITLINK: &str = "160000";
const SYMLINK: &str = "120000";
const EXECUTABLE: &str = "100755";

fn command(repo: &Path, args: &[&str], index: Option<&Path>) -> Command {
    let mut c = Command::new("git");
    c.env_clear()
        .envs(std::env::var_os("PATH").map(|p| ("PATH", p)))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(SAFE)
        .arg("-C")
        .arg(repo)
        .args(args);
    if let Some(index) = index {
        c.env("GIT_INDEX_FILE", index);
    }
    c
}

fn failed(args: &[&str], message: impl Into<String>) -> GateError {
    GateError::Git {
        args: args.join(" "),
        message: message.into(),
    }
}

fn git_ok(repo: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>, GateError> {
    let out = command(repo, args, index)
        .output()
        .map_err(|e| failed(args, e.to_string()))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(failed(args, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn split_z(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
}

/// Full commit id of `rev`, so the base cannot move while the gate runs.
pub fn resolve(repo: &Path, rev: &str) -> Result<String, GateError> {
    let spec = format!("{rev}^{{commit}}");
    let out = git_ok(
        repo,
        &["rev-parse", "--verify", "--end-of-options", &spec],
        None,
    )?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// One path the patch adds, changes or removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    /// The path is a submodule or nested repository on either side.
    pub gitlink: bool,
}

/// Paths the working tree changes against `base`, through a scratch index
/// at `index`: modified, added, deleted, both sides of a rename, new files
/// not ignored. Ignored files are listed by [`ignored`].
pub fn changes(repo: &Path, base: &str, index: &Path) -> Result<Vec<Change>, GateError> {
    let _ = std::fs::remove_file(index);
    git_ok(repo, &["read-tree", base], Some(index))?;
    git_ok(repo, &["add", "--all", "--", "."], Some(index))?;
    let raw = git_ok(
        repo,
        &[
            "diff",
            "--cached",
            "--raw",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            base,
        ],
        Some(index),
    )?;
    // `:<old mode> <new mode> <old sha> <new sha> <status>` NUL `<path>` NUL
    let mut fields = split_z(&raw);
    let mut changes = Vec::new();
    while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
        let mut modes = meta.trim_start_matches(':').split(' ');
        let gitlink = modes.next() == Some(GITLINK) || modes.next() == Some(GITLINK);
        changes.push(Change { path, gitlink });
    }
    Ok(changes)
}

/// Tree id of the scratch index after [`changes`]: exactly the files the
/// gate checked, for the pull request to commit.
pub fn write_tree(repo: &Path, index: &Path) -> Result<String, GateError> {
    let out = git_ok(repo, &["write-tree"], Some(index))?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// Files in the scratch index after [`changes`].
pub fn files(repo: &Path, index: &Path) -> Result<Vec<String>, GateError> {
    let out = git_ok(repo, &["ls-files", "-z"], Some(index))?;
    Ok(split_z(&out).collect())
}

/// Ignored files in the working tree. A whole ignored directory is listed
/// as one `dir/` entry only when its name is in `collapse` (the profile's
/// build caches); any other is opened, so a lockfile cannot hide in it.
pub fn ignored(repo: &Path, index: &Path, collapse: &[String]) -> Result<Vec<String>, GateError> {
    let list = |extra: &[&str]| -> Result<Vec<String>, GateError> {
        let mut args = vec![
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ];
        args.extend_from_slice(extra);
        Ok(split_z(&git_ok(repo, &args, Some(index))?).collect())
    };
    let mut files = Vec::new();
    for entry in list(&["--directory"])? {
        let Some(dir) = entry.strip_suffix('/') else {
            files.push(entry);
            continue;
        };
        let name = dir.rsplit('/').next().unwrap_or(dir);
        if collapse.iter().any(|c| c == name) {
            files.push(entry);
        } else {
            files.extend(list(&["--", dir])?);
        }
    }
    Ok(files)
}

/// `path` as it is on `base`, or `None` when it does not exist there.
pub fn show(repo: &Path, base: &str, path: &str) -> Result<Option<String>, GateError> {
    let object = format!("{base}:{path}");
    let exists = command(repo, &["cat-file", "-e", &object], None)
        .stderr(Stdio::null())
        .status()
        .map_err(|e| failed(&["cat-file"], e.to_string()))?;
    if !exists.success() {
        return Ok(None);
    }
    let bytes = git_ok(repo, &["cat-file", "blob", &object], None)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn plain(path: &str) -> bool {
    !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Writes the files of `base` under `dest` straight from the object store.
/// Submodules are left out; symlinks are written as links.
pub fn extract(repo: &Path, base: &str, dest: &Path) -> Result<(), GateError> {
    let listing = git_ok(repo, &["ls-tree", "-r", "-z", "--full-tree", base], None)?;
    // `<mode> <type> <sha>` TAB `<path>`
    let entries: Vec<(String, String, String)> = split_z(&listing)
        .filter_map(|e| {
            let (meta, path) = e.split_once('\t')?;
            let mut m = meta.split(' ');
            let (mode, kind, sha) = (m.next()?, m.next()?, m.next()?);
            (kind == "blob" && plain(path))
                .then(|| (mode.to_owned(), sha.to_owned(), path.to_owned()))
        })
        .collect();
    let args = ["cat-file", "--batch"];
    let mut child = command(repo, &args, None)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| failed(&args, e.to_string()))?;
    let mut stdin = child.stdin.take().expect("piped");
    let shas: Vec<String> = entries.iter().map(|(_, sha, _)| sha.clone()).collect();
    let writer = std::thread::spawn(move || {
        for sha in shas {
            if writeln!(stdin, "{sha}").is_err() {
                break;
            }
        }
    });
    let mut out = BufReader::new(child.stdout.take().expect("piped"));
    for (mode, _, path) in &entries {
        let mut header = String::new();
        out.read_line(&mut header)
            .map_err(|e| failed(&args, e.to_string()))?;
        let size: usize = header
            .trim_end()
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| failed(&args, format!("bad header `{}`", header.trim_end())))?;
        let mut content = vec![0u8; size + 1];
        out.read_exact(&mut content)
            .map_err(|e| failed(&args, e.to_string()))?;
        content.truncate(size);
        write_entry(&dest.join(path), mode, &content)?;
    }
    let _ = writer.join();
    let _ = child.wait();
    Ok(())
}

fn write_entry(target: &Path, mode: &str, content: &[u8]) -> Result<(), GateError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| GateError::io(parent, &e))?;
    }
    if mode == SYMLINK {
        #[cfg(unix)]
        {
            let link = String::from_utf8_lossy(content).into_owned();
            return std::os::unix::fs::symlink(OsStr::new(&link), target)
                .map_err(|e| GateError::io(target, &e));
        }
    }
    std::fs::write(target, content).map_err(|e| GateError::io(target, &e))?;
    #[cfg(unix)]
    if mode == EXECUTABLE {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| GateError::io(target, &e))?;
    }
    Ok(())
}

/// A copy of the base revision, removed on drop.
#[derive(Debug)]
pub struct BaseTree {
    path: PathBuf,
}

impl BaseTree {
    pub fn create(repo: &Path, base: &str, path: &Path) -> Result<Self, GateError> {
        if path.exists() {
            std::fs::remove_dir_all(path).map_err(|e| GateError::io(path, &e))?;
        }
        std::fs::create_dir_all(path).map_err(|e| GateError::io(path, &e))?;
        let tree = Self {
            path: path.to_path_buf(),
        };
        extract(repo, base, path)?;
        Ok(tree)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Puts `files` back as they are on `base`: rewritten, or removed when
    /// the base does not have them.
    pub fn restore(&self, repo: &Path, base: &str, files: &[String]) -> Result<(), GateError> {
        for file in files {
            let target = self.path.join(file);
            let object = format!("{base}:{file}");
            let exists = command(repo, &["cat-file", "-e", &object], None)
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if exists {
                let content = git_ok(repo, &["cat-file", "blob", &object], None)?;
                std::fs::write(&target, content).map_err(|e| GateError::io(&target, &e))?;
            } else if target.exists() {
                std::fs::remove_file(&target).map_err(|e| GateError::io(&target, &e))?;
            }
        }
        Ok(())
    }
}

impl Drop for BaseTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
