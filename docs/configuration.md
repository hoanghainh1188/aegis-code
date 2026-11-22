# Configuration Reference (`aegis-code`)

This document is the single reference for all configuration surfaces of **Aegis Platform (`aegis-code`)**:

1. **Server configuration (`server.toml`)** — instance-wide settings for `aegis-server` (`--role api|worker|all`).
2. **Mode configuration (`config/*.toml`)** — LLM provider, model tiers, data residency, sandbox runtime, and execution defaults shared by repos using that mode.
3. **Repository settings (`repos.config` / `[settings]`)** — per-repository forge connection, language overrides, policies, sidecars, SonarQube, ITSM trackers, APM telemetry, and diagnosis lane.

All configuration structs use `#[serde(deny_unknown_fields)]`: typos or unrecognized keys fail fast at startup or registration. **No secret value ever goes into a configuration file**; configs name secret keys resolved at runtime by `aegis-secrets`.

---

## 1. Server Configuration (`server.toml`)

Passed to `aegis-server --config /etc/aegis/server.toml`. Relative paths are resolved from the server's working directory. See [config/server.toml](../config/server.toml) for a commented template.

### `[secrets]` (Required)

Where `aegis-server` reads secrets at boot and on live rotation.

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `source` | `"aws"` \| `"file"` \| `"env"` | *(required)* | Secret backend: AWS Secrets Manager (`aws`), permission-checked files (`file`, `0400`/`0600`), or process env (`env`, dev/tests only). |
| `region` | `string` | `None` | Required when `source = "aws"`. Must be `"ap-northeast-1"` if a `customer-jp` mode is configured. |
| `prefix` | `string` | `""` | Prefix prepended to secret names in AWS Secrets Manager (e.g., `"aegis/prod/"`). |
| `dir` | `path` | `None` | Required when `source = "file"` (e.g., `"/run/secrets"`). Symlinks must stay inside `dir`. |
| `refresh_s` | `u64` | `300` | Background refresh interval in seconds for live secret rotation (minimum `1`). |

### `[modes]`

Maps mode names (1–64 chars of `a-z`, `0-9`, `_`, `-`; `_` and `-` are equivalent, so `customer_jp` and `customer-jp` name the same mode) to their mode TOML file paths. Any repo whose `repos.mode` is absent from `[modes]` will not run.

```toml
[modes]
poc = "config/poc.toml"
customer-jp = "config/customer-jp.toml"
customer-eu = "/etc/aegis/modes/customer-eu.toml"
local-airgap = "/etc/aegis/modes/local-airgap.toml"
```

### `[api]` & `[metrics]`

| Section & Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `api.listen` | `SocketAddr` | `"127.0.0.1:8080"` | HTTP listener for `/api/v1/*`, webhooks, and SSR dashboard (place behind TLS reverse proxy / ALB). |
| `metrics.listen` | `SocketAddr` | `"127.0.0.1:9464"` | Dedicated Prometheus `/metrics` listener (private network only; unauthenticated, exposes counts only). |

### `[auth]` & `[auth.oidc]`

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `public_url` | `URL` | `None` | Public dashboard origin (scheme + host only, e.g., `"https://aegis.example.com"`). Must be `https://` unless loopback. Required for OIDC. |
| `session_hours` | `u32` | `12` | Browser session lifetime in hours. |
| `api_requests_per_minute` | `u32` | `120` | Rate limit per service token, per API process. |
| `api_failures_per_minute` | `u32` | `20` | Unauthenticated/invalid-token rate limit per client IP, per API process. |
| `trusted_proxies` | `u8` | `0` | Number of reverse proxies appending `X-Forwarded-For` (`1` behind ALB; `0` ignores the header and uses TCP peer). |
| `oidc.issuer` | `string` | *(required if `[auth.oidc]` set)* | OIDC issuer URL; discovery reads `<issuer>/.well-known/openid-configuration`. |
| `oidc.client_id` | `string` | *(required if `[auth.oidc]` set)* | OIDC client ID. Client secret is read from `AEGIS_OIDC_CLIENT_SECRET`. |

