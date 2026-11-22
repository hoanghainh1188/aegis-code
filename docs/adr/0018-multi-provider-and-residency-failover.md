# ADR 0018 – Multi-Provider LLM Abstraction and Residency-Safe Failover

- Date: 2026-11-22
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #595 (Milestone: `Phase 3 – Mở rộng`)
- Related: [ADR 0001](0001-platform-in-rust.md), [ADR 0003](0003-platform-service-architecture.md), #592 (dynamic modes and `[residency]`)

## Context

`aegis-provider` originally supported three backends: direct `claude_api`, AWS `bedrock` (Anthropic Messages format), and `local` (Ollama / vLLM). For **Self-Hosted Enterprise** deployments, customers operate across diverse cloud commitments and data-residency boundaries:

- **Google Cloud Vertex AI** (`asia-northeast1` Tokyo, `europe-west4`, `us-central1`) running either Gemini models or Claude-on-Vertex;
- **Azure OpenAI** (`japaneast`, `swedencentral`) and **OpenAI-compatible Enterprise Gateways** (e.g., LiteLLM, internal proxies);
- **High-availability failover** across two regional backends (e.g., AWS Bedrock `ap-northeast-1` primary with Vertex AI `asia-northeast1` secondary) when one provider experiences rate limiting (`429`), overload (`529`/`503`), or transient `5xx` outages.

Adding providers must not weaken two core platform invariants:

1. **Deterministic Prompt Caching (Layer 2):** maintain ≥ 70% cache read ratio across providers by preserving byte-stable prefixes (`BP1`–`BP3`) and mapping each provider's cache-read counters into `Usage.cache_read_input_tokens`.
2. **Fail-Fast Data Residency:** a failover chain must never silently route customer code outside the mode's declared `[residency]` boundary.

## Decision

### 1. New `ProviderConfig` Variants
Extend `ProviderConfig` in `crates/aegis-provider/src/config.rs` with three variants:

- **`openai_compat` (`OpenAiCompatProvider`):**
  - Supports standard OpenAI-compatible gateways (`Authorization: Bearer <key>`, `/v1/chat/completions`) and **Azure OpenAI** (`api-key: <key>`, `/openai/deployments/{model}/chat/completions?api-version={azure_api_version}`).
  - Reuses the shared OpenAI request/response serializer (`prompt_tokens_details.cached_tokens` → `Usage.cache_read_input_tokens`, subtracting cached tokens from uncached `input_tokens`).
  - Supports live secret rotation via `aegis_secrets::Secrets` (re-reading on `401`/`403`) and an optional `region` tag for residency checks.
- **`vertex_ai` (`VertexAiProvider`):**
  - Configured with `project`, `region`, `access_token_env` (OAuth2 bearer token loaded from `Secrets` or environment), and optional `base_url` override (default `https://{region}-aiplatform.googleapis.com`, or `https://aiplatform.googleapis.com` for `global`).
  - Routes automatically by model family:
    - `claude-*` models use `:rawPredict` with `anthropic_version = "vertex-2023-10-16"` and explicit `BP1`–`BP4` cache breakpoints (`body::Target::VertexClaude`).
    - Gemini models use `:generateContent`, preserving byte-stable `systemInstruction` + `functionDeclarations` order for implicit prefix caching and mapping `usageMetadata.cachedContentTokenCount` into `Usage.cache_read_input_tokens`.
- **`failover` (`FailoverProvider`):**
  - Configured with `retryable_only: bool` (default `true`) and `chain: Vec<ProviderConfig>` (non-empty; nested `failover` inside `chain` is refused).
  - Iterates through `chain` in order. Fails over to the next backend **only** when the error is retryable (`ProviderError::is_retryable()`: `429`, `529`/`503` overload, `5xx`, transport error) when `retryable_only = true`.
  - **Never fails over** on `ProviderError::PolicyViolation`, `InvalidRequest`, `400` client errors, or `Decode` errors.

### 2. Residency Guard Extended to Regions and Chains
Extend `ResidencyConfig` in `crates/aegis-provider/src/guard.rs` with optional `allowed_regions: Option<Vec<String>>`:

- When `allowed_regions` is set, every backend (and every member of `Failover::chain`) must declare a `region` present in `allowed_regions`; `claude_api` or any backend without a matching `region` is rejected at startup.
- When `local_only = true`, every member of `Failover::chain` must be `ProviderConfig::Local` passing `check_local_url`.
- `allowed_model_prefixes` continues to be enforced both at startup across all model tiers and before every runtime request on every backend in the chain.

### 3. Prompt Caching Mapping

| Provider | Caching Mechanism | Mapping to `Usage` |
| :--- | :--- | :--- |
| `claude_api` / `bedrock` / `vertex_ai` (Claude) | Explicit `cache_control` (`BP1`–`BP4`) | Direct `cache_read_input_tokens` / `cache_creation_input_tokens` |
| `vertex_ai` (Gemini) | Implicit prefix caching (byte-stable prefix) | `usageMetadata.cachedContentTokenCount` → `cache_read_input_tokens` |
| `openai_compat` (OpenAI / Azure) | Automatic prefix caching (≥ 1024 tokens) | `prompt_tokens_details.cached_tokens` → `cache_read_input_tokens` |
| `local` (Ollama / vLLM) | KV-cache prefix reuse | `prompt_tokens_details.cached_tokens` when reported |

## Consequences

- **Positive:** Self-hosted enterprise customers can run Aegis on AWS Bedrock, GCP Vertex AI, Azure OpenAI, internal gateways, or local servers—or pair two regional backends in a residency-safe failover chain—without code changes and with zero new external crates added to `Cargo.lock`.
- **Positive:** Prompt caching and cost accounting (`aegis-ledger`) remain uniform across all backends.
- **Negative:** Vertex AI OAuth2 access tokens are read from `aegis-secrets` (rotated externally by workload identity / sidecar / secret refresher) rather than bundling a GCP OAuth JWT signer crate; keeping the dependency tree minimal is preferred until a customer requires built-in SA JSON signing.
