use thiserror::Error;

#[derive(Debug, Error)]
pub enum BatchError {
    #[error("git {args}: {message}")]
    Git { args: String, message: String },

    #[error("not a Sonar issue search response: {0}")]
    Search(String),

    #[error("invalid batch id `{0}`")]
    InvalidBatchId(String),

    /// The poll's settings are wrong: retrying will not help.
    #[error("{0}")]
    Config(String),

    /// `status` 0: the request did not get an answer.
    #[error("Sonar API {status}: {message}")]
    Sonar { status: u16, message: String },
}
