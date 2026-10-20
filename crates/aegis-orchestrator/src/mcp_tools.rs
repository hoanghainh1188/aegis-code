//! [`ToolExecutor`] over MCP servers (ADR 0001, #8): the ACI server and
//! Codebase-Memory run as child processes speaking MCP on stdio, and the
//! agent sees their tools as one list.
//!
//! The list is fixed when the servers connect and sorted by name, so the
//! cached prompt prefix stays byte-identical for the whole session (design
//! §4.4). A server's tools can be narrowed to an allow-list; a name in the
//! allow-list that the server does not offer, or a name offered by two
//! servers, fails the connection instead of quietly changing the list.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use aegis_provider::ToolDefinition;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;

use crate::{ToolExecutor, ToolOutcome};

/// One MCP server, started as `program args…` in `cwd`, with only
/// [`INHERITED_ENV`] and `env` in its environment. It is killed when
/// [`McpTools`] is dropped; [`McpTools::shutdown`] closes it cleanly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    /// Shown in errors.
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Tools to offer the agent; `None` offers every tool of the server.
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Error)]
pub enum McpToolsError {
    #[error("cannot start MCP server `{server}`: {message}")]
    Start { server: String, message: String },

    #[error("cannot list the tools of MCP server `{server}`: {message}")]
    ListTools { server: String, message: String },

    #[error("MCP server `{server}` has no tool `{tool}`")]
    MissingTool { server: String, tool: String },

    #[error("tool `{tool}` is offered by both `{first}` and `{second}`")]
    DuplicateTool {
        tool: String,
        first: String,
        second: String,
    },
}

/// Text shown for content the agent cannot read (images, resources).
const NON_TEXT: &str = "[non-text content omitted]";

/// Shown when a tool returns no content at all.
const NO_OUTPUT: &str = "[no output]";

/// Variables a server gets from this process; everything else comes from
/// [`McpServerSpec::env`].
pub const INHERITED_ENV: &[&str] = &["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL"];

pub struct McpTools {
    servers: Vec<(String, RunningService<RoleClient, ()>)>,
    /// Tool name to index in `servers`.
    routes: BTreeMap<String, usize>,
    definitions: Vec<ToolDefinition>,
    call_timeout: Duration,
}

impl std::fmt::Debug for McpTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTools")
            .field(
                "servers",
                &self.servers.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("routes", &self.routes)
            .finish_non_exhaustive()
    }
}

async fn start(spec: &McpServerSpec) -> Result<RunningService<RoleClient, ()>, McpToolsError> {
    let failed = |message: String| McpToolsError::Start {
        server: spec.name.clone(),
        message,
    };
    let mut command = Command::new(&spec.program);
    // Only a known set of variables, so provider credentials never reach
    // the server or the commands it runs for the repo.
    command
        .args(&spec.args)
        .env_clear()
        .envs(
            INHERITED_ENV
                .iter()
                .filter_map(|k| Some((*k, std::env::var_os(k)?))),
        )
        .envs(spec.env.iter().cloned())
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    let transport = TokioChildProcess::new(command).map_err(|e| failed(e.to_string()))?;
    ().serve(transport).await.map_err(|e| failed(e.to_string()))
}

/// The server's tools as definitions, narrowed to `spec.tools`.
async fn definitions_of(
    spec: &McpServerSpec,
    service: &RunningService<RoleClient, ()>,
) -> Result<Vec<ToolDefinition>, McpToolsError> {
    let tools = service
        .list_all_tools()
        .await
        .map_err(|e| McpToolsError::ListTools {
            server: spec.name.clone(),
            message: e.to_string(),
        })?;
    let offered: BTreeSet<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    if let Some(tool) = spec
        .tools
        .iter()
        .flatten()
        .find(|w| !offered.contains(w.as_str()))
    {
        return Err(McpToolsError::MissingTool {
            server: spec.name.clone(),
            tool: tool.clone(),
        });
    }
    Ok(tools
        .into_iter()
        .filter(|t| {
            spec.tools
                .as_ref()
                .is_none_or(|w| w.iter().any(|n| *n == t.name))
        })
        .map(|t| ToolDefinition {
            name: t.name.into_owned(),
            description: t.description.map(|d| d.into_owned()).unwrap_or_default(),
            input_schema: Value::Object((*t.input_schema).clone()),
        })
        .collect())
}

