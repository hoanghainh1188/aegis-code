//! The module map of a repo for the system prompt (BP2, design §4.1): its
//! source directories and the main definitions in each, read with the
//! profile's symbol query. Built once per repo and kept in a file, so every
//! issue on the repo sends the same bytes and the cached prefix holds
//! (design §4.4).

use std::collections::BTreeMap;
use std::path::{Component, Path};

use aegis_lang::{SymbolKind, symbols_in};

use crate::{AciError, Workspace};

/// The map stays small next to the rest of the prompt.
const MAX_CHARS: usize = 16_000;

/// Definitions named per directory.
const MAX_NAMES: usize = 12;

/// Files are grouped by their first directories, at most this deep.
const DEPTH: usize = 4;

const HEADER: &str = "# Module map\n\nSource directories of this repository and the main \
definitions in each, generated from the code. Use it to decide where to look first.\n";

/// Kinds shown, in this order: types first, then free functions.
fn rank(kind: SymbolKind) -> Option<u8> {
    Some(match kind {
        SymbolKind::Struct | SymbolKind::Class => 0,
        SymbolKind::Enum | SymbolKind::Union => 1,
        SymbolKind::Trait | SymbolKind::Interface => 2,
        SymbolKind::TypeAlias => 3,
        SymbolKind::Macro => 4,
        SymbolKind::Function => 5,
        _ => return None,
    })
}

#[derive(Default)]
struct Group {
    files: usize,
    /// (rank, name), deduplicated by the map.
    names: BTreeMap<(u8, String), ()>,
}

fn group_of(relative: &Path) -> String {
    let dirs: Vec<String> = relative
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .take(DEPTH)
        .collect();
    if dirs.is_empty() {
        ".".to_owned()
    } else {
        dirs.join("/")
    }
}

/// The module map of the workspace's repo, in path order.
pub fn module_summary(ws: &Workspace) -> Result<String, AciError> {
    let (files, _) = ws.files_under(ws.root())?;
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for path in &files {
        let Some(profile) = ws.profile_for(path).map(|p| &p.profile) else {
            continue;
        };
        let relative = path.strip_prefix(ws.root()).unwrap_or(path);
        let group = groups.entry(group_of(relative)).or_default();
        group.files += 1;
        let Some(source) = ws.read_text(path) else {
            continue;
        };
        let Ok(symbols) = symbols_in(profile.language, Some(path), &source) else {
            continue;
        };
        for s in symbols.into_iter().filter(|s| s.container.is_none()) {
            if let Some(r) = rank(s.kind) {
                group.names.insert((r, s.name), ());
            }
        }
    }
    let mut out = HEADER.to_owned();
    for (shown, (dir, group)) in groups.iter().enumerate() {
        let names: Vec<&str> = group
            .names
            .keys()
            .take(MAX_NAMES)
            .map(|(_, n)| n.as_str())
            .collect();
        let more = group.names.len().saturating_sub(MAX_NAMES);
        let mut line = format!("\n- `{dir}` ({} files)", group.files);
        if !names.is_empty() {
            line.push_str(": ");
            line.push_str(&names.join(", "));
            if more > 0 {
                line.push_str(&format!(", … {more} more"));
            }
        }
        if out.len() + line.len() > MAX_CHARS {
            out.push_str(&format!("\n- … {} more directories", groups.len() - shown));
            break;
        }
        out.push_str(&line);
    }
    out.push('\n');
    Ok(out)
}

/// The map kept at `path`, built and saved there the first time. `path`
/// must be outside the repo, or it would become part of a patch.
pub fn load_or_create_summary(ws: &Workspace, path: &Path) -> Result<String, AciError> {
    let shown = path.display().to_string();
    let parent = path
        .parent()
        .ok_or_else(|| AciError::InvalidInput(format!("`{shown}` has no parent directory")))?;
    std::fs::create_dir_all(parent).map_err(|e| AciError::io(parent, &e))?;
    let real_parent = parent
        .canonicalize()
        .map_err(|e| AciError::io(parent, &e))?;
    if real_parent.starts_with(ws.root()) {
        return Err(AciError::InvalidInput(format!(
            "the module map `{shown}` must be stored outside the repo"
        )));
    }
    if path.exists() {
        return std::fs::read_to_string(path).map_err(|e| AciError::io(path, &e));
    }
    let summary = module_summary(ws)?;
    let scratch = path.with_extension("tmp");
    std::fs::write(&scratch, &summary).map_err(|e| AciError::io(&scratch, &e))?;
    std::fs::rename(&scratch, path).map_err(|e| AciError::io(path, &e))?;
    Ok(summary)
}
