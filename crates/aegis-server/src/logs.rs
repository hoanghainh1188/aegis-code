//! Logs (ADR 0003 §6, #32): one JSON object per line, with the fields of
//! the spans it happened in. Each attempt runs in a `job` span carrying
//! `job_id`, `repo_id`, `attempt` and, once its repo is known, `repo`
//! (slug) and `language`, so every line of a job can be found by its id.
//!
//! Never logged: a secret's value (only its name), the content of a file
//! of the repo, a prompt or a model's answer. Errors are logged by their
//! message, which says what failed and where, not what a file held.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::{Format, Json, JsonFields};
use tracing_subscriber::fmt::{MakeWriter, Subscriber};

/// What `RUST_LOG` gives, or `info`.
pub fn filter_from_env() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// The server's subscriber, writing to `writer`.
pub fn subscriber<W>(
    writer: W,
    filter: EnvFilter,
) -> Subscriber<JsonFields, Format<Json>, EnvFilter, W>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(writer)
        .with_env_filter(filter)
        .finish()
}
