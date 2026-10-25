use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrError {
    #[error("not a remote the agent may push to: {0}")]
    InvalidRemote(String),

    #[error("the gate did not pass; nothing is pushed")]
    GateNotPassed,

    #[error("`{0}` is not a valid issue id for a branch name")]
    InvalidIssueId(String),

    #[error("git {args}: {message}")]
    Git { args: String, message: String },

    #[error("GitHub API {status}: {message}")]
    GitHub { status: u16, message: String },

    #[error("cannot reach GitHub: {0}")]
    Http(String),

    /// `status` 0: the request did not get an answer.
    #[error("GitLab API {status}: {message}")]
    GitLab { status: u16, message: String },
}