fn outcome(result: CallToolResult) -> ToolOutcome {
    let content = result
        .content
        .iter()
        .map(|c| c.as_text().map_or(NON_TEXT, |t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    ToolOutcome {
        content: if content.is_empty() {
            NO_OUTPUT.to_owned()
        } else {
            content
        },
        is_error: result.is_error.unwrap_or(false),
    }
}

fn failure(content: String) -> ToolOutcome {
    ToolOutcome {
        content,
        is_error: true,
    }
}

impl McpTools {
    /// A tool call still running after this long fails.
    pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(300);

    /// Starting a server and listing its tools must finish within this.
    pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

    /// Starts every server and builds the tool list. Servers already
    /// started are shut down when a later one fails.
    pub async fn connect(specs: &[McpServerSpec]) -> Result<Self, McpToolsError> {
        let mut tools = Self {
            servers: Vec::new(),
            routes: BTreeMap::new(),
            definitions: Vec::new(),
            call_timeout: Self::DEFAULT_CALL_TIMEOUT,
        };
        match tools.add_all(specs).await {
            Ok(()) => Ok(tools),
            Err(e) => {
                tools.shutdown().await;
                Err(e)
            }
        }
    }

    async fn add_all(&mut self, specs: &[McpServerSpec]) -> Result<(), McpToolsError> {
        for spec in specs {
            let timed_out = || McpToolsError::Start {
                server: spec.name.clone(),
                message: format!("no answer within {}s", Self::CONNECT_TIMEOUT.as_secs()),
            };
            let service = tokio::time::timeout(Self::CONNECT_TIMEOUT, start(spec))
                .await
                .map_err(|_| timed_out())??;
            let index = self.servers.len();
            self.servers.push((spec.name.clone(), service));
            let definitions = tokio::time::timeout(
                Self::CONNECT_TIMEOUT,
                definitions_of(spec, &self.servers[index].1),
            )
            .await
            .map_err(|_| timed_out())??;
            for definition in definitions {
                if let Some(&other) = self.routes.get(&definition.name) {
                    return Err(McpToolsError::DuplicateTool {
                        tool: definition.name,
                        first: self.servers[other].0.clone(),
                        second: spec.name.clone(),
                    });
                }
                self.routes.insert(definition.name.clone(), index);
                self.definitions.push(definition);
            }
        }
        self.definitions.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = timeout;
        self
    }

    /// Closes every server and waits for it to exit.
    pub async fn shutdown(self) {
        for (_, service) in self.servers {
            let _ = service.cancel().await;
        }
    }
}

impl ToolExecutor for McpTools {
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn call(&self, name: &str, input: &Value) -> ToolOutcome {
        let Some(&index) = self.routes.get(name) else {
            return failure(format!("unknown tool `{name}`"));
        };
        let (server, service) = &self.servers[index];
        let arguments = match input {
            Value::Object(map) => map.clone(),
            Value::Null => serde_json::Map::new(),
            _ => return failure(format!("the input of `{name}` must be a JSON object")),
        };
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        match tokio::time::timeout(self.call_timeout, service.call_tool(params)).await {
            Ok(Ok(result)) => outcome(result),
            Ok(Err(e)) => failure(format!("tool server `{server}` failed on `{name}`: {e}")),
            Err(_) => failure(format!(
                "`{name}` did not finish within {}s",
                self.call_timeout.as_secs()
            )),
        }
    }
}
