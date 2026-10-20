use std::path::Path;

use aegis_lang::LangError;
use thiserror::Error;

/// The gate could not run at all. A step that runs and fails is a
/// [`crate::StepStatus::Failed`] in the report, not an error.
#[derive(Debug, Error)]
pub enum GateError {
    #[error("git {args}: {message}")]
    Git { args: String, message: String },

    #[error("cannot start `{program}`: {message}")]
    Spawn { program: String, message: String },

    #[error("I/O error on `{path}`: {message}")]
    Io { path: String, message: String },

    #[error("invalid gate request: {0}")]
    InvalidRequest(String),

    #[error(transparent)]
    Lang(#[from] LangError),
}

impl GateError {
    pub(crate) fn io(path: &Path, e: &std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        }
    }
}
