//! `find_definition` and `find_references` (#89), answered by the profile's
//! language server: it resolves what a name means (a method called through
//! a trait, one of several `parse` functions), which matching names cannot.
//! A position is a file and line with a column or the name on that line, or
//! a name alone when one definition has it.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::json;

use crate::lsp::{Located, Lsp, Position};
use crate::search::{self, Hits, MAX_RESULTS, TextHit};
use crate::{AciError, Workspace};

/// Matched lines longer than this are cut with `…`, as in the searches.
const MAX_LINE_CHARS: usize = 200;

/// Places outside the repo listed by name, at most.
const MAX_OUTSIDE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    Definition,
    References,
}

/// Where to look: `file` + `line` with `column` or `symbol`, or `symbol`
/// alone. Lines and columns are 1-based, columns in characters.
#[derive(Debug, Clone, Copy, Default)]
pub struct LookupAt<'a> {
    pub file: Option<&'a str>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub symbol: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupResult {
    pub lookup: Lookup,
    /// `path:line:column`, and the name there when given.
    pub at: String,
    pub hits: Vec<TextHit>,
    pub total: usize,
    /// Places in the standard library or dependencies, by path.
    pub outside: Vec<String>,
    pub outside_total: usize,
}

/// `name` as a whole word in `text`: the 1-based character column.
fn column_of(text: &str, name: &str) -> Option<usize> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name)
        .find(|(i, _)| {
            let before = text[..*i].chars().next_back();
            let after = text[i + name.len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        })
        .map(|(i, _)| text[..i].chars().count() + 1)
}

fn cut(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_LINE_CHARS {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_LINE_CHARS).collect();
    out.push('…');
    out
}

/// The name a symbol query ends with: `total` of `Cart::total`.
fn last_segment(symbol: &str) -> &str {
    symbol
        .rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(symbol)
}

pub(crate) struct Resolved {
    /// From the repo root.
    pub relative: String,
    line: usize,
    column: usize,
    label: Option<String>,
}

pub(crate) fn resolve(ws: &Workspace, at: LookupAt<'_>) -> Result<Resolved, AciError> {
    let symbol = at.symbol.map(str::trim).filter(|s| !s.is_empty());
    let (relative, line) = match (at.file, at.line, symbol) {
        (Some(file), Some(line), _) => (file.trim().to_owned(), line),
        (Some(_), None, _) => {
            return Err(AciError::InvalidInput(
                "give `line` with `file` (and `column`, or the `symbol` on that line)".into(),
            ));
        }
        (None, _, Some(name)) => {
            let found = search::search_symbol(ws, name)?;
            let hits = match found.hits {
                Hits::Found(hits) => hits,
                Hits::TooMany { total } => {
                    return Err(AciError::InvalidInput(format!(
                        "{total} definitions are named `{name}`; give `file` and `line`"
                    )));
                }
            };
            match hits.as_slice() {
                [] => {
                    return Err(AciError::InvalidInput(format!(
                        "no definition is named `{name}`; give `file`, `line` and `column`"
                    )));
                }
                [one] => (one.path.clone(), one.symbol.start_line),
                many => {
                    let list: Vec<String> = many.iter().take(10).map(|h| h.to_string()).collect();
                    return Err(AciError::InvalidInput(format!(
                        "{} definitions are named `{name}`; give `file` and `line` of one:\n{}",
                        many.len(),
                        list.join("\n")
                    )));
                }
            }
        }
        (None, _, None) => {
            return Err(AciError::InvalidInput(
                "give `file` and `line` with `column` or `symbol`, or a `symbol` alone".into(),
            ));
        }
    };
    let path = ws.resolve(&relative)?;
    let text = ws.read_named_text(&path)?;
    let line_text = line
        .checked_sub(1)
        .and_then(|i| text.lines().nth(i))
        .ok_or_else(|| {
            AciError::InvalidInput(format!(
                "`{relative}` has {} lines, not {line}",
                text.lines().count()
            ))
        })?;
    let column = match (at.column, symbol) {
        (Some(column), _) if column >= 1 && column <= line_text.chars().count() + 1 => column,
        (Some(column), _) => {
            return Err(AciError::InvalidInput(format!(
                "line {line} of `{relative}` has {} characters, not column {column}",
                line_text.chars().count()
            )));
        }
        (None, Some(name)) => column_of(line_text, last_segment(name)).ok_or_else(|| {
            AciError::InvalidInput(format!(
                "`{}` is not on line {line} of `{relative}`; give `column`",
                last_segment(name)
            ))
        })?,
        (None, None) => {
            return Err(AciError::InvalidInput(
                "give `column` or the `symbol` on that line".into(),
            ));
        }
    };
    Ok(Resolved {
        relative: ws.display(&path),
        line,
        column,
        label: symbol.map(str::to_owned),
    })
}

