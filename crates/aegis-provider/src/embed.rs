//! Text embeddings for the semantic channel of memory recall (#655).
//!
//! An embedding endpoint is a [`ProviderConfig`] like the mode's chat
//! provider, so the mode's `[residency]` checks it the same way (region,
//! `local_only`). Three APIs:
//! - `local` and `open_ai_compat`: OpenAI's `POST …/embeddings` (Ollama,
//!   vLLM, LocalAI, an enterprise gateway, or an Azure OpenAI deployment);
//! - `vertex_ai`: `…/publishers/google/models/<model>:predict`
//!   (`text-multilingual-embedding-002`);
//! - `bedrock`: `InvokeModel` of Titan Text Embeddings v2
//!   (`amazon.titan-embed-text-v2:0`), in the configured region.
//!
//! `claude_api` has no embedding API, and a `failover` chain is not taken:
//! two models give vectors that cannot be compared.

use std::fmt;
use std::time::Duration;

use aegis_secrets::Secrets;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use reqwest::{StatusCode, redirect};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::local::{error_from, send_error};
use crate::{ModelPolicy, ProviderConfig, ProviderError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const AZURE_API_KEY_HEADER: HeaderName = HeaderName::from_static("api-key");
/// Texts in one request.
const MAX_BATCH: usize = 16;
/// Characters of one text sent: a memory is at most about 6,500, and the
/// models read about 8,000 tokens.
const MAX_TEXT_CHARS: usize = 8_000;
/// HNSW indexes of `pgvector` take at most 2,000 dimensions.
pub const MAX_DIMENSIONS: u32 = 2_000;
/// Cosine similarity a memory needs to count as near, when the mode gives
/// none.
pub const DEFAULT_MIN_SIMILARITY: f32 = 0.5;

/// `[memory.embedding]` of a mode (#655). Without it, recall is lexical
/// only.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// The endpoint: `local`, `open_ai_compat`, `vertex_ai` or `bedrock`.
    pub provider: ProviderConfig,
    /// The model (or Azure deployment), e.g. `nomic-embed-text`,
    /// `text-multilingual-embedding-002`, `amazon.titan-embed-text-v2:0`.
    pub model: String,
    /// Length of its vectors: 1 to [`MAX_DIMENSIONS`].
    pub dimensions: u32,
    /// Cosine similarity, from 0 to 1, below which a memory is not near.
    #[serde(default = "default_min_similarity")]
    pub min_similarity: f32,
}

fn default_min_similarity() -> f32 {
    DEFAULT_MIN_SIMILARITY
}

impl EmbeddingConfig {
    /// What deserializing alone does not check.
    pub fn check(&self) -> Result<(), String> {
        if self.model.trim().is_empty() {
            return Err("[memory.embedding]: `model` is empty".into());
        }
        if !(1..=MAX_DIMENSIONS).contains(&self.dimensions) {
            return Err(format!(
                "[memory.embedding]: `dimensions` must be 1 to {MAX_DIMENSIONS}"
            ));
        }
        if !(0.0..=1.0).contains(&self.min_similarity) {
            return Err("[memory.embedding]: `min_similarity` must be 0 to 1".into());
        }
        match &self.provider {
            ProviderConfig::Local { .. }
            | ProviderConfig::OpenAiCompat { .. }
            | ProviderConfig::VertexAi { .. }
            | ProviderConfig::Bedrock { .. } => Ok(()),
            ProviderConfig::ClaudeApi { .. } => Err(
                "[memory.embedding]: the Claude API has no embedding model; use `local`, \
                 `open_ai_compat`, `vertex_ai` or `bedrock`"
                    .into(),
            ),
            ProviderConfig::Failover { .. } => Err(
                "[memory.embedding]: a `failover` chain is not taken: vectors of two models \
                 cannot be compared"
                    .into(),
            ),
        }
    }
}

