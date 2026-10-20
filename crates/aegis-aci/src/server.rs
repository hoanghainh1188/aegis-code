//! The ACI tools as an MCP server (ADR 0001, #8). One server serves one
//! agent session on one repo: `open` and the scroll tools share the file
//! being viewed. A failed tool returns its error text with `isError` set,
//! so the agent sees what to change.

use std::fmt::Display;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::{Aci, AciError, EditOutcome, EditRequest};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchSymbolParams {
    /// Name of a function, type, trait, class…; `Type::method` or
    /// `Class.method` for a method.
    pub name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchTextParams {
    /// Literal, case-sensitive text to find.
    pub text: String,
    /// Directory to search, relative to the repo root (default: root).
    pub dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindFileParams {
    /// File name, `*` matches any run of characters (e.g. `test_*.py`).
    pub pattern: String,
    /// Directory to search, relative to the repo root (default: root).
    pub dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpenParams {
    /// File path relative to the repo root.
    pub file: String,
    /// 1-based line to show near the middle of the window (default: 1).
    pub line: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GotoParams {
    /// 1-based line of the open file.
    pub line: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditParams {
    /// File path relative to the repo root.
    pub file: String,
    /// First line to replace, 1-based.
    pub from: usize,
    /// Last line to replace, inclusive; `from - 1` inserts before `from`.
    pub to: usize,
    /// New lines, without line numbers; empty deletes the range.
    pub content: String,
}

/// A call that waited this long for the previous one is dropped. Kept below
/// the orchestrator's call timeout, so a call it reported as unfinished is
/// never run afterwards.
pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct AciServer {
    aci: Arc<Mutex<Aci>>,
    tool_router: ToolRouter<Self>,
}

impl AciServer {
    pub fn new(aci: Aci) -> Self {
        Self {
            aci: Arc::new(Mutex::new(aci)),
            tool_router: Self::tool_router(),
        }
    }

    /// Runs a tool off the async runtime: edits may wait on the formatter.
    async fn run<T: Send + 'static>(
        &self,
        run: impl FnOnce(&mut Aci) -> Result<T, AciError> + Send + 'static,
    ) -> Result<T, String> {
        let aci = Arc::clone(&self.aci);
        let queued = Instant::now();
        tokio::task::spawn_blocking(move || {
            // A tool that panicked leaves nothing half-written: edits write
            // whole files, the viewer only holds text.
            let mut aci = aci.lock().unwrap_or_else(PoisonError::into_inner);
            // The client has given up by now; do not apply a late edit the
            // agent was told did not finish.
            if queued.elapsed() > MAX_QUEUE_WAIT {
                return Err(format!(
                    "the server was busy for over {}s; the call was not run",
                    MAX_QUEUE_WAIT.as_secs()
                ));
            }
            run(&mut aci).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("the tool did not finish: {e}"))?
    }

    async fn with_aci<T: Display + Send + 'static>(
        &self,
        run: impl FnOnce(&mut Aci) -> Result<T, AciError> + Send + 'static,
    ) -> Result<String, String> {
        self.run(run).await.map(|out| out.to_string())
    }
}

#[tool_router]
impl AciServer {
    #[tool(
        description = "Find definitions by name. Returns kind, signature, file and line range; \
                          at most 50 results, otherwise asks to narrow."
    )]
    async fn search_symbol(
        &self,
        Parameters(p): Parameters<SearchSymbolParams>,
    ) -> Result<String, String> {
        self.with_aci(move |aci| aci.search_symbol(&p.name)).await
    }

    #[tool(
        description = "Find lines containing a literal text. Returns file and line; \
                          at most 50 results, otherwise asks to narrow."
    )]
    async fn search_text(
        &self,
        Parameters(p): Parameters<SearchTextParams>,
    ) -> Result<String, String> {
        self.with_aci(move |aci| aci.search_text(&p.text, p.dir.as_deref()))
            .await
    }

    #[tool(
        description = "Find files by name pattern. At most 50 results, otherwise asks \
                          to narrow."
    )]
    async fn find_file(&self, Parameters(p): Parameters<FindFileParams>) -> Result<String, String> {
        self.with_aci(move |aci| aci.find_file(&p.pattern, p.dir.as_deref()))
            .await
    }

    #[tool(
        description = "Open a file and show 100 numbered lines around `line`, with the \
                          number of lines hidden above and below."
    )]
    async fn open(&self, Parameters(p): Parameters<OpenParams>) -> Result<String, String> {
        self.with_aci(move |aci| aci.open(&p.file, p.line)).await
    }

    #[tool(description = "Show the 100 lines around a line of the open file.")]
    async fn goto(&self, Parameters(p): Parameters<GotoParams>) -> Result<String, String> {
        self.with_aci(move |aci| aci.goto(p.line)).await
    }

    #[tool(description = "Show the next 100 lines of the open file.")]
    async fn scroll_down(&self) -> Result<String, String> {
        self.with_aci(Aci::scroll_down).await
    }

    #[tool(description = "Show the previous 100 lines of the open file.")]
    async fn scroll_up(&self) -> Result<String, String> {
        self.with_aci(Aci::scroll_up).await
    }

    #[tool(
        description = "Replace lines `from` to `to` of a file with `content`. The result \
                          must parse and pass the repo's formatter, or the file is left \
                          unchanged and the proposed lines are shown. Returns the edited \
                          lines with 5 lines of context."
    )]
    async fn edit(&self, Parameters(p): Parameters<EditParams>) -> Result<String, String> {
        let outcome = self
            .run(move |aci| {
                aci.edit(EditRequest {
                    file: &p.file,
                    from: p.from,
                    to: p.to,
                    content: &p.content,
                })
            })
            .await?;
        // A refused edit is a failed call, so the agent does not take it
        // for written.
        match outcome {
            EditOutcome::Applied(_) => Ok(outcome.to_string()),
            EditOutcome::Rejected { .. } => Err(outcome.to_string()),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AciServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Find, view and edit code in one repo. Paths are relative to the repo root.",
        )
    }
}
