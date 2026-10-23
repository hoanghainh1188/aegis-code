use thiserror::Error;

#[derive(Debug, Error)]
pub enum BatchError {
    #[error("git {args}: {message}")]
    Git { args: String, message: String },

    #[error("not a Sonar issue search response: {0}")]
    Search(String),

    #[error("invalid batch id `{0}`")]
    InvalidBatchId(String),
}
