//! Agent loop for one issue: state, steps, history collapsing (design §2, §3.3; ADR 0001).

mod agent_loop;
mod mcp_tools;
mod prompt;
mod session;

pub use agent_loop::{
    LoopConfig, LoopEnd, LoopOutcome, MaxModelCalls, NoObserver, RetryPolicy, StopCheck,
    ToolCallRecord, ToolExecutor, ToolOutcome, TurnObserver, TurnRecord, Warning, run_loop,
    run_loop_observed,
};
pub use mcp_tools::{INHERITED_ENV, McpServerSpec, McpTools, McpToolsError};
pub use prompt::{RepoPrompt, SYSTEM_PROMPT, system_blocks};
pub use session::{COLLAPSED, ModelSpec, Session};
