use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("model `{model}` refused by policy: {reason}")]
    PolicyViolation { model: String, reason: &'static str },

    #[error("rate limited (retry after {retry_after_s:?}s)")]
    RateLimited { retry_after_s: Option<u64> },

    #[error("model overloaded")]
    Overloaded,

    #[error("API error {status} ({kind}): {message}")]
    Api {
        status: u16,
        kind: String,
        message: String,
    },

    #[error("transport error: {0}")]
    Transport(String),

    #[error("could not decode response: {0}")]
    Decode(String),

    #[error("configuration error: {0}")]
    Config(String),
}

impl ProviderError {
    /// Whether the same request may succeed if sent again later.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Overloaded | Self::Transport(_) => true,
            Self::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(ProviderError::Overloaded.is_retryable());
        assert!(
            ProviderError::RateLimited {
                retry_after_s: None
            }
            .is_retryable()
        );
        assert!(
            ProviderError::Api {
                status: 500,
                kind: "api_error".into(),
                message: String::new()
            }
            .is_retryable()
        );
        assert!(
            !ProviderError::Api {
                status: 400,
                kind: "invalid_request_error".into(),
                message: String::new()
            }
            .is_retryable()
        );
        assert!(
            !ProviderError::PolicyViolation {
                model: "x".into(),
                reason: "r"
            }
            .is_retryable()
        );
    }
}
