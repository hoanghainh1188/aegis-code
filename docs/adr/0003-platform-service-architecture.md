# ADR 0003 – Platform Service Architecture (Phase 2)

- Date: 2026-10-18
- Status: Accepted (2026-10-25: implemented across issues #22–#33; gate retains docker CLI, §6 refined in #184)
- Decider: @Nguyen Han Hoang Hai
- Related: [ADR 0001](0001-platform-in-rust.md), [ADR 0002](0002-full-platform-mvp-as-checkpoint.md), Proposal §7, §9

## Context

ADR 0002 established the objective of delivering a complete production platform: operating as a persistent service, connecting to enterprise ticket systems, handling multiple repositories, and offering a web dashboard with role-based access control. The initial technical design focused primarily on the PoC engine.

The platform's operational workload characteristics guide technical choices:

- Jobs run from several minutes up to tens of minutes; latency is dominated by model inference and test suite compilation.
- Throughput is dozens to hundreds of jobs daily, rather than thousands of requests per second.
- Every run incurs actual model API expenses (capped at $4). Duplicate execution or redundant retries directly burn budget.
- Repositories under `customer-jp` mode require strict data residency within Japan (Proposal §7).
- Engineering team is small and focused on Rust. Every additional infrastructure component introduces operational complexity.

## Decision

### 1. Unified Binary with Role-Based Execution
- The unified binary `aegis-server` bundles the HTTP API, background pollers (Jira/Backlog/Sonar), and job workers. The `--role api|worker|all` flag selects active responsibilities (defaults to `all`).
- Early deployments run as a single `all` process. When scaling is required, operators deploy multiple instances from the same container image with differentiated roles without code changes.
- Every job executes verification tests in an isolated sandbox container (`--network none`, zero secrets) as specified in Design §6–§7.

### 2. State Persistence: PostgreSQL + sqlx
- Crate `aegis-store` interacts with PostgreSQL via `sqlx`. Versioned migrations are embedded directly into the binary (`sqlx::migrate!`) and run automatically during startup.
- Queries are validated at compile time; the `.sqlx/` query metadata directory is committed to source control to enable offline compilation. CI validates consistency via `cargo sqlx prepare --check` and runs integration tests against live PostgreSQL.
- All listing queries enforce strict pagination and bounded limits.
- SQLite is reserved exclusively for repository Codebase-Memory indexes (Design §3.2); platform state resides in PostgreSQL.

### 3. Transactional Job Queue in PostgreSQL
- The job queue is modeled directly in the `jobs` table. Workers claim work using `SELECT ... FOR UPDATE SKIP LOCKED` and maintain a distributed lease (`locked_until`), renewed periodically during execution.
- Avoid external message brokers (Redis, NATS, SQS). Job creation, deduplication, and budget validation execute atomically in a single database transaction.
- Concurrency limits operate at two tiers: global `max_workers` per host and repository-level `max_concurrent_jobs`. The claim query automatically enforces repository concurrency limits.
- **Retries are restricted strictly to infrastructure errors** (sustained model outages, container execution failures, worker process crashes): exponential backoff up to 3 attempts. Agent stopping conditions (`stopped_budget`, `stopped_steps`, `stopped_expert_fail`) represent final outcomes and are never retried.
- Token costs accumulate across all attempts. The $4 circuit breaker applies to the overall job, not individual retry attempts.
- **Service Restarts:** When a lease expires, abandoned jobs are reclaimed by available workers. Interrupted runs are recorded as `interrupted`, and jobs restart cleanly on a fresh worktree if remaining budget permits.
- **Cancellation & Graceful Shutdown:** Utilizes `CancellationToken` (tokio-util), evaluated before each model invocation and within the gate runner; canceling terminates active containers. On receiving `SIGTERM`, workers cease claiming jobs and wait for active jobs up to `worker.drain_s`; exceeding the timeout releases the lease and records the attempt as `interrupted`.

### 4. HTTP and Webhooks: axum
- Web services utilize `axum` + `tower-http` (tracing, timeouts, request body size limits).
- REST APIs are versioned (`/api/v1/...`) with uniform envelope responses: `data`, `error`, and `meta` (pagination).
- In-memory token bucket rate limiting (#161, #173) is implemented natively in `aegis-server` (`auth/api.rs`):
  - Buckets replenish uniformly per minute. Exhausting tokens returns HTTP 429 `rate_limited` with `Retry-After`.
  - Authenticated tokens have per-user buckets (`api_requests_per_minute`, default 120), shared across client tokens.
  - Unauthenticated requests are throttled by client IP (`api_failures_per_minute`, default 20) before database lookups, defending against DB exhaustion attacks.
  - Client IP resolves from TCP peers or `X-Forwarded-For` strictly counting trusted proxies configured in `trusted_proxies`.
- Webhooks:
  - GitHub: verifies `X-Hub-Signature-256` (HMAC-SHA256) via constant-time comparison.
  - GitLab: verifies `X-Gitlab-Token` via constant-time comparison.
  - Deduplication is guaranteed via unique constraints on delivery IDs (`X-GitHub-Delivery`, `X-Gitlab-Event-UUID`).
  - Webhooks return HTTP 202 immediately upon transaction commit; job execution runs asynchronously.
- Periodic polling for Jira, Backlog, and Sonar runs under `api`/`all` roles, with schedules maintained in table `polls`. Distributed coordination uses atomic timestamp advancement (`UPDATE ... WHERE next_at <= now()`) rather than persistent advisory locks (#26).
- All external inputs (issue bodies, ticket descriptions, comments) are treated as untrusted data (Design §7).

### 5. Web Dashboard: Server-Side Rendered HTML
- The dashboard and PR review portal (#28) are rendered server-side via `askama` templates enhanced with `htmx`. No separate SPA is maintained.
- Single language, unified deployment artifact, strict Content Security Policy (self-hosted scripts, nonces, zero inline scripts).
- Lightweight, self-hosted JS charting libraries; zero CDN dependencies.
- OIDC SSO authentication, server-managed sessions stored in PostgreSQL, and CSRF token protection (#29).

### 6. Deployment Topology
- Standard container image `aegis-server` running on VM instances (EC2) with Docker; workers spawn sandbox containers using the host `docker` CLI. Kubernetes is reserved for subsequent phases ([ADR 0008](0008-gate-sandbox-on-kubernetes.md)).
  - Invokes `docker run` as a synchronous child process via `spawn_blocking` (#184). Sandbox constraints (`--network none`, `--cap-drop ALL`, `no-new-privileges`, `--pids-limit`, non-root user, read-only cache mounts) are passed as CLI argument arrays.
  - Before starting the gate, the worker validates daemon health (`docker version`). Container termination during timeouts or cancellations uses `docker kill` by container name.
- **Japan Data Sovereignty:** All assets for `customer-jp` repositories (RDS, EBS workspace, build caches, logs) reside strictly in `ap-northeast-1` (Tokyo).
- Infrastructure as Code via OpenTofu (Terraform), with isolated staging environments.
- Secrets retrieved dynamically from AWS Secrets Manager on boot and rotated at runtime (#30); zero secrets in configuration files or container images.
- Structured logging via `tracing`; Prometheus metrics exported at `:9464/metrics` (#32).
  - `/metrics` listens on an internal port (`127.0.0.1:9464`) across all roles, requiring no auth tokens. Alert rules defined in `deploy/alerts/`, dashboards in `deploy/grafana/`.
  - On AWS (#33), a dedicated monitoring EC2 runs Prometheus (EC2 service discovery), Alertmanager (SNS routing), and Grafana behind SSM port forwarding.
- Workspace: Each job receives an isolated git worktree on the VM host. Concurrent jobs on the same repo do not share `target/` directories.

### 7. Architectural Constraints on Core Engine (Phase 1)
To ensure core components remain drop-in compatible with Phase 2 service requirements:

| Constraint | Enforcement Point |
| --- | --- |
| Token ledger abstracts through traits; JSON file for MVP, PostgreSQL for Phase 2. Ledger records every model call and gate run to preserve cost tracking across restarts | #9, #22 |
| Repository configuration (`RepoConfig`) is cleanly separated from `ProviderConfig` and injected externally | #10, #11, #16, #24 |
| Issue lifecycle futures must satisfy `Send + 'static` to support `tokio::spawn`. Router `StopCheck` implementations must be `Send + Sync` | #10, #16 |
| Static dispatch: traits `ModelProvider` and `ToolExecutor` use `impl Future` in trait; runtime backend selection uses enums rather than `Box<dyn ...>` | #8, #10 |
| Issue lifecycle accepts external cancellation tokens checked prior to each model call and inside gate execution | #11, #16 |
| Job, issue, and session identifiers are excluded from cache breakpoints BP1–BP2 (Design §4.4) | #13, #16 |

## Consequences

- Introduces crates `aegis-store` (data access library) and `aegis-server` (binary hosting the API, worker, and dashboard).
- External dependencies added: `sqlx`, `axum`, `tower-http`, `askama`, `tokio-util`, `openidconnect`, all audited via `cargo deny` (#42).
- Single stateful infrastructure dependency to manage in production (PostgreSQL).
- Japan customer data is isolated in `ap-northeast-1` by architectural topology rather than runtime configuration flags.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Redis / NATS / SQS for job queue | Introduces operational overhead for moderate daily job volume; atomic job creation, deduplication, and budget validation would no longer fit in a single ACID transaction |
| Generic queue crates (`apalis`, etc.) | Custom requirements (repository concurrency limits, lease renewal, token budget integration) are tightly coupled with application schema; custom implementation is small and testable |
| SQLite for platform state | Concurrent writes from multiple worker processes lead to lock contention; lacks `SKIP LOCKED` |
| Full ORMs (SeaORM, Diesel) | Application schema is compact; `sqlx` compile-time query verification provides superior safety |
| SPA (React/Vue) for dashboard | Introduces extra language runtime, build tooling, and asset pipelines for a dashboard primarily composed of tables, forms, and metric charts |
| Kubernetes per-job Pods | High operational burden for initial phases; host Docker sandboxes provide equivalent process and network isolation. (Kubernetes path detailed in [ADR 0008](0008-gate-sandbox-on-kubernetes.md)) |