/// What a text is embedded for: Vertex embeds the two differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedPurpose {
    /// A memory, stored.
    Document,
    /// An issue, searched with.
    Query,
}

#[derive(Clone)]
enum Credential {
    None,
    Fixed(String),
    Secret { secrets: Secrets, name: String },
}

impl Credential {
    fn of(env: Option<&str>, secrets: Option<&Secrets>) -> Result<Self, ProviderError> {
        match (env, secrets) {
            (None, _) => Ok(Self::None),
            (Some(name), Some(secrets)) => {
                let credential = Self::Secret {
                    secrets: secrets.clone(),
                    name: name.to_owned(),
                };
                credential.current()?;
                Ok(credential)
            }
            (Some(name), None) => std::env::var(name)
                .map(|v| Self::Fixed(v.trim().to_owned()))
                .map_err(|_| {
                    ProviderError::Config(format!("environment variable {name} is not set"))
                }),
        }
    }

    fn current(&self) -> Result<Option<String>, ProviderError> {
        match self {
            Self::None => Ok(None),
            Self::Fixed(value) => Ok(Some(value.clone())),
            Self::Secret { secrets, name } => secrets
                .get(name)
                .map(|s| Some(s.expose().trim().to_owned()))
                .ok_or_else(|| ProviderError::Config(format!("secret {name} is not loaded"))),
        }
    }

    /// Whether a rotated value was read.
    async fn refresh(&self) -> bool {
        match self {
            Self::None | Self::Fixed(_) => false,
            Self::Secret { secrets, name } => secrets
                .refresh()
                .await
                .iter()
                .any(|changed| changed == name),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Header {
    Bearer,
    AzureKey,
}

#[derive(Clone)]
enum Api {
    OpenAi {
        url: String,
        send_dimensions: bool,
    },
    Vertex {
        url: String,
    },
    #[cfg(feature = "bedrock")]
    Bedrock(aws_sdk_bedrockruntime::Client),
}

/// One embedding model behind one endpoint.
#[derive(Clone)]
pub struct Embedder {
    http: reqwest::Client,
    api: Api,
    credential: Credential,
    header: Header,
    model: String,
    dimensions: u32,
    min_similarity: f32,
    local: bool,
    policy: ModelPolicy,
}

impl fmt::Debug for Embedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Embedder")
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

fn http_client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| ProviderError::Config(format!("HTTP client: {e}")))
}

fn base(url: &str, what: &str) -> Result<String, ProviderError> {
    let trimmed = url.trim().trim_end_matches('/').to_owned();
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        Ok(trimmed)
    } else {
        Err(ProviderError::Config(format!(
            "{what} base_url `{trimmed}` must start with https:// or http://"
        )))
    }
}

/// OpenAI's embeddings URL under `base`.
fn openai_url(base: &str) -> String {
    if base.ends_with("/embeddings") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/embeddings")
    } else {
        format!("{base}/v1/embeddings")
    }
}

impl Embedder {
    /// Keys from the environment (the CLI).
    pub async fn from_config(config: &EmbeddingConfig) -> Result<Self, ProviderError> {
        Self::build(config, None).await
    }

    /// Keys from `secrets`, read again when rotated (the server).
    pub async fn from_config_with_secrets(
        config: &EmbeddingConfig,
        secrets: &Secrets,
    ) -> Result<Self, ProviderError> {
        Self::build(config, Some(secrets)).await
    }

