//! `search_symbol`, `search_text`, `find_file` (design §3.1). At most
//! [`MAX_RESULTS`] hits; past that the agent is told to narrow the search
//! instead of getting a cut list.

use std::fmt;
use std::path::Path;

use aegis_lang::{Symbol, glob_match, symbols_in};

use crate::{AciError, Workspace};

pub const MAX_RESULTS: usize = 50;

/// Matched lines longer than this are cut with `…`.
const MAX_LINE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hits<T> {
    Found(Vec<T>),
    TooMany { total: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult<T> {
    pub query: String,
    pub hits: Hits<T>,
    /// Files or directories that could not be read as source (binary, too
    /// large, unreadable, not parsable).
    pub skipped_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolHit {
    pub path: String,
    pub symbol: Symbol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    pub path: String,
}

fn non_empty(what: &str, value: &str) -> Result<String, AciError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AciError::InvalidInput(format!("{what} is empty")));
    }
    Ok(value.to_owned())
}

fn collect<T>(query: String, hits: Vec<T>, skipped_files: usize) -> SearchResult<T> {
    let hits = if hits.len() > MAX_RESULTS {
        Hits::TooMany { total: hits.len() }
    } else {
        Hits::Found(hits)
    };
    SearchResult {
        query,
        hits,
        skipped_files,
    }
}

/// `name`, or `Container::name` / `Container.name` to pick a method.
fn symbol_matches(query: &str, symbol: &Symbol) -> bool {
    let qualified = query
        .rsplit_once("::")
        .or_else(|| query.rsplit_once('.'))
        .filter(|(c, n)| !c.is_empty() && !n.is_empty());
    match qualified {
        Some((container, name)) => {
            symbol.name == name && symbol.container.as_deref() == Some(container)
        }
        None => symbol.name == query,
    }
}

pub fn search_symbol(ws: &Workspace, name: &str) -> Result<SearchResult<SymbolHit>, AciError> {
    let query = non_empty("symbol name", name)?;
    let profile = &ws.profile().profile;
    let mut hits = Vec::new();
    let (files, mut skipped) = ws.files_under(ws.root())?;
    for path in files {
        if !profile.owns_file(&path) {
            continue;
        }
        let Some(source) = ws.read_text(&path) else {
            skipped += 1;
            continue;
        };
        let Ok(found) = symbols_in(profile.language, Some(&path), &source) else {
            skipped += 1;
            continue;
        };
        hits.extend(
            found
                .into_iter()
                .filter(|s| symbol_matches(&query, s))
                .map(|symbol| SymbolHit {
                    path: ws.display(&path),
                    symbol,
                }),
        );
    }
    Ok(collect(query, hits, skipped))
}

fn cut(line: &str) -> String {
    let line = line.trim_end();
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

/// Literal, case-sensitive match on each line of the text files in `dir`.
pub fn search_text(
    ws: &Workspace,
    text: &str,
    dir: Option<&str>,
) -> Result<SearchResult<TextHit>, AciError> {
    let query = non_empty("search text", text)?;
    let base = ws.resolve(dir.unwrap_or("."))?;
    let mut hits = Vec::new();
    let (files, mut skipped) = ws.files_under(&base)?;
    for path in files {
        let Some(source) = ws.read_text(&path) else {
            skipped += 1;
            continue;
        };
        hits.extend(
            source
                .lines()
                .enumerate()
                .filter(|(_, l)| l.contains(&query))
                .map(|(i, l)| TextHit {
                    path: ws.display(&path),
                    line: i + 1,
                    text: cut(l),
                }),
        );
    }
    Ok(collect(query, hits, skipped))
}

/// File names matching `pattern`, where `*` matches any run of characters.
pub fn find_file(
    ws: &Workspace,
    pattern: &str,
    dir: Option<&str>,
) -> Result<SearchResult<FileHit>, AciError> {
    let query = non_empty("file name pattern", pattern)?;
    let base = ws.resolve(dir.unwrap_or("."))?;
    let (files, skipped) = ws.files_under(&base)?;
    let hits = files
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| glob_match(&query, n))
        })
        .map(|p| FileHit {
            path: ws.display(Path::new(&p)),
        })
        .collect();
    Ok(collect(query, hits, skipped))
}

fn kind_name(symbol: &Symbol) -> String {
    format!("{:?}", symbol.kind).to_lowercase()
}

impl fmt::Display for SymbolHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = &self.symbol;
        let name = match &s.container {
            Some(c) => format!("{c}::{}", s.name),
            None => s.name.clone(),
        };
        write!(
            f,
            "{}:{}-{} {} {name}: {}",
            self.path,
            s.start_line,
            s.end_line,
            kind_name(s),
            s.signature
        )
    }
}

impl fmt::Display for TextHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.text)
    }
}

impl fmt::Display for FileHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path)
    }
}

impl<T: fmt::Display> fmt::Display for SearchResult<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.hits {
            Hits::TooMany { total } => write!(
                f,
                "{total} matches for \"{}\", more than {MAX_RESULTS}. Narrow the search.",
                self.query
            )?,
            Hits::Found(hits) if hits.is_empty() => {
                write!(f, "No matches for \"{}\".", self.query)?
            }
            Hits::Found(hits) => {
                write!(f, "{} matches for \"{}\":", hits.len(), self.query)?;
                for hit in hits {
                    write!(f, "\n{hit}")?;
                }
            }
        }
        if self.skipped_files > 0 {
            write!(
                f,
                "\n({} files skipped: binary, too large, unreadable or not parsable)",
                self.skipped_files
            )?;
        }
        Ok(())
    }
}