/// UTF-16 units before the 1-based character `column`, as LSP counts.
fn utf16_offset(text: &str, column: usize) -> u32 {
    text.chars()
        .take(column.saturating_sub(1))
        .map(char::len_utf16)
        .sum::<usize>() as u32
}

/// Asks `lsp`, the server of the part holding `target`, for `lookup`
/// there; `language` is the part's, for a file the server has not opened.
pub(crate) fn lookup(
    ws: &Workspace,
    lsp: &mut Lsp,
    language: &str,
    lookup: Lookup,
    target: Resolved,
) -> Result<LookupResult, AciError> {
    let path = ws.resolve(&target.relative)?;
    let text = ws.read_named_text(&path)?;
    let line_text = text.lines().nth(target.line - 1).unwrap_or_default();
    if !lsp.is_open(&target.relative) {
        lsp.sync(&target.relative, &text, language)?;
    }
    let position = Position {
        path: target.relative.clone(),
        line: (target.line - 1) as u32,
        character: utf16_offset(line_text, target.column),
    };
    let (method, extra) = match lookup {
        Lookup::Definition => ("textDocument/definition", json!({})),
        Lookup::References => (
            "textDocument/references",
            json!({"context": {"includeDeclaration": true}}),
        ),
    };
    let located = lsp.locations(method, &position, extra)?;

    let mut texts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut hits = Vec::new();
    let mut outside = Vec::new();
    for place in located {
        match place {
            Located::InRepo(p) => {
                let lines = texts.entry(p.path.clone()).or_insert_with(|| {
                    ws.resolve(&p.path)
                        .ok()
                        .and_then(|full| ws.read_text(&full))
                        .map(|t| t.lines().map(str::to_owned).collect())
                        .unwrap_or_default()
                });
                let line = p.line as usize + 1;
                hits.push(TextHit {
                    path: p.path,
                    line,
                    text: cut(lines.get(line - 1).map_or("", String::as_str)),
                });
            }
            Located::Outside(p) => outside.push(p),
        }
    }
    hits.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    hits.dedup_by(|a, b| a.path == b.path && a.line == b.line);
    outside.sort();
    outside.dedup();
    let (total, outside_total) = (hits.len(), outside.len());
    hits.truncate(MAX_RESULTS);
    outside.truncate(MAX_OUTSIDE);
    let mut at = format!("{}:{}:{}", target.relative, target.line, target.column);
    if let Some(label) = target.label {
        at.push_str(&format!(" (`{label}`)"));
    }
    Ok(LookupResult {
        lookup,
        at,
        hits,
        total,
        outside,
        outside_total,
    })
}

impl fmt::Display for LookupResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.lookup {
            Lookup::Definition => "definitions",
            Lookup::References => "references",
        };
        if self.total == 0 && self.outside_total == 0 {
            return write!(f, "No {what} found for {}.", self.at);
        }
        write!(f, "{} {what} in the repo for {}:", self.total, self.at)?;
        for hit in &self.hits {
            write!(f, "\n{hit}")?;
        }
        if self.total > self.hits.len() {
            write!(f, "\n… {} more not shown.", self.total - self.hits.len())?;
        }
        if self.outside_total > 0 {
            write!(
                f,
                "\n{} outside the repo (standard library, dependencies):",
                self.outside_total
            )?;
            for path in &self.outside {
                write!(f, "\n{path}")?;
            }
            if self.outside_total > self.outside.len() {
                write!(f, "\n…")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_words_and_utf16() {
        assert_eq!(column_of("    fn total(&self)", "total"), Some(8));
        assert_eq!(column_of("subtotal + total", "total"), Some(12));
        assert_eq!(column_of("totally", "total"), None);
        assert_eq!(utf16_offset("é𝄞x", 3), 3);
        assert_eq!(last_segment("Cart::total"), "total");
        assert_eq!(last_segment("cart.add"), "add");
    }
}
