use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("invalid price table: {0}")]
    InvalidPrices(String),

    #[error("no price for model `{0}` in the price table")]
    UnknownModel(String),

    #[error("invalid issue id `{id}`: {reason}")]
    InvalidIssueId { id: String, reason: &'static str },

    #[error("`{field}` must be a finite number, got {value}")]
    NonFinite { field: &'static str, value: f64 },

    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("corrupt ledger {path} line {line}: {message}")]
    Corrupt {
        path: PathBuf,
        line: usize,
        message: String,
    },

    #[error("could not encode ledger entry: {0}")]
    Encode(String),
}

impl LedgerError {
    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }
}
