//! Data-residency guard (design §7): with customer code, every request must
//! use a Japan-only Bedrock inference profile.

use crate::ProviderError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelPolicy {
    #[default]
    Any,
    /// Only model IDs starting with `jp.` are sent.
    JapanOnly,
}

const JAPAN_PROFILE_PREFIX: &str = "jp.";

impl ModelPolicy {
    pub fn check(self, model: &str) -> Result<(), ProviderError> {
        match self {
            Self::Any => Ok(()),
            Self::JapanOnly if model.starts_with(JAPAN_PROFILE_PREFIX) => Ok(()),
            Self::JapanOnly => Err(ProviderError::PolicyViolation {
                model: model.to_owned(),
                reason: "customer-code configuration requires a `jp.` inference profile",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_allows_every_model() {
        assert!(ModelPolicy::Any.check("claude-sonnet-5-5").is_ok());
    }

    #[test]
    fn japan_only_allows_jp_profiles() {
        assert!(
            ModelPolicy::JapanOnly
                .check("jp.anthropic.claude-opus-5-5")
                .is_ok()
        );
    }

    #[test]
    fn japan_only_refuses_global_and_bare_ids() {
        for model in [
            "claude-sonnet-5-5",
            "global.anthropic.claude-sonnet-5-5",
            "anthropic.claude-haiku-5-5",
            "JP.anthropic.claude-opus-5-5",
            "",
        ] {
            let err = ModelPolicy::JapanOnly.check(model).unwrap_err();
            assert!(
                matches!(err, ProviderError::PolicyViolation { .. }),
                "{model}"
            );
        }
    }
}