### `[budget]`

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `time_zone` | `string` | `"UTC"` | IANA time zone defining month boundaries for monthly repo/tenant budgets (e.g., `"Asia/Tokyo"`). |
| `warn_ratio` | `f64` | `0.8` | Fraction of monthly budget cap (`0.0`–`1.0`) at which dashboard warnings and ChatOps alerts trigger. |

### `[worker]`

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `max_workers` | `u32` | `4` | Concurrent jobs executed by this worker process. |
| `max_cost_usd` | `f64` | `4.0` | Hard per-job USD ceiling across all attempts (repos may set a lower cap). |
| `lease_s` | `u64` | `60` | Distributed PostgreSQL job lease duration in seconds. |
| `renew_s` | `u64` | `20` | Lease renewal interval in seconds (also controls emergency-stop reaction time). |
| `poll_s` | `u64` | `5` | Idle queue poll interval in seconds. |
| `drain_s` | `u64` | `600` | Graceful `SIGTERM` drain timeout in seconds before releasing running jobs back to the queue. |
| `retry_base_s` | `u64` | `30` | Base exponential backoff in seconds after infrastructure failures (up to 3 retries). |
| `sla_aging_s` | `u64` | `3600` | Wait duration in seconds that promotes a queued job by one SLA level (`P3 → P2 → P1`) to prevent starvation. |
| `max_cache_disk_gb` | `u64` | `60` | Total build cache quota on this worker's disk in GB; sweeper evicts LRU caches down to 85% (`0` disables quota). |
| `cache_ttl_days` | `u64` | `14` | Evict unused repo build caches older than this many days (`0` disables TTL). |

### `[trackers]`, `[sonar]`, `[reviews]`, `[web]`

| Section & Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `trackers.poll_s` | `u64` | `300` | Interval in seconds between Jira/Backlog/JSM/ServiceNow polls (`0` disables polling). |
| `sonar.check_s` | `u64` | `60` | Interval in seconds to check which repos are due for Sonar warning polling (`0` disables). |
| `reviews.max_rounds` | `u32` | `3` | Maximum automated review rounds (`/aegis fix` / *Request changes*) per PR. |
| `web.make_webhooks` | `bool` | `false` | Allow admins to create forge webhooks from the UI using a one-time, unstored admin token. |

### `[[chatops.channels]]`

Optional Slack and Microsoft Teams webhook notifications (each webhook URL is stored as a secret, never inline).

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `kind` | `"slack"` \| `"teams"` | *(required)* | Target chat platform (Slack Block Kit or Teams Adaptive Card / Workflows). |
| `webhook_env` | `string` | *(required)* | Secret name in `[secrets]` holding the incoming webhook URL. |
| `tenants` | `string[]` | `[]` | Restrict notifications to these tenant slugs (empty = all tenants). |
| `repos` | `string[]` | `[]` | Restrict notifications to these repo slugs (empty = all repos). |
| `events` | `string[]` | *(all)* | Filter events: `"pr_opened"`, `"diagnosed"`, `"p1_stopped"`, `"budget"`. |

### `[github_app]`

