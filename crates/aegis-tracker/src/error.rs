use thiserror::Error;

#[derive(Debug, Error)]
pub enum TrackerError {
    /// The configuration or an argument is wrong: retrying will not help.
    #[error("{0}")]
    Config(String),

    /// `status` 0: the request did not get an answer.
    #[error("{tracker} API {status}: {message}")]
    Api {
        tracker: &'static str,
        status: u16,
        message: String,
    },
}