    async fn build(
        config: &EmbeddingConfig,
        secrets: Option<&Secrets>,
    ) -> Result<Self, ProviderError> {
        config.check().map_err(ProviderError::Config)?;
        let model = config.model.trim().to_owned();
        let (api, credential, header, local) = match &config.provider {
            ProviderConfig::Local {
                base_url,
                api_key_env,
                ..
            } => (
                Api::OpenAi {
                    url: openai_url(&base(base_url, "local")?),
                    send_dimensions: false,
                },
                Credential::of(api_key_env.as_deref(), secrets)?,
                Header::Bearer,
                true,
            ),
            ProviderConfig::OpenAiCompat {
                base_url,
                api_key_env,
                azure_api_version,
                ..
            } => {
                let base = base(base_url, "open_ai_compat")?;
                let (url, header) = match azure_api_version {
                    Some(version) => (
                        format!(
                            "{base}/openai/deployments/{model}/embeddings?api-version={version}"
                        ),
                        Header::AzureKey,
                    ),
                    None => (openai_url(&base), Header::Bearer),
                };
                (
                    Api::OpenAi {
                        url,
                        send_dimensions: true,
                    },
                    Credential::of(Some(api_key_env), secrets)?,
                    header,
                    false,
                )
            }
            ProviderConfig::VertexAi {
                project,
                region,
                access_token_env,
                base_url,
            } => {
                let default_base = if region == "global" {
                    "https://aiplatform.googleapis.com".to_owned()
                } else {
                    format!("https://{region}-aiplatform.googleapis.com")
                };
                let base = base(base_url.as_deref().unwrap_or(&default_base), "vertex_ai")?;
                (
                    Api::Vertex {
                        url: format!(
                            "{base}/v1/projects/{project}/locations/{region}/publishers/google/models/{model}:predict"
                        ),
                    },
                    Credential::of(Some(access_token_env), secrets)?,
                    Header::Bearer,
                    false,
                )
            }
            ProviderConfig::Bedrock { region, .. } => (
                bedrock(region).await?,
                Credential::None,
                Header::Bearer,
                false,
            ),
            ProviderConfig::ClaudeApi { .. } | ProviderConfig::Failover { .. } => {
                unreachable!("refused by EmbeddingConfig::check")
            }
        };
        Ok(Self {
            http: http_client()?,
            api,
            credential,
            header,
            model,
            dimensions: config.dimensions,
            min_similarity: config.min_similarity,
            local,
            policy: ModelPolicy::Any,
        })
    }

    /// The same embedder, refusing a cloud endpoint when `policy` is
    /// [`ModelPolicy::LocalOnly`]. Model prefixes are not checked: an
    /// embedding model has no inference profile, and runs in the region
    /// its endpoint names, which the mode's `[residency]` checked.
    pub fn with_policy(self, policy: ModelPolicy) -> Self {
        Self { policy, ..self }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn dimensions(&self) -> u32 {
        self.dimensions
    }

    pub fn min_similarity(&self) -> f32 {
        self.min_similarity
    }

    pub fn is_local(&self) -> bool {
        self.local
    }

    /// One vector of [`Self::dimensions`] per text, in order.
    pub async fn embed(
        &self,
        texts: &[String],
        purpose: EmbedPurpose,
    ) -> Result<Vec<Vec<f32>>, ProviderError> {
        self.policy.check_endpoint(&self.model, self.local)?;
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_BATCH) {
            let inputs: Vec<String> = chunk
                .iter()
                .map(|t| t.chars().take(MAX_TEXT_CHARS).collect())
                .collect();
            let vectors = self.embed_batch(&inputs, purpose).await?;
            if vectors.len() != inputs.len() {
                return Err(ProviderError::Decode(format!(
                    "{} vectors for {} texts",
                    vectors.len(),
                    inputs.len()
                )));
            }
            for v in &vectors {
                if v.len() != self.dimensions as usize {
                    return Err(ProviderError::Decode(format!(
                        "a vector of {} dimensions, not the {} configured",
                        v.len(),
                        self.dimensions
                    )));
                }
            }
            out.extend(vectors);
        }
        Ok(out)
    }

