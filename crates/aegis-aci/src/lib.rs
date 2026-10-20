//! ACI tools for the agent (design §3.1): find, view and edit code in one
//! repo. Everything language-specific (which files are source, grammar,
//! symbol query, formatter) comes from the repo's resolved language profile
//! (ADR 0004); this crate names no tool of any language.
//!
//! Each tool returns a value whose `Display` is the fixed text shown to the
//! agent, with paths relative to the repo root and line numbers.
//! [`AciServer`] serves them over MCP; the `aegis-aci` binary runs it on
//! stdio.

mod edit;
mod error;
mod formatter;
mod search;
mod secrets;
mod server;
mod summary;
mod viewer;
mod workspace;

use std::path::Path;

use aegis_lang::ResolvedProfile;

pub use edit::{CONTEXT, EditOutcome, EditRequest, Rejection, Snippet};
pub use error::AciError;
pub use search::{FileHit, Hits, MAX_RESULTS, SearchResult, SymbolHit, TextHit};
pub use secrets::{MARKER as REDACTED_MARKER, Redacted, redact};
pub use server::AciServer;
pub use summary::{load_or_create_summary, module_summary};
pub use viewer::{WINDOW, Window};
pub use workspace::Workspace;

/// The tools of one agent session on one repo; `open` and the scroll tools
/// share the file being viewed.
#[derive(Debug)]
pub struct Aci {
    workspace: Workspace,
    viewer: viewer::Viewer,
}

impl Aci {
    pub fn new(root: &Path, profile: ResolvedProfile) -> Result<Self, AciError> {
        Ok(Self {
            workspace: Workspace::new(root, profile)?,
            viewer: viewer::Viewer::default(),
        })
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Definitions named `name`, or `Container::name` / `Container.name`.
    pub fn search_symbol(&self, name: &str) -> Result<SearchResult<SymbolHit>, AciError> {
        search::search_symbol(&self.workspace, name)
    }

    /// Lines containing `text` in the files under `dir` (default: root).
    pub fn search_text(
        &self,
        text: &str,
        dir: Option<&str>,
    ) -> Result<SearchResult<TextHit>, AciError> {
        search::search_text(&self.workspace, text, dir)
    }

    /// Files whose name matches `pattern` (`*` wildcard) under `dir`.
    pub fn find_file(
        &self,
        pattern: &str,
        dir: Option<&str>,
    ) -> Result<SearchResult<FileHit>, AciError> {
        search::find_file(&self.workspace, pattern, dir)
    }

    pub fn open(&mut self, file: &str, line: Option<usize>) -> Result<Window, AciError> {
        self.viewer.open(&self.workspace, file, line)
    }

    pub fn goto(&mut self, line: usize) -> Result<Window, AciError> {
        self.viewer.goto(line)
    }

    pub fn scroll_down(&mut self) -> Result<Window, AciError> {
        self.viewer.scroll_down()
    }

    pub fn scroll_up(&mut self) -> Result<Window, AciError> {
        self.viewer.scroll_up()
    }

    /// A new file; refused when it already exists or does not parse.
    pub fn create_file(&mut self, file: &str, content: &str) -> Result<EditOutcome, AciError> {
        edit::create(&self.workspace, file, content)
    }

    pub fn edit(&mut self, request: EditRequest<'_>) -> Result<EditOutcome, AciError> {
        let outcome = edit::edit(&self.workspace, request)?;
        if let EditOutcome::Applied(snippet) = &outcome {
            // The edit is written either way; a file that cannot be
            // re-read is closed, and the next scroll asks for `open`.
            self.viewer.reload(&self.workspace, &snippet.path);
        }
        Ok(outcome)
    }
}
