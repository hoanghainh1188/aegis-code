//! Provider section of the run configuration (design §1: the two run
//! configurations differ only in config files, never in code).

use serde::Deserialize;

use crate::claude_api::{ClaudeApi, DEFAULT_API_KEY_ENV};
use crate::{ModelPolicy, ModelProvider, ProviderError, Request, Response};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderConfig {
    ClaudeApi {
        #[serde(default = "default_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    Bedrock {
        region: String,
        /// Refuse any model ID that is not a `jp.` profile (design §7).
        /// Required, so a missing line cannot silently turn the guard off.
        require_jp_profile: bool,
    },
}

fn default_api_key_env() -> String {
    DEFAULT_API_KEY_ENV.to_owned()
}

/// A provider chosen at runtime from [`ProviderConfig`].
#[derive(Debug, Clone)]
pub enum Provider {
    ClaudeApi(ClaudeApi),
    #[cfg(feature = "bedrock")]
    Bedrock(crate::Bedrock),
}

impl Provider {
    pub async fn from_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        match config {
            ProviderConfig::ClaudeApi {
                api_key_env,
                base_url,
            } => {
                let api = ClaudeApi::from_env(api_key_env)?;
                Ok(Self::ClaudeApi(match base_url {
                    Some(url) => api.with_base_url(url.as_str())?,
                    None => api,
                }))
            }
            ProviderConfig::Bedrock {
                region,
                require_jp_profile,
            } => {
                let policy = if *require_jp_profile {
                    ModelPolicy::JapanOnly
                } else {
                    ModelPolicy::Any
                };
                bedrock(region, policy).await
            }
        }
    }
}

#[cfg(feature = "bedrock")]
async fn bedrock(region: &str, policy: ModelPolicy) -> Result<Provider, ProviderError> {
    Ok(Provider::Bedrock(
        crate::Bedrock::from_region(region, policy).await,
    ))
}

#[cfg(not(feature = "bedrock"))]
async fn bedrock(_region: &str, _policy: ModelPolicy) -> Result<Provider, ProviderError> {
    Err(ProviderError::Config(
        "Bedrock provider requested but aegis-provider was built without the `bedrock` feature"
            .into(),
    ))
}

impl ModelProvider for Provider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        match self {
            Self::ClaudeApi(api) => api.send(request).await,
            #[cfg(feature = "bedrock")]
            Self::Bedrock(bedrock) => bedrock.send(request).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct RunConfig {
        provider: ProviderConfig,
    }

    fn load(name: &str) -> ProviderConfig {
        let path = format!("{}/../../config/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        toml::from_str::<RunConfig>(&text).unwrap().provider
    }

    #[test]
    fn poc_config_uses_claude_api() {
        assert_eq!(
            load("poc.toml"),
            ProviderConfig::ClaudeApi {
                api_key_env: "ANTHROPIC_API_KEY".into(),
                base_url: None,
            }
        );
    }

    #[test]
    fn customer_config_uses_bedrock_tokyo_with_jp_guard() {
        assert_eq!(
            load("customer-jp.toml"),
            ProviderConfig::Bedrock {
                region: "ap-northeast-1".into(),
                require_jp_profile: true,
            }
        );
    }

    #[test]
    fn bedrock_requires_explicit_jp_setting() {
        let result = toml::from_str::<RunConfig>(
            "[provider]\nkind = \"bedrock\"\nregion = \"ap-northeast-1\"\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let result = toml::from_str::<RunConfig>(
            "[provider]\nkind = \"bedrock\"\nregion = \"ap-northeast-1\"\nrequire_jp = true\n",
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missing_api_key_fails_fast() {
        let config = ProviderConfig::ClaudeApi {
            api_key_env: "AEGIS_TEST_KEY_THAT_IS_NOT_SET".into(),
            base_url: None,
        };
        let err = Provider::from_config(&config).await.unwrap_err();
        assert!(matches!(err, ProviderError::Config(_)));
    }

    #[cfg(not(feature = "bedrock"))]
    #[tokio::test]
    async fn bedrock_without_feature_is_a_config_error() {
        let err = Provider::from_config(&load("customer-jp.toml"))
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Config(_)));
    }
}
