//! Provider section of the run configuration (design §1: the two run
//! configurations differ only in config files, never in code).

use aegis_secrets::Secrets;
use serde::Deserialize;

use crate::claude_api::{ClaudeApi, DEFAULT_API_KEY_ENV};
use crate::failover::FailoverProvider;
use crate::local::{LocalModelProvider, LocalProtocol};
use crate::openai_compat::OpenAiCompatProvider;
use crate::vertex_ai::VertexAiProvider;
use crate::{ModelPolicy, ModelProvider, ProviderError, Request, Response};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderConfig {
    ClaudeApi {
        /// The key's name: the variable for the CLI, the secret's name for
        /// the server (#30).
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
    Local {
        base_url: String,
        #[serde(default)]
        api_key_env: Option<String>,
        #[serde(default)]
        protocol: LocalProtocol,
    },
    /// OpenAI-compatible enterprise gateway or Azure OpenAI deployment (ADR 0018, #595).
    OpenAiCompat {
        base_url: String,
        api_key_env: String,
        /// When set, sends Azure OpenAI `api-key` header and deployment URL.
        #[serde(default)]
        azure_api_version: Option<String>,
        /// Optional region label validated against `[residency] allowed_regions`.
        #[serde(default)]
        region: Option<String>,
    },
    /// Google Cloud Vertex AI (Gemini or Claude-on-Vertex, ADR 0018, #595).
    VertexAi {
        project: String,
        region: String,
        /// Secret or environment variable holding an OAuth2 Bearer access token.
        access_token_env: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    /// Ordered residency-safe failover chain (ADR 0018, #595).
    Failover {
        #[serde(default = "default_true")]
        retryable_only: bool,
        chain: Vec<ProviderConfig>,
    },
}

fn default_api_key_env() -> String {
    DEFAULT_API_KEY_ENV.to_owned()
}

fn default_true() -> bool {
    true
}

/// A provider chosen at runtime from [`ProviderConfig`], with the
/// residency policy it checks before every request it sends (#592).
#[derive(Debug, Clone)]
pub struct Provider {
    backend: Backend,
    policy: ModelPolicy,
}

#[derive(Debug, Clone)]
pub(crate) enum LeafBackend {
    ClaudeApi(ClaudeApi),
    #[cfg(feature = "bedrock")]
    Bedrock(crate::Bedrock),
    Local(LocalModelProvider),
    OpenAiCompat(OpenAiCompatProvider),
    VertexAi(VertexAiProvider),
}

impl LeafBackend {
    pub(crate) fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }

    pub(crate) async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        match self {
            Self::ClaudeApi(api) => api.send(request).await,
            #[cfg(feature = "bedrock")]
            Self::Bedrock(bedrock) => bedrock.send(request).await,
            Self::Local(local) => local.send(request).await,
            Self::OpenAiCompat(compat) => compat.send(request).await,
            Self::VertexAi(vertex) => vertex.send(request).await,
        }
    }
}

#[derive(Debug, Clone)]
enum Backend {
    Leaf(LeafBackend),
    Failover(FailoverProvider),
}

impl Provider {
    /// The API key from the environment (the CLI).
    pub async fn from_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        Self::build(config, None).await
    }

    /// The API key from `secrets`, read now and again when rotated (the
    /// server, #30). Bedrock needs none: its credentials are the role's.
    pub async fn from_config_with_secrets(
        config: &ProviderConfig,
        secrets: &Secrets,
    ) -> Result<Self, ProviderError> {
        Self::build(config, Some(secrets)).await
    }

    /// The same provider, refusing what `policy` does not allow. A
    /// Bedrock provider keeps its own `jp.` check as well.
    pub fn with_policy(self, policy: ModelPolicy) -> Self {
        Self { policy, ..self }
    }

    pub fn policy(&self) -> &ModelPolicy {
        &self.policy
    }

    /// Whether requests go exclusively to `local` endpoints.
    pub fn is_local(&self) -> bool {
        match &self.backend {
            Backend::Leaf(leaf) => leaf.is_local(),
            Backend::Failover(f) => f.all_local(),
        }
    }

    async fn build_leaf(
        config: &ProviderConfig,
        secrets: Option<&Secrets>,
    ) -> Result<LeafBackend, ProviderError> {
        match config {
            ProviderConfig::ClaudeApi {
                api_key_env,
                base_url,
            } => {
                let api = match secrets {
                    Some(secrets) => {
                        secrets
                            .require(&[api_key_env])
                            .await
                            .map_err(|e| ProviderError::Config(e.to_string()))?;
                        ClaudeApi::from_secret(secrets, api_key_env)?
                    }
                    None => ClaudeApi::from_env(api_key_env)?,
                };
                Ok(LeafBackend::ClaudeApi(match base_url {
                    Some(url) => api.with_base_url(url.as_str())?,
                    None => api,
                }))
            }
            ProviderConfig::Bedrock {
                region,
                require_jp_profile,
            } => {
                let policy = if *require_jp_profile {
                    ModelPolicy::japan_only()
                } else {
                    ModelPolicy::Any
                };
                bedrock(region, policy).await
            }
            ProviderConfig::Local {
                base_url,
                api_key_env,
                protocol,
            } => {
                let api_key = match (api_key_env, secrets) {
                    (Some(env), Some(secrets)) => secrets.get(env).map(|s| s.expose().to_owned()),
                    (Some(env), None) => std::env::var(env).ok(),
                    (None, _) => None,
                };
                Ok(LeafBackend::Local(LocalModelProvider::new(
                    base_url, api_key, *protocol,
                )?))
            }
            ProviderConfig::OpenAiCompat {
                base_url,
                api_key_env,
                azure_api_version,
                ..
            } => {
                let provider = match secrets {
                    Some(secrets) => {
                        secrets
                            .require(&[api_key_env])
                            .await
                            .map_err(|e| ProviderError::Config(e.to_string()))?;
                        OpenAiCompatProvider::from_secret(
                            base_url,
                            secrets,
                            api_key_env,
                            azure_api_version.clone(),
                        )?
                    }
                    None => OpenAiCompatProvider::from_env(
                        base_url,
                        api_key_env,
                        azure_api_version.clone(),
                    )?,
                };
                Ok(LeafBackend::OpenAiCompat(provider))
            }
            ProviderConfig::VertexAi {
                project,
                region,
                access_token_env,
                base_url,
            } => {
                let provider = match secrets {
                    Some(secrets) => {
                        secrets
                            .require(&[access_token_env])
                            .await
                            .map_err(|e| ProviderError::Config(e.to_string()))?;
                        VertexAiProvider::from_secret(
                            project,
                            region,
                            secrets,
                            access_token_env,
                            base_url.clone(),
                        )?
                    }
                    None => VertexAiProvider::from_env(
                        project,
                        region,
                        access_token_env,
                        base_url.clone(),
                    )?,
                };
                Ok(LeafBackend::VertexAi(provider))
            }
            ProviderConfig::Failover { .. } => Err(ProviderError::Config(
                "nested `failover` inside `chain` is not supported".into(),
            )),
        }
    }

    async fn build(
        config: &ProviderConfig,
        secrets: Option<&Secrets>,
    ) -> Result<Self, ProviderError> {
        let backend = match config {
            ProviderConfig::Failover {
                retryable_only,
                chain,
            } => {
                if chain.is_empty() {
                    return Err(ProviderError::Config(
                        "failover provider requires at least one backend in `chain`".into(),
                    ));
                }
                let mut built = Vec::with_capacity(chain.len());
                for item in chain {
                    built.push(Self::build_leaf(item, secrets).await?);
                }
                Backend::Failover(FailoverProvider::new(built, *retryable_only)?)
            }
            other => Backend::Leaf(Self::build_leaf(other, secrets).await?),
        };
        Ok(Self {
            backend,
            policy: ModelPolicy::Any,
        })
    }
}

