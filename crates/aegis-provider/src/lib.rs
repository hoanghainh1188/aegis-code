//! ModelProvider trait with Claude API and Bedrock backends, cache_control
//! placement, and the `jp.` guard (design §4, §5.1, §7; ADR 0001).

use std::future::Future;

#[cfg(feature = "bedrock")]
mod bedrock;
mod body;
mod claude_api;
mod config;
mod error;
mod guard;
mod local;
mod observe;
mod types;

#[cfg(feature = "bedrock")]
pub use bedrock::Bedrock;
pub use claude_api::{API_VERSION, ClaudeApi, DEFAULT_API_KEY_ENV, DEFAULT_BASE_URL};
pub use config::{Provider, ProviderConfig};
pub use error::ProviderError;
pub use guard::ModelPolicy;
pub use local::{LocalModelProvider, LocalProtocol};
pub use observe::{CallObserver, CallReport, Observed};
pub use types::{
    CacheCreation, CachePlan, CacheTtl, ContentBlock, Effort, Message, Request, Response, Role,
    StopReason, ToolDefinition, Usage,
};

/// One backend that can answer a Messages request.
///
/// Implementations place the cache breakpoints described by
/// [`Request::cache`] and return the raw [`Usage`] so the ledger can price it.
/// Retries are left to the caller; see [`ProviderError::is_retryable`].
pub trait ModelProvider {
    fn send(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, ProviderError>> + Send;
}
