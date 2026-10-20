//! `edit` (design §3.1): replace lines `from..=to`, check the result with
//! the profile's tree-sitter grammar, then run the profile's formatter on
//! it. A refused edit leaves the file as it was and shows the proposed
//! lines so the agent can fix them.

use std::fmt;
use std::path::Path;

use aegis_lang::{SyntaxError, has_grammar, syntax_errors_in};

use crate::formatter::{self, Failure};
use crate::viewer::write_numbered;
use crate::{AciError, Workspace};

/// Lines shown around the edited range.
pub const CONTEXT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub path: String,
    pub total: usize,
    pub first: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The edit adds tree-sitter errors.
    Syntax(Vec<SyntaxError>),
    /// The formatter exited with an error.
    Formatter { code: Option<i32>, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Applied(Snippet),
    Rejected {
        reason: Rejection,
        proposed: Snippet,
    },
}

/// What the agent asked for: lines `from..=to` (1-based) become `content`.
/// `to = from - 1` inserts before `from` without replacing anything.
#[derive(Debug, Clone, Copy)]
pub struct EditRequest<'a> {
    pub file: &'a str,
    pub from: usize,
    pub to: usize,
    pub content: &'a str,
}

struct Text {
    lines: Vec<String>,
    newline: &'static str,
    trailing: bool,
}

impl Text {
    fn parse(source: &str) -> Self {
        Self {
            lines: source.lines().map(str::to_owned).collect(),
            // The file's usual line ending, so one CRLF inside a string
            // does not rewrite every line.
            newline: if 2 * source.matches("\r\n").count() > source.matches('\n').count() {
                "\r\n"
            } else {
                "\n"
            },
            trailing: source.is_empty() || source.ends_with('\n'),
        }
    }

    fn render(&self) -> String {
        let mut out = self.lines.join(self.newline);
        if self.trailing && !self.lines.is_empty() {
            out.push_str(self.newline);
        }
        out
    }
}

fn check_range(path: &str, total: usize, from: usize, to: usize) -> Result<(), AciError> {
    if from == 0 || from > total.saturating_add(1) || to.saturating_add(1) < from || to > total {
        return Err(AciError::InvalidInput(format!(
            "lines {from}-{to} are not a range of `{path}` ({total} lines); \
             use 1 <= from <= to <= {total}, or to = from - 1 to insert"
        )));
    }
    Ok(())
}

/// Lines `start..=end` widened by [`CONTEXT`], clamped to the file.
fn snippet(path: &str, lines: &[String], start: usize, end: usize) -> Snippet {
    let first = start.saturating_sub(CONTEXT).max(1);
    let last = (end.max(start) + CONTEXT).min(lines.len());
    Snippet {
        path: path.to_owned(),
        total: lines.len(),
        first,
        lines: lines
            .get(first - 1..last)
            .map(<[String]>::to_vec)
            .unwrap_or_default(),
    }
}

/// Rejects the edit when it adds a tree-sitter error, or when an error
/// falls on the edited lines `start..=end`. A file tree-sitter already
/// misreads elsewhere can still be edited.
fn syntax_rejection(
    ws: &Workspace,
    path: &Path,
    before: &str,
    after: &str,
    (start, end): (usize, usize),
) -> Result<Option<Rejection>, AciError> {
    let profile = &ws.profile().profile;
    if !profile.owns_file(path) || !has_grammar(profile.language) {
        return Ok(None);
    }
    let errors = |s| syntax_errors_in(profile.language, Some(path), s);
    let after_errors = errors(after)?;
    let on_edit = after_errors
        .iter()
        .any(|e| e.line >= start && e.line <= end.max(start));
    if on_edit || after_errors.len() > errors(before)?.len() {
        return Ok(Some(Rejection::Syntax(after_errors)));
    }
    Ok(None)
}

fn write(path: &Path, text: &str) -> Result<(), AciError> {
    std::fs::write(path, text).map_err(|e| AciError::io(path, &e))
}

/// Puts back what was there: the old content, or no file at all.
fn restore(path: &Path, original: Option<&str>) -> Result<(), AciError> {
    match original {
        Some(text) => write(path, text),
        None => std::fs::remove_file(path).map_err(|e| AciError::io(path, &e)),
    }
}