    async fn embed_batch(
        &self,
        inputs: &[String],
        purpose: EmbedPurpose,
    ) -> Result<Vec<Vec<f32>>, ProviderError> {
        match &self.api {
            Api::OpenAi {
                url,
                send_dimensions,
            } => {
                let mut body = json!({ "model": self.model, "input": inputs });
                if *send_dimensions {
                    body["dimensions"] = json!(self.dimensions);
                }
                let text = self.post(url, &body).await?;
                let parsed: OpenAiEmbeddings = serde_json::from_str(&text)
                    .map_err(|e| ProviderError::Decode(format!("embeddings: {e}")))?;
                let mut data = parsed.data;
                data.sort_by_key(|d| d.index);
                Ok(data.into_iter().map(|d| d.embedding).collect())
            }
            Api::Vertex { url } => {
                let task = match purpose {
                    EmbedPurpose::Document => "RETRIEVAL_DOCUMENT",
                    EmbedPurpose::Query => "RETRIEVAL_QUERY",
                };
                let instances: Vec<Value> = inputs
                    .iter()
                    .map(|t| json!({ "content": t, "task_type": task }))
                    .collect();
                let body = json!({
                    "instances": instances,
                    "parameters": { "outputDimensionality": self.dimensions },
                });
                let text = self.post(url, &body).await?;
                let parsed: VertexPredictions = serde_json::from_str(&text)
                    .map_err(|e| ProviderError::Decode(format!("predictions: {e}")))?;
                Ok(parsed
                    .predictions
                    .into_iter()
                    .map(|p| p.embeddings.values)
                    .collect())
            }
            #[cfg(feature = "bedrock")]
            Api::Bedrock(client) => {
                let mut out = Vec::with_capacity(inputs.len());
                for input in inputs {
                    out.push(titan(client, &self.model, input, self.dimensions).await?);
                }
                Ok(out)
            }
        }
    }

    /// POSTs `body`, once more after a 401 or 403 when the key was rotated.
    async fn post(&self, url: &str, body: &Value) -> Result<String, ProviderError> {
        let (mut status, mut retry_after_s, mut text) = self.post_once(url, body).await?;
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && self.credential.refresh().await
        {
            (status, retry_after_s, text) = self.post_once(url, body).await?;
        }
        if status.is_success() {
            Ok(text)
        } else {
            Err(error_from(status, retry_after_s, &text))
        }
    }

    async fn post_once(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<(StatusCode, Option<u64>, String), ProviderError> {
        let mut headers = HeaderMap::new();
        if let Some(key) = self.credential.current()? {
            let (name, value) = match self.header {
                Header::Bearer => (AUTHORIZATION, format!("Bearer {key}")),
                Header::AzureKey => (AZURE_API_KEY_HEADER, key),
            };
            let mut value = HeaderValue::from_str(&value)
                .map_err(|_| ProviderError::Config("key contains invalid characters".into()))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(send_error)?;
        let status = response.status();
        let retry_after_s = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.without_url().to_string()))?;
        Ok((status, retry_after_s, text))
    }
}

#[derive(Deserialize)]
struct OpenAiEmbeddings {
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct VertexPredictions {
    predictions: Vec<VertexPrediction>,
}

#[derive(Deserialize)]
struct VertexPrediction {
    embeddings: VertexValues,
}

#[derive(Deserialize)]
struct VertexValues {
    values: Vec<f32>,
}

#[cfg(feature = "bedrock")]
async fn bedrock(region: &str) -> Result<Api, ProviderError> {
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(region.to_owned()))
        .load()
        .await;
    Ok(Api::Bedrock(aws_sdk_bedrockruntime::Client::new(&config)))
}

#[cfg(not(feature = "bedrock"))]
async fn bedrock(_region: &str) -> Result<Api, ProviderError> {
    Err(ProviderError::Config(
        "Bedrock embeddings requested but aegis-provider was built without the `bedrock` feature"
            .into(),
    ))
}

/// Titan Text Embeddings v2 body for `input`.
#[cfg(any(feature = "bedrock", test))]
fn titan_body(input: &str, dimensions: u32) -> Value {
    json!({ "inputText": input, "dimensions": dimensions, "normalize": true })
}

