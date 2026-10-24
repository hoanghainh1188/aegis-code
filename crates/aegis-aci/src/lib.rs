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
mod search;
mod secrets;
mod server;
mod summary;
mod viewer;
mod workspace;

use std::path::Path;

use aegis_gate::CancelToken;
use aegis_lang::ResolvedProfile;

pub use edit::{CONTEXT, EditOutcome, EditRequest, Rejection, Snippet};
pub use error::AciError;
pub use navigate::{Lookup, LookupAt, LookupResult};
pub use run_tests::{RUN_TESTS_TIMEOUT, TestRun, TestSettings};
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
    /// The profile's language server, once started.
    lsp: Option<lsp::Lsp>,
    /// Files written since the language server last saw them.
    changed: lsp::Changed,
    tests: run_tests::TestRunner,
}

impl Aci {
    pub fn new(root: &Path, profile: ResolvedProfile) -> Result<Self, AciError> {
        Ok(Self {
            workspace: Workspace::new(root, profile)?,
            viewer: viewer::Viewer::default(),
            lsp: None,
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

    /// Whether the profile can run one test: `run_tests` is offered only
    /// then.
    pub fn can_run_tests(&self) -> bool {
        self.workspace.profile().gate.repro_test.command().is_some()
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

    /// Whether the profile names a language server: `find_definition` and
    /// `find_references` are offered only then.
    pub fn has_language_server(&self) -> bool {
        self.workspace.profile().profile.aci.lsp.is_some()
    }

    /// Starts the profile's language server, in the format container when
    /// there is one, so it indexes while the agent reads the issue.
    pub fn start_language_server(&mut self) -> Result<(), AciError> {
        let Some(command) = self.workspace.profile().profile.aci.lsp.clone() else {
            return Ok(());
        };
        let options = self
            .workspace
            .profile()
            .profile
            .aci
            .lsp_options
            .as_ref()
            .map(|o| serde_json::to_value(&o.0))
            .transpose()
            .map_err(|e| AciError::LanguageServer(format!("aci.lsp_options: {e}")))?;
        self.lsp = Some(lsp::Lsp::start(
            command.argv(),
            self.workspace.root(),
            options,
            self.workspace.format_container(),
        )?);
        Ok(())
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
        let Some(server) = self.lsp.as_mut() else {
            return Err(AciError::LanguageServer(
                "no language server is running for this repo; use search_symbol".into(),
            ));
        };
        server.ready(lsp::CALL_WAIT)?;
        // The server reads files from disk only when it opens them: what the
        // ACI wrote since goes to it first.
        for path in self.changed.take() {
            if let Some(text) = self.workspace.read_text(&path) {
                let relative = self.workspace.display(&path);
                server.sync(&relative, &text, self.workspace.profile().language().id())?;
            }
        }
        navigate::lookup(&self.workspace, server, kind, at)
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