Enables the GitHub App integration ([ADR 0017](adr/0017-github-app.md)) for repos using `[settings.app]`. Omit to use only `[settings.fork]` tokens.

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `app_id` | `u64` | `None` | GitHub App numeric ID (set **either** `app_id` **or** `client_id`, not both). |
| `client_id` | `string` | `None` | GitHub App client ID (GitHub's preferred JWT issuer). |
| `api` | `string` | `"https://api.github.com"` | GitHub API root (set to `"https://<host>/api/v3"` for GitHub Enterprise Server). |
| `private_key_env` | `string` | `"AEGIS_GITHUB_APP_PRIVATE_KEY"` | Secret name holding the App's PEM private key. |
| `webhook_secret_env` | `string` | `"AEGIS_GITHUB_APP_WEBHOOK_SECRET"` | Secret name holding the App's webhook HMAC secret. |

### Standard Instance Secret Names

| Secret Name | Used By | Purpose |
| :--- | :--- | :--- |
| `AEGIS_DATABASE_URL` | All roles | PostgreSQL connection string (`postgres://...`). |
| `AEGIS_SESSION_KEY` | `api`, `all` | ≥ 32 random bytes for signing dashboard session/review timestamps. |
| `AEGIS_OIDC_CLIENT_SECRET` | `api`, `all` | OIDC client secret when `[auth.oidc]` is configured. |
| `AEGIS_GITHUB_WEBHOOK_SECRET` | `api`, `all` | HMAC secret for token-path GitHub webhooks. |
| `AEGIS_GITLAB_WEBHOOK_TOKEN` | `api`, `all` | Secret token for GitLab webhooks. |
| `AEGIS_GITHUB_COMMENT_TOKEN` | `api`, `worker`, `all` | Token for commenting on upstream GitHub issues (token path). |
| `AEGIS_GITHUB_APP_PRIVATE_KEY` | `api`, `worker`, `all` | GitHub App PEM key (when `[github_app]` is set). |
| `AEGIS_GITHUB_APP_WEBHOOK_SECRET` | `api`, `all` | GitHub App webhook HMAC secret (when `[github_app]` is set). |

---

## 2. Mode Configuration (`config/*.toml`)

Each entry in `[modes]` points to a mode file defining the LLM provider, model tiers, residency policy, and sandbox defaults. In server mode, any `[repo]` and `[sonar]` sections in a mode file are ignored (registered repos supply their own).

### `[provider]` (Required)

Tagged by `kind`:

| `kind` | Keys | Description |
| :--- | :--- | :--- |
| `"claude_api"` | `api_key_env` (default `"ANTHROPIC_API_KEY"`), `base_url` (optional) | Direct Anthropic Messages API. |
| `"bedrock"` | `region` (required, e.g., `"ap-northeast-1"`), `require_jp_profile` (required `bool`) | AWS Bedrock Runtime (requires binary built with `--features bedrock`). |
| `"local"` | `base_url` (required), `api_key_env` (optional), `protocol` (`"ollama"` \| `"openai"`, default `"ollama"`) | Local or private-network inference server (Ollama, vLLM). |

### `[residency]`

Enforces data residency both at startup (fail-fast) and before every model request. Set **at most one** of `allowed_model_prefixes` or `local_only`.

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `allowed_model_prefixes` | `string[]` | `None` | Every tier's model ID must start with one of these prefixes (e.g., `["jp."]`, `["eu."]`). |
| `local_only` | `bool` | `false` | Allows only `provider.kind = "local"` whose `base_url` resolves to loopback, RFC1918/link-local IP, dotless container name, `.local`/`.internal`/`.lan`/`.localhost`/`.home.arpa`, or `local_hosts`. |
| `local_hosts` | `string[]` | `[]` | Additional internal DNS hostnames accepted when `local_only = true` (e.g., `["vllm.corp.example"]`). |

*(Note: any mode named `customer-jp` or setting `require_jp_profile = true` automatically enforces the Japan guard: Bedrock in `ap-northeast-1` with `jp.*` models across all tiers.)*

### `[models.light]`, `[models.core]`, `[models.expert]` (Required)

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `id` | `string` | *(required)* | Provider model ID or regional inference profile (e.g., `"jp.anthropic.claude-sonnet-..."`). |
| `effort` | `"low"` \| `"medium"` \| `"high"` | `None` | Optional extended thinking / reasoning effort parameter. |

### `[run]` (Required)

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `state_dir` | `path` | *(required)* | Root directory for clones, job worktrees, AST maps, build caches, and JSON ledgers. |
| `pricing` | `path` | *(required)* | Path to model pricing table (e.g., `"config/pricing.toml"`). |
| `aci_bin` | `path` | *(sibling binary)* | Path to `aegis-aci` MCP server binary. |
| `step_timeout_s` | `u64` | `1800` | Per-gate-step execution timeout in seconds. |
| `max_tokens` | `u32` | `16000` | Max output tokens per model call. |
| `context_window` | `u64` | `200000` | Model context window size (batch observation collapse triggers at 60%). |
| `draft_prs` | `bool` | `true` | Open pull requests as drafts. |
| `open_prs` | `bool` | `true` | Push branch and open PR after passing gate (`false` for eval dry runs). |
| `sandbox` | `bool` | `false` | Execute gate steps inside isolated Docker containers (`true` in production). |
| `docker` | `string` | `"docker"` | Docker CLI binary name/path. |
| `sandbox_env` | `map<string,string>` | `{}` | Extra environment variables injected into gate containers (e.g., `CARGO_BUILD_JOBS = "2"`). |
| `sandbox_limits` | table | `memory_mb=8192`, `cpus=4`, `pids=4096`, `tmp_mb=1024` | Per-container resource caps (overridable by language profile). |
| `sandbox_runtime` | `string` | `None` | OCI runtime for untrusted code containers (`"runc"` or `"runsc"` for gVisor). |
| `egress_image` | `string` | `"aegis-sandbox-python"` | Image used for the pre-gate dependency fetch egress proxy (must contain Python 3). |
| `egress_ready_timeout_s` | `u64` | `30` | Timeout waiting for the egress proxy container to listen. |
| `ledger` | `"json"` \| `"postgres"` | `"json"` | Ledger storage backend (`"postgres"` in server deployments). |

### `[budget]`, `[identity]`, `[codebase_memory]`

| Section & Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `budget.max_cost_usd` | `f64` | `4.0` | Default per-job USD cap for this mode. |
| `budget.max_model_calls` | `u32` | `None` | Override call cap across all languages (unset = language profile default: 40, or 80 for Rust). |
| `identity.name` / `identity.email` | `string` | *(required)* | Git commit author name and email for agent commits. |
| `codebase_memory.program` / `args` / `env` / `tools` | table | `None` | Optional Codebase-Memory MCP server (`{index_dir}` expands to `<state_dir>/index`). |

### Example: EU Residency Mode (`customer-eu.toml`)

```toml
[provider]
kind = "bedrock"
region = "eu-central-1"
require_jp_profile = false

[residency]
allowed_model_prefixes = ["eu."]

[models.light]
id = "eu.anthropic.claude-haiku-5-5-v1:0"

[models.core]
id = "eu.anthropic.claude-sonnet-5-5-v1:0"

[models.expert]
id = "eu.anthropic.claude-opus-5-5-v1:0"
effort = "high"

[run]
state_dir = "/srv/aegis/state-eu"
pricing = "config/pricing.toml"
sandbox = true
ledger = "postgres"

[identity]
name = "aegis-bot"
email = "aegis-bot@example.com"
```

---

## 3. Repository Settings (`repos.config` / `[settings]`)

Stored as JSONB in `repos.config` (registered via `aegis-server repo add`, `add-repo.sh`, or the admin dashboard). Top-level repository metadata (`slug`, `url`, `default_branch`, `language`, `mode`, `max_concurrent_jobs`, `tenant`) lives in table columns; the fields below live inside `[settings]` (or `repos.config`).

### 3.1. Forge Connection (Exactly one of `fork` or `app` is required)

- **`[settings.fork]`** (Token path — GitHub or GitLab):
  - `platform`: `"github"` (default) or `"gitlab"`
  - `owner`, `name`: fork (or upstream) repository/project path
  - `api`: API root (default `"https://api.github.com"`; required for GitLab, e.g., `"https://gitlab.com/api/v4"`)
  - `token_env`: secret name holding the bot/project token (must start with `<TENANT>_` for non-default tenants)
  - `push_to_local`: optional local bare repo path for dry runs
- **`[settings.app]`** (GitHub App path — [ADR 0017](adr/0017-github-app.md), GitHub only):
  - `api`: optional GitHub Enterprise Server API root (defaults to `[github_app].api`). No secret name is needed.

### 3.2. Language, Monorepo & Policy Settings

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `conventions` | `string` | `None` | Repository-specific coding/testing conventions appended to the system prompt. |
| `image` | `string` | `None` | Override the language profile's sandbox Docker image. |
| `overrides` | table | `{}` | Override commands/settings from `profiles/<lang>.toml` (e.g., `fetch`, `gate.test`, `coverage`, `dependencies.upgrade`). |
| `dirs` | `DirConfig[]` | `[]` | Subdirectories using a different language profile (`path`, `language`, `overrides`, `hidden_test`). |
| `allowed_packages` | `string[]` | `[]` | Additional package names a patch is allowed to add. |
| `licenses` | `string[]` | `None` | Allowed dependency licenses (replaces `policies/licenses.toml`). |
| `ignored_advisories` | `string[]` | `[]` | Advisory IDs (`RUSTSEC-...`, `GHSA-...`) allowed through the audit step. |
| `budget` | `BudgetSection` | `None` | Per-repo override of `max_cost_usd` and `max_model_calls`. |
| `issue_trust` | `"any"` \| `"collaborators"` | `"any"` | Restrict who may trigger jobs by labeling an issue (also accepts `{ policy = "collaborators" }`). |
| `patch_scope` | table | defaults | Patch scope guards (`max_files`, `max_dirs`, `allow_test_deletion`, `allow_test_skips`, `enforce`). |
| `secrets.allow` | `SecretException[]` | `[]` | Exceptions for the gate's `secrets` step (`path` glob, optional `kinds[]`, required `reason` ≤ 200 chars). |

### 3.3. Ephemeral Database Sidecars (`[[settings.sidecars]]`)

Requires `run.sandbox = true` and `--sidecar-image <image>` on `aegis-docker-proxy` ([ADR 0013](adr/0013-ephemeral-database-sidecars.md)).

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `name` | `string` | *(required)* | DNS hostname on the internal sidecar network (e.g., `"postgres"`, `"redis"`). |
| `image` | `string` | *(required)* | Allow-listed container image (e.g., `"postgres:16-alpine"`). |
| `user` | `string` | *(required)* | Non-root numeric `uid:gid` (e.g., `"70:70"` for Postgres Alpine, `"999:999"` for Redis). |
| `env` | `map<string,string>` | `{}` | Container environment variables (e.g., `POSTGRES_PASSWORD`). |
| `tmpfs` | `string[]` | `[]` | Writable tmpfs directories owned by `user` (e.g., `["/var/lib/postgresql/data"]`). |
| `ready` | `string[]` | *(required)* | Readiness probe command run inside the sidecar's network namespace. |
| `ready_timeout_s` | `u64` | `60` | Readiness probe timeout in seconds. |
| `localhost` | `bool` | `false` | Run test steps in this sidecar's network namespace so tests reach it on `localhost` (at most one per repo). |
| `test_env` | `map<string,string>` | `{}` | Environment variables injected into test steps (e.g., `DATABASE_URL`). |
| `memory_mb` / `cpus` | `u64` / `u32` | `1024` / `2` | Optional resource caps for the sidecar container. |

### 3.4. Diagnosis Lane, SonarQube, Trackers & APM

- **`[settings.triage_lane]`** (Off by default; [ADR 0012](adr/0012-triage-and-diagnostic-lane.md)):
  - `categories`: subset of `["ops_or_config", "data_issue", "needs_info"]` (`code_bug` is forbidden).
  - `min_confidence`: `f64` in `0.0..=1.0` (default `0.8`).
- **`[settings.sonar]`** & **`[settings.sonar_poll]`**:
  - `sonar`: `url`, `token_env`, `project_key`, optional `organization` (SonarCloud), `pull_request` (default `true`), `scanner`, `scanner_image`, `network` (default `"bridge"`), `wait_s` (`600`), `poll_s` (`3`), `properties`.
  - `sonar_poll`: `every_s`, severity/type filters for automated Sonar alert batching.
- **`[settings.tracker]`** (Tagged by `kind`):
  - `"jira"`: `url`, `deployment` (`"cloud"` \| `"data-center"`), `user` (Cloud email; omit for Data Center PAT), `token_env`, `jql`, optional `pr_transition`.
  - `"backlog"`: `url`, `token_env`, `project_ids`, optional `status_ids`, `issue_type_ids`, `category_ids`, `keyword`, `pr_status_id`.
  - `"jsm"` (Jira Service Management): `url`, `deployment`, `user`, `token_env`, `jql`, optional `service_desk_id`, `sla_field`, `pr_transition`, `diagnosis_transition`, `internal_comments` (default `true`).
  - `"servicenow"`: `url`, optional `user`, `token_env`, `table` (default `"incident"`), `query` (`sysparm_query`), optional `pr_state`, `diagnosis_state`, `work_notes` (default `true`).
- **`[settings.apm]`** (Read-only production telemetry enrichment, tagged by `kind`):
  - `"sentry"`: `url`, `organization`, optional `project`, `token_env`.
  - `"datadog"`: `url`, `token_env` (`DD-API-KEY`), `app_key_env` (`DD-APPLICATION-KEY`), optional `service`, `env`.

### Example: Enterprise Repository Registration File

```toml
slug = "acme-orders"
url = "https://github.com/acme/orders"
default_branch = "main"
language = "java"
mode = "customer-jp"
max_concurrent_jobs = 2
tenant = "acme"

[settings.app]

[settings.issue_trust]
policy = "collaborators"

[settings.triage_lane]
categories = ["ops_or_config", "needs_info"]
min_confidence = 0.85

[[settings.sidecars]]
name = "postgres"
image = "postgres:16-alpine"
user = "70:70"
env = { POSTGRES_PASSWORD = "test", POSTGRES_DB = "orders_test" }
tmpfs = ["/var/lib/postgresql/data", "/var/run/postgresql"]
ready = ["pg_isready", "-h", "localhost", "-U", "postgres"]
test_env = { DATABASE_URL = "postgres://postgres:test@postgres:5432/orders_test" }

[[settings.secrets.allow]]
path = "src/test/resources/fixtures/*"
kinds = ["aws-access-key"]
reason = "Synthetic test fixture key used by unit tests"

[settings.tracker]
kind = "jsm"
url = "https://acme.atlassian.net"
deployment = "cloud"
user = "aegis-bot@acme.example"
token_env = "ACME_JSM_TOKEN"
jql = 'project = OPS AND labels = aegis AND status = "Waiting for support"'
internal_comments = true
```

---

## 4. Validating Configuration Before Going Live

1. **Dry-check repository readiness** before registering a repo:
   ```bash
   docker compose exec aegis aegis check-repo --config .workspace/preflight/<slug>.toml
   ```
   Verifies language profile, lockfiles, sandbox image, egress dependency fetch, sidecars, and baseline tests under `--network none`.
2. **Startup fail-fast checks:** `aegis-server` validates all `[secrets]`, `[modes]` residency rules, `[github_app]`, `[auth.oidc]`, and `[[chatops.channels]]` before accepting requests or claiming jobs.
3. **Further reading:** [Adding Repositories](runbook/repos.md) · [On-Premise Runbook](runbook/on-premise.md) · [Tenants](runbook/tenants.md) · [AWS Runbook](runbook/README.md)