#[cfg(feature = "bedrock")]
async fn titan(
    client: &aws_sdk_bedrockruntime::Client,
    model: &str,
    input: &str,
    dimensions: u32,
) -> Result<Vec<f32>, ProviderError> {
    use aws_sdk_bedrockruntime::primitives::Blob;

    #[derive(Deserialize)]
    struct Titan {
        embedding: Vec<f32>,
    }
    let bytes = serde_json::to_vec(&titan_body(input, dimensions))
        .map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;
    let output = client
        .invoke_model()
        .model_id(model)
        .content_type("application/json")
        .accept("application/json")
        .body(Blob::new(bytes))
        .send()
        .await
        .map_err(crate::bedrock::error_from)?;
    let parsed: Titan = serde_json::from_slice(output.body().as_ref())
        .map_err(|e| ProviderError::Decode(format!("titan: {e}")))?;
    Ok(parsed.embedding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalProtocol;
    use wiremock::matchers::{body_partial_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn local(base_url: &str, dimensions: u32) -> EmbeddingConfig {
        EmbeddingConfig {
            provider: ProviderConfig::Local {
                base_url: base_url.into(),
                api_key_env: None,
                protocol: LocalProtocol::OpenAi,
            },
            model: "nomic-embed-text".into(),
            dimensions,
            min_similarity: DEFAULT_MIN_SIMILARITY,
        }
    }

    #[tokio::test]
    async fn a_local_endpoint_embeds_in_order_and_in_batches() {
        let server = MockServer::start().await;
        // Answers out of order: the index decides.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_partial_json(json!({ "model": "nomic-embed-text" })))
            .respond_with(|req: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let n = body["input"].as_array().unwrap().len();
                assert!(body.get("dimensions").is_none(), "Ollama is not sent one");
                let data: Vec<Value> = (0..n)
                    .rev()
                    .map(|i| json!({ "index": i, "embedding": [i as f32, 1.0] }))
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({ "data": data }))
            })
            .expect(2)
            .mount(&server)
            .await;
        let embedder = Embedder::from_config(&local(&format!("{}/v1", server.uri()), 2))
            .await
            .unwrap();
        assert!(embedder.is_local());
        let texts: Vec<String> = (0..20).map(|i| format!("text {i}")).collect();
        let vectors = embedder
            .embed(&texts, EmbedPurpose::Document)
            .await
            .unwrap();
        assert_eq!(vectors.len(), 20);
        assert_eq!(vectors[3], vec![3.0, 1.0]);
        assert_eq!(
            vectors[19],
            vec![3.0, 1.0],
            "second batch restarts its index"
        );
    }

    #[tokio::test]
    async fn a_vector_of_the_wrong_length_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({ "data": [{ "index": 0, "embedding": [1.0, 2.0, 3.0] }] }),
                ),
            )
            .mount(&server)
            .await;
        let embedder = Embedder::from_config(&local(&server.uri(), 2))
            .await
            .unwrap();
        let err = embedder
            .embed(&["x".into()], EmbedPurpose::Query)
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Decode(_)), "{err}");
    }

    #[tokio::test]
    async fn azure_uses_the_deployment_url_and_its_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/embed-3/embeddings"))
            .and(query_param("api-version", "2024-10-21"))
            .and(header("api-key", "k-azure"))
            .and(body_partial_json(json!({ "dimensions": 2 })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "data": [{ "index": 0, "embedding": [0.5, 0.5] }] })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let config = EmbeddingConfig {
            provider: ProviderConfig::OpenAiCompat {
                base_url: server.uri(),
                api_key_env: "AZURE_EMBED_KEY".into(),
                azure_api_version: Some("2024-10-21".into()),
                region: Some("japaneast".into()),
            },
            model: "embed-3".into(),
            dimensions: 2,
            min_similarity: 0.4,
        };
        let secrets = Secrets::fixed([("AZURE_EMBED_KEY", "k-azure")]);
        let embedder = Embedder::from_config_with_secrets(&config, &secrets)
            .await
            .unwrap();
        assert!(!embedder.is_local());
        let v = embedder
            .embed(&["x".into()], EmbedPurpose::Document)
            .await
            .unwrap();
        assert_eq!(v, vec![vec![0.5, 0.5]]);
    }

    #[tokio::test]
    async fn vertex_predicts_with_the_task_and_the_dimensions() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/p1/locations/asia-northeast1/publishers/google/models/text-multilingual-embedding-002:predict",
            ))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(json!({
                "instances": [{ "content": "障害", "task_type": "RETRIEVAL_QUERY" }],
                "parameters": { "outputDimensionality": 3 },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "predictions": [{ "embeddings": { "values": [0.1, 0.2, 0.3] } }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let config = EmbeddingConfig {
            provider: ProviderConfig::VertexAi {
                project: "p1".into(),
                region: "asia-northeast1".into(),
                access_token_env: "VERTEX_TOKEN".into(),
                base_url: Some(server.uri()),
            },
            model: "text-multilingual-embedding-002".into(),
            dimensions: 3,
            min_similarity: DEFAULT_MIN_SIMILARITY,
        };
        let secrets = Secrets::fixed([("VERTEX_TOKEN", "tok")]);
        let embedder = Embedder::from_config_with_secrets(&config, &secrets)
            .await
            .unwrap();
        let v = embedder
            .embed(&["障害".into()], EmbedPurpose::Query)
            .await
            .unwrap();
        assert_eq!(v, vec![vec![0.1, 0.2, 0.3]]);
    }

    #[tokio::test]
    async fn local_only_refuses_a_cloud_endpoint_before_any_call() {
        // Nothing listens there: a request that got through would fail
        // with a transport error, not a policy one.
        let config = EmbeddingConfig {
            provider: ProviderConfig::OpenAiCompat {
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "K".into(),
                azure_api_version: None,
                region: None,
            },
            model: "m".into(),
            dimensions: 2,
            min_similarity: DEFAULT_MIN_SIMILARITY,
        };
        let embedder = Embedder::from_config_with_secrets(&config, &Secrets::fixed([("K", "k")]))
            .await
            .unwrap()
            .with_policy(ModelPolicy::LocalOnly);
        let err = embedder
            .embed(&["x".into()], EmbedPurpose::Query)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::PolicyViolation { .. }),
            "{err}"
        );
    }

    #[test]
    fn the_config_refuses_what_cannot_embed() {
        let mut config = local("http://localhost:11434/v1", 768);
        assert!(config.check().is_ok());
        config.dimensions = 0;
        assert!(config.check().is_err());
        config.dimensions = MAX_DIMENSIONS + 1;
        assert!(config.check().is_err());
        config.dimensions = 768;
        config.min_similarity = 1.5;
        assert!(config.check().is_err());
        config.min_similarity = 0.5;
        config.provider = ProviderConfig::ClaudeApi {
            api_key_env: "ANTHROPIC_API_KEY".into(),
            base_url: None,
        };
        assert!(config.check().unwrap_err().contains("Claude API"));
        let text = "model = \"nomic-embed-text\"\ndimensions = 768\n\
                    [provider]\nkind = \"local\"\nbase_url = \"http://ollama:11434/v1\"\n";
        let parsed: EmbeddingConfig = toml::from_str(text).unwrap();
        assert_eq!(parsed.min_similarity, DEFAULT_MIN_SIMILARITY);
    }

    #[test]
    fn titan_is_asked_for_normalized_vectors() {
        assert_eq!(
            titan_body("x", 512),
            json!({ "inputText": "x", "dimensions": 512, "normalize": true })
        );
    }
}
