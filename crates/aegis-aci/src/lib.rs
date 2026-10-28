//! ACI tools for the agent (design §3.1): find, view and edit code in one
//! repo, and run one test on it. Everything language-specific (which files
//! are source, grammar, symbol query, formatter, test command) comes from
//! the repo's resolved language profile (ADR 0004); this crate names no
//! tool of any language.
//!
//! Each tool returns a value whose `Display` is the fixed text shown to the
//! agent, with paths relative to the repo root and line numbers.
//! [`AciServer`] serves them over MCP; the `aegis-aci` binary runs it on
//! stdio.

mod edit;
mod error;
mod formatter;
mod lsp;
mod navigate;
mod run_tests;
pub mod sbfl;
mod search;
mod secrets;
mod server;
mod summary;
mod viewer;
mod workspace;

use std::collections::BTreeMap;
use std::path::Path;

use aegis_gate::CancelToken;
use aegis_lang::{ProfilePart, RepoProfiles};

pub use edit::{CONTEXT, EditOutcome, EditRequest, Rejection, Snippet};
pub use error::AciError;
pub use navigate::{Lookup, LookupAt, LookupResult};
pub use run_tests::{RUN_TESTS_TIMEOUT, TestRun, TestSettings};
pub use sbfl::{SbflFormula, SbflMatrix, SuspiciousElement, TestExecution};
pub use search::{FileHit, Hits, MAX_RESULTS, SearchResult, SymbolHit, TextHit};
pub use secrets::{MARKER as REDACTED_MARKER, Redacted, redact};
pub use server::AciServer;
pub use summary::{load_or_create_summary, module_summary};
pub use viewer::{WINDOW, Window};
pub use workspace::{FormatContainer, Workspace};

/// The tools of one agent session on one repo; `open` and the scroll tools
/// share the file being viewed.
#[derive(Debug)]
pub struct Aci {
    workspace: Workspace,
    viewer: viewer::Viewer,
    /// Each part's language server, by the part's directory, once asked
    /// for (#199, #268); a start that failed keeps its reason, so later
    /// calls do not try again.
    lsp: BTreeMap<String, Result<lsp::Lsp, String>>,
    /// Files written since their part's language server last saw them.
    changed: lsp::Changed,
    tests: run_tests::TestRunner,
}

impl Aci {
    pub fn new(root: &Path, profiles: impl Into<RepoProfiles>) -> Result<Self, AciError> {
        Ok(Self {
            workspace: Workspace::new(root, profiles)?,
            viewer: viewer::Viewer::default(),
            lsp: BTreeMap::new(),
            changed: lsp::Changed::default(),
            tests: run_tests::TestRunner::new(TestSettings::default()),
        })
    }

    /// Where `run_tests` runs: the gate's sandbox, environment, time limit.
    pub fn with_test_settings(self, settings: TestSettings) -> Self {
        Self {
            tests: run_tests::TestRunner::new(settings),
            ..self
        }
    }

    /// Whether a profile of the repo can run one test: `run_tests` is
    /// offered only then.
    pub fn can_run_tests(&self) -> bool {
        self.workspace
            .profiles()
            .parts()
            .iter()
            .any(|p| p.profile.gate.repro_test.command().is_some())
    }

    /// Stops a `run_tests` call in progress, and refuses later ones; for a
    /// server shutting down.
    pub fn tests_cancel_token(&self) -> CancelToken {
        self.tests.cancel_token()
    }

    /// Runs `test` with the profile's `gate.repro_test` command on the tree
    /// as it is now.
    pub fn run_tests(&mut self, test: &str) -> Result<TestRun, AciError> {
        self.tests.run(&self.workspace, test)
    }

    /// Runs the profile's formatter in `container` (see [`FormatContainer`]).
    pub fn with_format_container(self, container: FormatContainer) -> Self {
        Self {
            workspace: self.workspace.with_format_container(container),
            ..self
        }
    }

    /// Mounts `mounts[dir]` in the formatter's and `run_tests`' containers
    /// of the part at `dir` (see [`Workspace::part_mounts`]).
    pub fn with_part_mounts(
        self,
        mounts: std::collections::BTreeMap<String, Vec<(String, String)>>,
    ) -> Self {
        Self {
            workspace: self.workspace.with_part_mounts(mounts),
            ..self
        }
    }

    /// Whether a profile of the repo names a language server:
    /// `find_definition` and `find_references` are offered only then.
    pub fn has_language_server(&self) -> bool {
        self.workspace
            .profiles()
            .parts()
            .iter()
            .any(|p| p.profile.profile.aci.lsp.is_some())
    }

