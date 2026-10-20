use thiserror::Error;

use crate::Language;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LangError {
    #[error("invalid profile `{source_name}`: {message}")]
    InvalidProfile {
        source_name: String,
        message: String,
    },

    #[error("invalid overrides for {language}: {message}")]
    InvalidOverrides { language: Language, message: String },

    #[error("no profile for {0}")]
    MissingProfile(Language),

    #[error("no tree-sitter grammar for {0} yet")]
    UnsupportedGrammar(Language),

    #[error("{language} source is {size} bytes, over the {limit}-byte limit")]
    SourceTooLarge {
        language: Language,
        size: usize,
        limit: usize,
    },

    #[error("tree-sitter could not parse the {0} source")]
    ParseFailed(Language),

    #[error("invalid lockfile {file}: {message}")]
    InvalidLockfile { file: String, message: String },

    #[error("no {language} reader for lockfile `{file}` yet")]
    UnsupportedLockfile { language: Language, file: String },

    #[error("invalid allow-list: {0}")]
    InvalidAllowList(String),

    #[error("symbol query for {language} does not compile: {message}")]
    InvalidQuery { language: Language, message: String },

    #[error("invalid command: {0}")]
    InvalidCommand(String),

    #[error("value for `{{{placeholder}}}` is not allowed: {reason}")]
    InvalidValue {
        placeholder: &'static str,
        reason: &'static str,
    },
}
