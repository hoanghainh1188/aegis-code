//! The module map of a repo for the system prompt (BP2, design §4.1): its
//! source directories and the main definitions in each, read with the
//! profile's symbol query. Built once per repo and kept in a file, so every
//! issue on the repo sends the same bytes and the cached prefix holds
//! (design §4.4). Test files are left out, and each directory names its
//! public definitions first, and of each kind the most used first (#269).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path};

use aegis_lang::{Language, Symbol, SymbolKind, is_test_path, symbols_in};

use crate::{AciError, Workspace};

/// The map's format: a stored map of an older one is not read, so a change
/// of what the map shows takes effect on code already mapped.
pub const MAP_FORMAT: u32 = 2;

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
    /// Each name once: whether one of its definitions is public, and its
    /// best rank.
    names: BTreeMap<String, (bool, u8)>,
}

impl Group {
    fn add(&mut self, name: String, public: bool, rank: u8) {
        let entry = self.names.entry(name).or_insert((public, rank));
        *entry = (entry.0 || public, entry.1.min(rank));
    }

    /// Public first, then by kind, then the most used, then by name.
    fn ranked(&self, uses: &HashMap<String, usize>) -> Vec<&str> {
        let mut names: Vec<(&String, &(bool, u8))> = self.names.iter().collect();
        names.sort_by_key(|(name, (public, rank))| {
            let used = uses.get(name.as_str()).copied().unwrap_or(0);
            (!public, *rank, Reverse(used), name.as_str())
        });
        names.into_iter().map(|(n, _)| n.as_str()).collect()
    }
}

/// Whether the definition can be used outside its module, by its
/// language's rule.
fn is_public(language: Language, symbol: &Symbol) -> bool {
    let signature = symbol.signature.as_str();
    match language {
        Language::Rust => signature.starts_with("pub ") || signature.starts_with("pub("),
        Language::Python => !symbol.name.starts_with('_'),
        Language::Go => symbol.name.starts_with(|c: char| c.is_uppercase()),
        Language::Java | Language::CSharp => signature.split_whitespace().any(|w| w == "public"),
        Language::TypeScript => signature.starts_with("export"),
    }
}

/// Counts the files each identifier appears in: how widely a name is used,
/// its own file included. Per file rather than per occurrence, so a name
/// that is also a common word (`and`, `f`) in comments and locals of a few
/// files does not come first.
fn count_identifiers(source: &str, uses: &mut HashMap<String, usize>) {
    source
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with(|c: char| !c.is_ascii_digit()))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .for_each(|w| *uses.entry(w.to_owned()).or_default() += 1);
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
    let mut uses: HashMap<String, usize> = HashMap::new();
    let (mut test_files, mut test_dirs) = (0, BTreeSet::new());
    for path in &files {
        let Some(profile) = ws.profile_for(path).map(|p| &p.profile) else {
            continue;
        };
        let relative = path.strip_prefix(ws.root()).unwrap_or(path);
        let source = ws.read_text(path);
        // Uses in tests count too: what tests call is what matters.
        if let Some(source) = &source {
            count_identifiers(source, &mut uses);
        }
        if is_test_path(&relative.to_string_lossy()) {
            test_files += 1;
            test_dirs.insert(group_of(relative));
            continue;
        }
        let group = groups.entry(group_of(relative)).or_default();
        group.files += 1;
        let Some(source) = source else {
            continue;
        };
        let Ok(symbols) = symbols_in(profile.language, Some(path), &source) else {
            continue;
        };
        for s in symbols.into_iter().filter(|s| s.container.is_none()) {
            if let Some(r) = rank(s.kind) {
                let public = is_public(profile.language, &s);
                group.add(s.name, public, r);
            }
        }
    }
    let mut out = HEADER.to_owned();
    for (shown, (dir, group)) in groups.iter().enumerate() {
        let ranked = group.ranked(&uses);
        let names = &ranked[..ranked.len().min(MAX_NAMES)];
        let more = ranked.len() - names.len();
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
    if test_files > 0 {
        out.push_str(&format!(
            "\n- Tests: {test_files} files in {} directories, not listed",
            test_dirs.len()
        ));
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
