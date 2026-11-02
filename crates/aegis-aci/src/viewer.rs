//! `open`, `scroll_up`, `scroll_down`, `goto` (design §3.1): one file at a
//! time, shown [`WINDOW`] numbered lines at a time with the count of hidden
//! lines above and below.

use std::fmt;

use crate::secrets::{MARKER, is_sensitive_path};
use crate::{AciError, Workspace};

pub const WINDOW: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub path: String,
    pub total: usize,
    /// 1-based number of the first line shown.
    pub first: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Viewer {
    open: Option<OpenFile>,
}

#[derive(Debug)]
struct OpenFile {
    path: String,
    lines: Vec<String>,
    first: usize,
}

impl OpenFile {
    /// Highest first line that still fills a whole window.
    fn last_first(&self) -> usize {
        self.lines.len().saturating_sub(WINDOW) + 1
    }

    fn check_line(&self, line: usize) -> Result<(), AciError> {
        let total = self.lines.len().max(1);
        if line == 0 || line > total {
            return Err(AciError::InvalidInput(format!(
                "line {line} is outside `{}` (lines 1-{total})",
                self.path
            )));
        }
        Ok(())
    }

    /// Show `line` near the middle of the window.
    fn center_on(&mut self, line: usize) {
        self.first = line.saturating_sub(WINDOW / 2).clamp(1, self.last_first());
    }

    fn window(&self) -> Window {
        let start = self.first - 1;
        let end = (start + WINDOW).min(self.lines.len());
        Window {
            path: self.path.clone(),
            total: self.lines.len(),
            first: self.first,
            lines: self.lines[start..end].to_vec(),
        }
    }
}

impl Viewer {
    pub fn open(
        &mut self,
        ws: &Workspace,
        file: &str,
        line: Option<usize>,
    ) -> Result<Window, AciError> {
        let path = ws.resolve(file)?;
        let text = ws.read_named_text(&path)?;
        let display_path = ws.display(&path);
        let lines: Vec<String> = if is_sensitive_path(&display_path) {
            let total = text.lines().count().max(1);
            vec![format!("{MARKER}sensitive-file]"); total]
        } else {
            text.lines().map(str::to_owned).collect()
        };
        let mut open = OpenFile {
            path: display_path,
            lines,
            first: 1,
        };
        let line = line.unwrap_or(1);
        open.check_line(line)?;
        open.center_on(line);
        self.open = Some(open);
        Ok(self.current()?.window())
    }

    pub fn goto(&mut self, line: usize) -> Result<Window, AciError> {
        let open = self.current_mut()?;
        open.check_line(line)?;
        open.center_on(line);
        Ok(open.window())
    }

    pub fn scroll_down(&mut self) -> Result<Window, AciError> {
        let open = self.current_mut()?;
        open.first = (open.first + WINDOW).min(open.last_first());
        Ok(open.window())
    }

    pub fn scroll_up(&mut self) -> Result<Window, AciError> {
        let open = self.current_mut()?;
        open.first = open.first.saturating_sub(WINDOW).max(1);
        Ok(open.window())
    }

    /// Re-reads the open file after an edit, keeping the position. A file
    /// that can no longer be read is closed.
    pub fn reload(&mut self, ws: &Workspace, edited: &str) {
        let Some(open) = self.open.as_mut().filter(|o| o.path == edited) else {
            return;
        };
        match ws.resolve(edited).and_then(|p| ws.read_named_text(&p)) {
            Ok(text) => {
                if is_sensitive_path(&open.path) {
                    let total = text.lines().count().max(1);
                    open.lines = vec![format!("{MARKER}sensitive-file]"); total];
                } else {
                    open.lines = text.lines().map(str::to_owned).collect();
                }
                open.first = open.first.min(open.last_first());
            }
            Err(_) => self.open = None,
        }
    }

    fn current(&self) -> Result<&OpenFile, AciError> {
        self.open.as_ref().ok_or(AciError::NoOpenFile)
    }

    fn current_mut(&mut self) -> Result<&mut OpenFile, AciError> {
        self.open.as_mut().ok_or(AciError::NoOpenFile)
    }
}

/// Numbered lines, `n:text`, shared by windows and edit snippets.
pub(crate) fn write_numbered(
    f: &mut fmt::Formatter<'_>,
    first: usize,
    lines: &[String],
) -> fmt::Result {
    for (i, line) in lines.iter().enumerate() {
        write!(f, "\n{}:{line}", first + i)?;
    }
    Ok(())
}

impl fmt::Display for Window {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[File: {} ({} lines total)]", self.path, self.total)?;
        let above = self.first - 1;
        let below = self.total - above - self.lines.len();
        if above > 0 {
            write!(f, "\n({above} more lines above)")?;
        }
        write_numbered(f, self.first, &self.lines)?;
        if below > 0 {
            write!(f, "\n({below} more lines below)")?;
        }
        Ok(())
    }
}