/// Writes `text` to `path` and runs the profile's formatter on it in place,
/// so the formatter sees the file where it lives (its config, its
/// submodules). When the formatter refuses, `original` is put back
/// (`None`: the file did not exist).
fn write_formatted(
    ws: &Workspace,
    path: &Path,
    text: &str,
    original: Option<&str>,
) -> Result<Result<String, Rejection>, AciError> {
    let profile = ws.profile();
    let command = profile
        .edit_format
        .command()
        .filter(|_| profile.profile.owns_file(path));
    write(path, text)?;
    let Some(command) = command else {
        return Ok(Ok(text.to_owned()));
    };
    let result = formatter::run(command, ws.root(), &ws.display(path));
    let rejection = match result {
        Ok(Ok(())) => return ws.read_named_text(path).map(Ok),
        Ok(Err(Failure::Exit { code, message })) => Rejection::Formatter { code, message },
        Ok(Err(Failure::TimedOut)) => Rejection::Formatter {
            code: None,
            message: format!("timed out after {}s", formatter::TIMEOUT.as_secs()),
        },
        Err(e) => {
            restore(path, original)?;
            return Err(e);
        }
    };
    restore(path, original)?;
    Ok(Err(rejection))
}

/// `create_file`: a new file with `content`, checked like an edit. A
/// refused file is not left behind.
pub fn create(ws: &Workspace, file: &str, content: &str) -> Result<EditOutcome, AciError> {
    let path = ws.resolve_new(file)?;
    let shown = ws.display(&path);
    let lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let text = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    let proposed = || snippet(&shown, &lines, 1, lines.len().min(2 * CONTEXT));
    if let Some(reason) = syntax_rejection(ws, &path, "", &text, (1, lines.len()))? {
        return Ok(EditOutcome::Rejected {
            reason,
            proposed: proposed(),
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AciError::io(parent, &e))?;
    }
    match write_formatted(ws, &path, &text, None)? {
        Ok(formatted) => {
            let lines: Vec<String> = formatted.lines().map(str::to_owned).collect();
            Ok(EditOutcome::Applied(snippet(
                &shown,
                &lines,
                1,
                lines.len().min(2 * CONTEXT),
            )))
        }
        Err(reason) => Ok(EditOutcome::Rejected {
            reason,
            proposed: proposed(),
        }),
    }
}

pub fn edit(ws: &Workspace, request: EditRequest<'_>) -> Result<EditOutcome, AciError> {
    let EditRequest {
        file,
        from,
        to,
        content,
    } = request;
    let path = ws.resolve(file)?;
    let shown = ws.display(&path);
    let before = ws.read_named_text(&path)?;
    let mut text = Text::parse(&before);
    check_range(&shown, text.lines.len(), from, to)?;

    let new_lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let added = new_lines.len();
    text.lines.splice(from - 1..to, new_lines);
    let candidate = text.render();
    let end = from + added - 1;
    let proposed = || snippet(&shown, &text.lines, from, end);

    if let Some(reason) = syntax_rejection(ws, &path, &before, &candidate, (from, end))? {
        return Ok(EditOutcome::Rejected {
            reason,
            proposed: proposed(),
        });
    }
    let formatted = match write_formatted(ws, &path, &candidate, Some(&before))? {
        Ok(formatted) => formatted,
        Err(reason) => {
            return Ok(EditOutcome::Rejected {
                reason,
                proposed: proposed(),
            });
        }
    };
    let lines: Vec<String> = formatted.lines().map(str::to_owned).collect();
    // The formatter may grow or shrink the edited range; shift its end.
    let end = (end + lines.len()).saturating_sub(text.lines.len());
    Ok(EditOutcome::Applied(snippet(&shown, &lines, from, end)))
}

impl fmt::Display for Snippet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let last = self.first + self.lines.len();
        write!(
            f,
            "[File: {} ({} lines total), lines {}-{}]",
            self.path,
            self.total,
            self.first,
            last.saturating_sub(1).max(self.first)
        )?;
        write_numbered(f, self.first, &self.lines)
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(errors) => {
                f.write_str("the edit adds syntax errors:")?;
                for e in errors {
                    let what = if e.missing { "missing token" } else { "error" };
                    write!(f, "\n  line {}, column {}: {what}", e.line, e.column)?;
                }
                Ok(())
            }
            Self::Formatter { code, message } => {
                match code {
                    Some(c) => write!(f, "the formatter failed with exit code {c}")?,
                    None => f.write_str("the formatter was killed")?,
                }
                if !message.is_empty() {
                    write!(f, ":\n{message}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for EditOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied(snippet) => write!(f, "Edit applied.\n{snippet}"),
            Self::Rejected { reason, proposed } => write!(
                f,
                "Edit rejected, `{}` is unchanged: {reason}\nProposed lines:\n{proposed}",
                proposed.path
            ),
        }
    }
}
