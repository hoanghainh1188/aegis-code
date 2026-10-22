use std::path::Path;

use aegis_lang::LangError;
use thiserror::Error;

/// Errors are shown to the agent as they are, so each says what to change.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AciError {
    #[error("`{0}` is outside the workspace; use a path relative to the repo root")]
    OutsideWorkspace(String),

    #[error("`{0}` does not exist")]
    NotFound(String),

    #[error("`{0}` is not a file")]
    NotAFile(String),

    #[error("`{0}` is not a directory")]
    NotADirectory(String),

    #[error("`{path}` is not UTF-8 text or is over {limit} bytes")]
    NotText { path: String, limit: usize },

    #[error("{0}")]
    InvalidInput(String),

    #[error("no file is open; call `open` first")]
    NoOpenFile,

    #[error("cannot run the formatter `{program}`: {message}")]
    Formatter { program: String, message: String },

    /// The language server of `find_definition` / `find_references`.
    #[error("{0}")]
    LanguageServer(String),

    #[error("I/O error on `{path}`: {message}")]
    Io { path: String, message: String },

    #[error(transparent)]
    Lang(#[from] LangError),
}

impl AciError {
    pub(crate) fn io(path: &Path, e: &std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        }
    }
}