#[cfg(feature = "bedrock")]
async fn bedrock(region: &str, policy: ModelPolicy) -> Result<LeafBackend, ProviderError> {
    Ok(LeafBackend::Bedrock(
        crate::Bedrock::from_region(region, policy).await,
    ))
}

#[cfg(not(feature = "bedrock"))]
async fn bedrock(_region: &str, _policy: ModelPolicy) -> Result<LeafBackend, ProviderError> {
    Err(ProviderError::Config(
        "Bedrock provider requested but aegis-provider was built without the `bedrock` feature"
            .into(),
    ))
}

impl ModelProvider for Provider {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.policy
            .check_endpoint(&request.model, self.is_local())?;
        self.policy.check(&request.model)?;
        match &self.backend {
            Backend::Leaf(leaf) => leaf.send(request).await,
            Backend::Failover(failover) => failover.send(request).await,
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
    fn local_config_uses_local_provider() {
        assert_eq!(
            load("local.toml"),
            ProviderConfig::Local {
                base_url: "http://localhost:11434/v1".into(),
                api_key_env: None,
                protocol: LocalProtocol::OpenAi,
            }
        );
    }

    #[tokio::test]
    async fn local_provider_builds_without_error() {
        let config = load("local.toml");
        let provider = Provider::from_config(&config).await.unwrap();
        assert!(provider.is_local());
        assert_eq!(provider.policy(), &ModelPolicy::Any);
    }

    fn request(model: &str) -> Request {
        Request {
            model: model.into(),
            max_tokens: 16,
            effort: None,
            system: Vec::new(),
            tools: Vec::new(),
            messages: vec![crate::Message::user_text("hi")],
            cache: crate::CachePlan::default(),
        }
    }

    #[tokio::test]
    async fn the_policy_refuses_before_any_call() {
        // Nothing listens there: a request that got through would fail
        // with a transport error, not a policy one.
        let config = ProviderConfig::Local {
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key_env: None,
            protocol: LocalProtocol::OpenAi,
        };
        let eu = Provider::from_config(&config)
            .await
            .unwrap()
            .with_policy(ModelPolicy::AllowedPrefixes(vec!["eu.".into()]));
        let err = eu.send(&request("qwen2.5-coder:32b")).await.unwrap_err();
        assert!(
            matches!(err, ProviderError::PolicyViolation { .. }),
            "{err}"
        );

        let cloud = ProviderConfig::ClaudeApi {
            api_key_env: "AEGIS_TEST_KEY_FOR_POLICY".into(),
            base_url: Some("http://127.0.0.1:9".into()),
        };
        let secrets = Secrets::fixed([("AEGIS_TEST_KEY_FOR_POLICY", "k")]);
        let airgap = Provider::from_config_with_secrets(&cloud, &secrets)
            .await
            .unwrap()
            .with_policy(ModelPolicy::LocalOnly);
        let err = airgap.send(&request("claude-haiku-5-5")).await.unwrap_err();
        assert!(
            matches!(err, ProviderError::PolicyViolation { .. }),
            "{err}"
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

    #[tokio::test]
    async fn a_missing_secret_key_fails_by_name() {
        let config = ProviderConfig::ClaudeApi {
            api_key_env: "ANTHROPIC_API_KEY".into(),
            base_url: None,
        };
        let err = Provider::from_config_with_secrets(&config, &Secrets::none())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ANTHROPIC_API_KEY"), "{err}");
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