    /// Starts the language server of each part whose profile names one now,
    /// rather than at the first `find_definition` / `find_references` on
    /// the part. One that cannot start leaves the others running.
    pub fn start_language_server(&mut self) -> Result<(), AciError> {
        let mut failed = Vec::new();
        for part in self.workspace.profiles().parts() {
            if part.profile.profile.aci.lsp.is_none() {
                continue;
            }
            if let Err(e) = server_of(&mut self.lsp, &self.workspace, part) {
                failed.push((part.label(), e));
            }
        }
        match (failed.len(), self.workspace.profiles().parts().len()) {
            (0, _) => Ok(()),
            (_, 1) => Err(failed.remove(0).1),
            _ => Err(AciError::LanguageServer(
                failed
                    .iter()
                    .map(|(label, e)| format!("{label}: {e}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )),
        }
    }

    /// The definitions of what is at `at`, by the language server.
    pub fn find_definition(&mut self, at: LookupAt<'_>) -> Result<LookupResult, AciError> {
        self.lookup(Lookup::Definition, at)
    }

    /// Where what is at `at` is used, its definition included.
    pub fn find_references(&mut self, at: LookupAt<'_>) -> Result<LookupResult, AciError> {
        self.lookup(Lookup::References, at)
    }

    fn lookup(&mut self, kind: Lookup, at: LookupAt<'_>) -> Result<LookupResult, AciError> {
        let target = navigate::resolve(&self.workspace, at)?;
        let part = self.workspace.profiles().part_of(&target.relative);
        if part.profile.profile.aci.lsp.is_none() {
            return Err(AciError::LanguageServer(format!(
                "no language server is running for {}; use search_symbol",
                part.label()
            )));
        }
        // Started at the first call on its part (#268): most jobs never ask,
        // and an index of the whole repo is not free.
        let server = server_of(&mut self.lsp, &self.workspace, part)?;
        server.ready(lsp::CALL_WAIT)?;
        // The server reads files from disk only when it opens them: what the
        // ACI wrote in its part since goes to it first.
        let language = part.profile.language().id();
        let workspace = &self.workspace;
        let mine = self
            .changed
            .take_where(|path| workspace.part_of(path).dir == part.dir);
        for path in mine {
            if let Some(text) = workspace.read_text(&path) {
                server.sync(&workspace.display(&path), &text, language)?;
            }
        }
        navigate::lookup(workspace, server, language, kind, target)
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
        let outcome = edit::create(&self.workspace, file, content)?;
        if let EditOutcome::Applied(snippet) = &outcome {
            self.changed
                .mark(&self.workspace.root().join(&snippet.path));
        }
        Ok(outcome)
    }

    pub fn edit(&mut self, request: EditRequest<'_>) -> Result<EditOutcome, AciError> {
        let outcome = edit::edit(&self.workspace, request)?;
        if let EditOutcome::Applied(snippet) = &outcome {
            self.changed
                .mark(&self.workspace.root().join(&snippet.path));
            // The edit is written either way; a file that cannot be
            // re-read is closed, and the next scroll asks for `open`.
            self.viewer.reload(&self.workspace, &snippet.path);
        }
        Ok(outcome)
    }
}

/// `part`'s language server, started now if it was not yet.
fn server_of<'a>(
    servers: &'a mut BTreeMap<String, Result<lsp::Lsp, String>>,
    workspace: &Workspace,
    part: &ProfilePart,
) -> Result<&'a mut lsp::Lsp, AciError> {
    servers
        .entry(part.dir.clone())
        .or_insert_with(|| start_server(workspace, part).map_err(|e| e.to_string()))
        .as_mut()
        .map_err(|e| AciError::LanguageServer(e.clone()))
}

/// `part`'s language server, with the part's options.
fn start_server(workspace: &Workspace, part: &ProfilePart) -> Result<lsp::Lsp, AciError> {
    let aci = &part.profile.profile.aci;
    let command = aci.lsp.as_ref().expect("checked by the caller");
    let options = aci
        .lsp_options
        .as_ref()
        .map(|o| serde_json::to_value(&o.0))
        .transpose()
        .map_err(|e| AciError::LanguageServer(format!("aci.lsp_options: {e}")))?;
    lsp::Lsp::start(
        command.argv(),
        workspace.root(),
        &part.dir,
        options,
        workspace.format_container_of(part).as_ref(),
    )
}
