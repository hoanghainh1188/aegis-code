<p align="center">
  <img src="assets/banner.svg" alt="Aegis Platform Banner" width="100%" />
</p>

# Aegis Platform (`aegis-code`)

> **Autonomous Code Maintenance Platform:** Agentic software maintenance powered by AI — ingests issues, localizes root causes via AST graphs, synthesizes verified patches, and opens high-quality Pull Requests for human review.

[![Rust 2024](https://img.shields.io/badge/rust-2024-orange.svg?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg?style=flat-square)](LICENSE)
[![Status: Phase 2 Platform](https://img.shields.io/badge/status-Phase%202%20Platform-00F5D4.svg?style=flat-square)]()
[![npm: @hoanghainh1188/aegis-job](https://img.shields.io/npm/v/@hoanghainh1188/aegis-job?style=flat-square&color=crimson&label=aegis-job&logo=npm)](https://www.npmjs.com/package/@hoanghainh1188/aegis-job)
[![PostgreSQL Queue](https://img.shields.io/badge/queue-PostgreSQL%20SKIP%20LOCKED-336791.svg?style=flat-square&logo=postgresql)]()
[![Sandbox: Air-Gapped Docker](https://img.shields.io/badge/sandbox-Docker%20--network%20none-2496ED.svg?style=flat-square&logo=docker)]()

<p align="center">
  <a href="#-1-context--objectives">Context & KPIs</a> •
  <a href="#-2-how-it-works-end-to-end-workflow">How It Works</a> •
  <a href="#-3-quick-start--installation">Quick Start</a> •
  <a href="#-4-system-architecture">System Architecture</a> •
  <a href="#-5-deployment--operations">Deployment</a> •
  <a href="#-7-repository-structure">Crate Topology</a> •
  <a href="#-9-references--documentation">Docs & ADRs</a>
</p>

---

## 🎯 1. Context & Objectives

### The Problem
In enterprise software engineering, code maintenance (resolving production bugs, addressing CI test breakages, remediating SonarQube quality alerts) consumes thousands of engineering hours annually (~2–3 FTEs per organization). This manual, repetitive loop causes context fatigue, introduces unintended regressions, and delays core feature delivery.

### Vision & The Agentic SDLC
Aegis transforms manual maintenance into an **Agentic Software Development Life Cycle (SDLC)**:
* **Automates 80% of Repetitive Tasks:** Ingests tickets $\rightarrow$ locates root causes via AST relational graphs $\rightarrow$ synthesizes failing reproduction test cases $\rightarrow$ implements patches $\rightarrow$ rigorously validates against a 6-step isolated sandbox gate $\rightarrow$ publishes comprehensive Pull Requests.
* **Strict "Human-in-the-Loop" Principle:** Aegis **never merges autonomously**. Final approval and merge decisions always remain with human engineers. Review time drops from **3–5 hours to 5–10 minutes** per issue.

### Core KPIs: Targets and Measured Results
Thresholds are set in [ADR 0005](docs/adr/0005-kpi-thresholds-after-phase-1.md). "Measured" is the Phase 1 evaluation: 32 real, already-fixed issues from two open-source projects, scrapy (Python, 14) and typst (Rust, 18), with hidden tests ([eval/README.md](eval/README.md), [report](docs/measurements/2026-10-24-eval-kpi.md)).

| Metric | Target | Measured (Phase 1) | Description |
| :--- | :---: | :---: | :--- |
| **Acceptance Rate** | $\ge 40\%$ total, $\ge 35\%$ per language | **50%** (Python 64%, Rust 39%) | PRs a reviewer accepts with at most 10 lines edited. |
| **Zero Regression** | 0 | **0** | PRs that break an existing test and still reach a reviewer. |
| **Cost per Issue** | $\le \$4$ average | **$0.55** | Model spend per issue, failed runs included. A job is stopped at $4 or 40 model calls. |
| **Review Time** | $\le 15$ min | **1.8 min** | Human time to judge one PR. |
| **Prompt Cache Reads** | $\ge 70\%$ | **92%** | Share of input tokens served from the prompt cache. |

Caveats: both projects are public, so models may have seen their fixes during training; Phase 1 reviewers could see hidden test results, which shortens review time. Both are re-measured on internal repositories during the pilot (#158). The four Phase 2 profiles (TypeScript, Java, Go, C#) have passed a dry run (19/20 issues, [report](docs/measurements/2026-10-29-new-profiles-dry-run.md)); their KPI run is #259.

---

## 🔄 2. How It Works (End-to-End Workflow)

```mermaid
sequenceDiagram
    autonumber
    actor Dev as Developer / Reviewer
    participant Forge as GitHub / GitLab / Jira
    participant Server as aegis-server (Queue & API)
    participant Agent as aegis-worker (Agent Loop)
    participant Sandbox as Docker Sandbox (--network none)

    Dev->>Forge: Add label 'aegis' or fail CI on main
    Forge->>Server: Ingest via Webhook / Poll (HMAC verified)
    Server->>Server: Enqueue job (PostgreSQL SKIP LOCKED)
    Server->>Agent: Worker leases job & creates Git worktree
    Agent->>Agent: AST context search & synthesize repro test (FAIL)
    loop Edit & Verify Loop (Circuit breaker capped at $4 / 40 calls)
        Agent->>Agent: Synthesize patch with syntax-guarded edit
        Agent->>Sandbox: Execute 6-Step Verification Gate
        Sandbox-->>Agent: Gate Results (Format, Lint, Tests, Sonar)
    end
    Agent->>Forge: Open Pull Request (RCA, Diff, Proof, Token Ledger)
    Dev->>Forge: Review: "Request changes" or comment '/aegis fix'
    Forge->>Server: Trigger automated review round on same branch
```

1. **Ingest:** Triggered automatically via GitHub/GitLab webhook (labels like `aegis`), polled tickets (Jira, Backlog), failed CI runs on `main`, or manually via `aegis-job submit`.
2. **Reproduce & Isolate:** The agent navigates the codebase using bounded AST tools, generates a minimal reproducing test, and confirms that it **FAILS** before making changes.
3. **Patch & Verify:** The agent edits code with syntax-safe linters and executes the 6-step gate inside an isolated Docker sandbox container (`--network none`).
4. **Human Review & Review Rounds:** The agent opens a PR detailing the root cause, verification evidence, and cost breakdown. If a reviewer requests changes or comments `/aegis fix`, the agent triggers a review round on the same branch without force-pushing.

### What You Get

Screenshots from a [local trial](docs/local-trial.md) run with a free local model (Ollama `gpt-oss:20b`), fixing a Go function that cut multi-byte strings in the middle of a character. The PR body and the dashboard are currently written in Vietnamese; the annotations below give the English.

**The pull request** — what a reviewer reads. *Issue*, *Nguyên nhân* (root cause), *Thay đổi* (changes, per hunk), *Test* (the reproducing test, FAIL before the patch and PASS after; *Test được phép đổi*: existing tests the patch was allowed to change, here none), *Kết quả gate* (every gate step), *Chi phí* (model, calls, USD). The diff holds the new test and the fix:

<p align="center"><img src="assets/screenshots/trial-pull-request.png" alt="Pull request opened by Aegis: root cause, changes, reproducing test, gate result, cost, and diff" width="85%" /></p>

**The job page** on the dashboard — every model call, tool call, escalation and gate run, with its cost. Here the Core model's edits were rejected by the syntax check twice, so the job moved up to the Expert tier, wrote the reproducing test, fixed the function, and passed the gate in 3.8 minutes:

<p align="center"><img src="assets/screenshots/trial-job-page.png" alt="Dashboard job page: outcome PrOpened, model tiers, 19 model calls, cost, and the ledger of every call" width="85%" /></p>

The reviewer's side of this, step by step, is in the [Reviewer Guide](docs/reviewing.md).

### Glossary

| Term | Meaning |
| :--- | :--- |
| **Job** | One attempt to resolve one issue (or one CI failure, or one review round). Visible on the dashboard and through `aegis-job`. |
| **Gate** | The verification run in a network-isolated Docker sandbox: format, lint, reproducing test (FAIL → PASS), full test suite, coverage, Sonar, integrity. A PR is opened only when it passes. |
| **Reproducing test** | A test the agent writes that fails on the current code and passes after the fix — the proof that the bug is real and fixed. |
| **Language profile** | Per-language settings: sandbox image, build/test/lint commands, test conventions (`profiles/*.toml`, [ADR 0004](docs/adr/0004-multi-language-profiles.md)). |
| **Mode** | A named server configuration a repo runs under: which models, provider and region (`poc` = Claude API, `customer-jp` = Bedrock in Tokyo, or your own local-model file). |
| **Fork** | The repository the agent pushes `agent/*` branches to and opens PRs in. Either a bot-owned fork of the upstream repo, or the upstream repo itself. |
| **Light / Core / Expert** | The three model tiers: triage, main solving, and escalation for hard cases. |
| **Circuit breaker** | The per-job ceiling ($4 or 40 model calls). The job stops and reports `stopped_budget` / `stopped_steps` instead of opening a PR. |
| **Review round** | A follow-up job on an open PR, started by *Request changes* or a `/aegis fix` comment; it pushes a new commit to the same branch. |
| **Ledger** | The per-job record of every model call, its tokens and its cost. |

---

## 🚀 3. Quick Start & Installation

Choose the path that fits your role.

> [!IMPORTANT]
> **No release has been published yet.** There is no `vX.Y.Z` tag, so the npm package `@hoanghainh1188/aegis-job`, the GHCR images and the offline bundles do not exist yet; they appear with the first release ([docs/release.md](docs/release.md)). Until then:
> * **Developers** install `aegis-job` from source (the `cargo install` line below).
> * **Operators** build the images locally and deploy them with `--offline` ([On-Premise Runbook, "Before the First Release"](docs/runbook/on-premise.md#before-the-first-release-local-images)).
> * The **local trial** and **building from source** work as written.

### 3.1 For Developers (Submit & Follow Jobs)
Interact with an Aegis instance your team already runs, from your terminal, using `aegis-job` ([ADR 0006](docs/adr/0006-developer-client.md), [docs/client.md](docs/client.md)). You need its URL and a role on your repo from its administrator; to try it without one, use the local trial (3.2). Most jobs do not need the CLI at all: adding the `aegis` label to an issue starts one.

```bash
# Install from source via Cargo (until the first release)
cargo install --locked --git https://github.com/hoanghainh1188/aegis-code aegis-client

# After the first release: install via npm (macOS, Linux - prebuilt binaries bundled)
npm install -g @hoanghainh1188/aegis-job

# Log in with your personal token (from https://<aegis-server>/me/tokens)
aegis-job login https://aegis.internal.example.com

# Submit an issue and watch progress live
aegis-job submit my-repo owner/repo#42
aegis-job watch <job-id>
```

### 3.2 For Quick Trial (30-Minute Local Sandbox)
Run the entire platform on a single machine with zero external cloud dependencies ([docs/local-trial.md](docs/local-trial.md)). This spins up PostgreSQL, a mock GitHub service, mock OIDC, and a sample Go repository with a known bug:

```bash
# Clone the repository
git clone https://github.com/hoanghainh1188/aegis-code.git
cd aegis-code

# Start with local Ollama (100% free, requires Ollama running)
./scripts/trial/up.sh ollama

# Or start with Claude API (faster, requires ANTHROPIC_API_KEY)
export ANTHROPIC_API_KEY="sk-ant-..."
./scripts/trial/up.sh claude

# Tear down when finished
./scripts/trial/down.sh
```

### 3.3 For Operators & DevOps (Self-Hosted & Production)
* **On-Premise (Single Linux Host with Docker):** Set up in minutes via Docker Compose or systemd ([docs/runbook/on-premise.md](docs/runbook/on-premise.md), [ADR 0007](docs/adr/0007-on-premise-deployment.md)):
  ```bash
  cd deploy/compose
  sudo ./install.sh                     # Prepares /srv/aegis/.workspace and ./secrets
  # Configure ./server.toml and secrets in ./secrets/
  sudo ./aegis-deploy.sh 0.1.0          # Release version without "v" (images are tagged 0.1.0)
  ```
  Before starting, collect what the [Preparation Checklist](docs/runbook/on-premise.md#preparation-checklist) lists (SSO client, bot account, tokens, domain with TLS, model API key).
* **Cloud Infrastructure (AWS):** Production-grade deployment with OpenTofu (EC2, RDS PostgreSQL, Secrets Manager, ALB with TLS, CloudWatch) in [deploy/tofu/](deploy/tofu/) and [docs/runbook/README.md](docs/runbook/README.md).
* **Kubernetes:** Run worker pods with Docker-in-Docker sidecars for safe sandbox isolation ([ADR 0008](docs/adr/0008-gate-sandbox-on-kubernetes.md)).

### 3.4 For Contributors (Build & Test from Source)
* **Prerequisites:** Rust stable (edition 2024, 1.85+) and Docker.
```bash
# Build core CLI and tool daemon
cargo build --release -p aegis-cli -p aegis-aci

# Run formatting and clippy linter checks
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p aegis-provider --all-targets --features bedrock -- -D warnings

# Execute test suite
cargo test --workspace
cargo test -p aegis-provider --features bedrock
```

---

## 🏛️ 4. System Architecture

The platform is engineered 100% in **Rust** as a Cargo Workspace ([ADR 0001](docs/adr/0001-platform-in-rust.md), [ADR 0003](docs/adr/0003-platform-service-architecture.md)), ensuring native speed, minimal memory overhead, and compile-time safety. To provide a complete architectural picture, this section breaks down Aegis from three perspectives:
1. **System Context & External Integrations** — how Aegis interacts with developers, external DevOps/ticketing platforms, AI providers, and host infrastructure.
2. **Internal Component & Crate Topology** — how the 16 internal Rust crates and subsystems are structured and wired together inside Aegis.
3. **4-Layer Agent Control Pipeline** — how an individual issue is processed, cached, routed, and verified inside the agent execution loop.

---

### 4.1 System Context & External Integrations (Aegis $\leftrightarrow$ External Ecosystem)

Aegis sits between your engineering workflow tools (Git forges, issue trackers, static analyzers) and LLM providers, operating within a strictly controlled host boundary:

<p align="center">
  <img src="assets/architecture-context.svg" alt="Aegis System Context & External Integrations" width="100%" />
</p>

<details>
<summary><b>🔍 View Interactive Mermaid Diagram (System Context)</b></summary>

```mermaid
flowchart LR
    classDef actor fill:#0c2d48,stroke:#38BDF8,stroke-width:1.5px,color:#F8FAFC;
    classDef external fill:#111927,stroke:#60A5FA,stroke-width:1.5px,color:#E2E8F0;
    classDef aegis fill:#042f2e,stroke:#00F5D4,stroke-width:2px,color:#F8FAFC;
    classDef sandbox fill:#451a03,stroke:#F59E0B,stroke-width:1.5px,color:#FEF3C7;
    classDef llm fill:#2e1065,stroke:#A855F7,stroke-width:1.5px,color:#F3E8FF;
    classDef infra fill:#064e3b,stroke:#10B981,stroke-width:1.5px,color:#ECFDF5;

    subgraph Actors["👥 Human Actors"]
        Dev["Developer / Reviewer"]:::actor
        Op["Platform Operator"]:::actor
    end

    subgraph ExternalDevOps["🔗 External DevOps & Issue Systems"]
        Forge["Git Forges<br/>(GitHub / GitLab)"]:::external
        Tracker["Issue Trackers<br/>(Jira / Backlog)"]:::external
        Sonar["Code Quality<br/>(SonarQube Server / Cloud)"]:::external
    end

    subgraph AegisBoundary["🛡️ Aegis Platform Boundary"]
        CLI["aegis-job CLI<br/>(Developer Client)"]:::aegis
        Server["aegis-server<br/>(API · Webhooks · Pollers · Dashboard · Workers)"]:::aegis
        Sandbox["Isolated Docker Sandboxes<br/>(--network none · non-root · cap-drop ALL)"]:::sandbox
        Proxy["Allow-listed Egress Proxy<br/>(Pre-gate dependency fetch)"]:::sandbox
    end

    subgraph AIProviders["🧠 AI / LLM Providers"]
        Claude["Anthropic Claude API<br/>(PoC / Direct Mode)"]:::llm
        Bedrock["AWS Bedrock ap-northeast-1<br/>(Japan Residency: jp.*)"]:::llm
        Ollama["Local Ollama<br/>(Offline / Local Trial)"]:::llm
    end

    subgraph Infra["🗄️ State, Security & Observability"]
        PG[("PostgreSQL<br/>(State & SKIP LOCKED Queue)")]:::infra
        OIDC["OIDC Provider<br/>(SSO Authentication)"]:::infra
        Secrets["Secrets Store<br/>(AWS Secrets Manager / Files)"]:::infra
        Prom["Prometheus & Grafana<br/>(:9464/metrics & Alerts)"]:::infra
    end

    Dev -->|"Browser (SSR Dashboard)"| Server
    Dev -->|"Terminal (REST API)"| CLI
    CLI -->|"Submit / Watch / Cancel"| Server
    Dev -->|"Label 'aegis' / Review PR / '/aegis fix'"| Forge

    Forge -->|"HMAC Webhooks (Issues, CI fails, Reviews)"| Server
    Server -->|"Clone worktree, push branch 'agent/*', open PR"| Forge
    Server <-->|"Poll JQL/Filters & write back outcomes"| Tracker
    Server <-->|"Fetch alerts & run Sonar Quality Gate"| Sonar

    Server -->|"HTTPS / AWS SDK (Prompt Caching BP1–BP4)"| Claude
    Server -->|"HTTPS / AWS SDK (ModelPolicy::JapanOnly)"| Bedrock
    Server -->|"Local HTTP"| Ollama

    Server <-->|"ACID transactions, leases, token ledger"| PG
    Server <-->|"OAuth2 / OIDC login flow"| OIDC
    Server <-- "Boot load & live rotation" --> Secrets
    Prom -->|"Scrape :9464/metrics"| Server
    Op -->|"Monitor & manage"| Prom

    Server -->|"spawn_blocking (docker run)"| Sandbox
    Sandbox -.->|"Pre-gate package resolution only"| Proxy
```

</details>

| External System | Direction | Protocol / Mechanism | Purpose & Security Boundary | Handled By |
| :--- | :---: | :--- | :--- | :--- |
| **Developers & Reviewers** | $\leftrightarrow$ | HTTPS (Browser / `htmx`), REST API (`/api/v1/*`) | View job ledgers on the dashboard, submit/watch jobs via `aegis-job`, or review PRs on GitHub/GitLab. | `aegis-server`, `aegis-client` |
| **GitHub / GitLab** | $\leftrightarrow$ | Webhooks (`X-Hub-Signature-256` / `X-Gitlab-Token`), Git / HTTPS API | Ingests labeled issues, `main` CI breakages, and PR review comments; pushes `agent/<id>` branches and opens structured PRs. | `aegis-server`, `aegis-pr` |
| **Jira / Backlog** | $\leftrightarrow$ | REST API Polling (atomic `polls` schedule) | Polls issues matching configured JQL/filters and writes resolution status/PR links back to the ticket. | `aegis-tracker`, `aegis-server` |
| **SonarQube** | $\leftrightarrow$ | Web API + `sonar-scanner` CLI | Ingests static analysis warnings (clustered by AST/dataflow) and verifies patches against the Sonar Quality Gate. | `aegis-sonar-batch`, `aegis-gate` |
| **Claude / Bedrock / Ollama** | $\rightarrow$ | HTTPS / AWS SDK (`aws-sdk-bedrockruntime`) | Executes model inference across Light/Core/Expert tiers. Enforces `jp.` inference profiles in `ap-northeast-1` for `customer-jp`. | `aegis-provider`, `aegis-router` |
| **PostgreSQL** | $\leftrightarrow$ | TCP / TLS (`sqlx` compile-time checked) | Stores repositories, transactional job queue (`FOR UPDATE SKIP LOCKED`), worker leases, user sessions, and token cost ledgers. | `aegis-store`, `aegis-ledger` |
| **Docker Daemon & Host** | $\rightarrow$ | Host CLI (`docker run` / `docker kill`) | Runs per-job Git worktrees on host and spawns air-gapped verification containers (`--network none`, `--cap-drop ALL`, uid 10001). | `aegis-gate`, `aegis-lifecycle` |
| **OIDC / Secrets / Metrics** | $\leftrightarrow$ | OIDC, AWS Secrets Manager API, Prometheus | Authenticates dashboard users via SSO, rotates credentials without restart, and exports operational/financial metrics on `:9464/metrics`. | `aegis-server`, `aegis-secrets` |

---

### 4.2 Internal Architecture & Component Topology (Inside Aegis)

Internally, Aegis is structured into **five modular subsystems** across 16 Rust crates. The unified binary `aegis-server` can run all subsystems in a single process (`--role all`) or split horizontally into stateless API nodes (`--role api`) and compute workers (`--role worker`):

<p align="center">
  <img src="assets/architecture-internal.svg" alt="Aegis Internal Component & Crate Topology" width="100%" />
</p>

<details>
<summary><b>🔍 View Interactive Mermaid Diagram (Internal Crate Topology)</b></summary>

```mermaid
flowchart TB
    classDef entry fill:#0c2d48,stroke:#38BDF8,stroke-width:1.5px,color:#F8FAFC;
    classDef service fill:#042f2e,stroke:#00F5D4,stroke-width:1.8px,color:#F8FAFC;
    classDef store fill:#064e3b,stroke:#10B981,stroke-width:1.5px,color:#ECFDF5;
    classDef engine fill:#2e1065,stroke:#A855F7,stroke-width:1.5px,color:#F3E8FF;
    classDef tools fill:#451a03,stroke:#F59E0B,stroke-width:1.5px,color:#FEF3C7;

    subgraph Entrypoints["1. Entrypoints & Clients"]
        Client["aegis-client<br/>(aegis-job CLI)"]:::entry
        LocalCLI["aegis-cli<br/>(aegis local & eval runner)"]:::entry
    end

    subgraph ServiceLayer["2. Platform Service Layer (aegis-server)"]
        API["HTTP API & Webhook Ingress<br/>(Axum · HMAC · Rate Limiter)"]:::service
        UI["SSR Web Dashboard<br/>(Askama + htmx · OIDC · CSRF)"]:::service
        Poller["Background Pollers<br/>(Jira · Backlog · Sonar schedules)"]:::service
        WorkerPool["Worker Pool & Lease Manager<br/>(Concurrency limits · CancellationToken)"]:::service
    end

    subgraph Persistence["3. State, Secrets & Accounting"]
        Store["aegis-store<br/>(PostgreSQL Queue · Leases · Repos · Audit)"]:::store
        Secrets["aegis-secrets<br/>(AWS Secrets Manager · File secrets · Rotation)"]:::store
        Ledger["aegis-ledger<br/>(Per-call Token Accounting & USD Cost)"]:::store
    end

    subgraph CoreEngine["4. Issue Lifecycle & Agent Engine"]
        Lifecycle["aegis-lifecycle<br/>(End-to-End Job Coordinator)"]:::engine
        Orch["aegis-orchestrator<br/>(Agent Turn Loop · History Collapsing)"]:::engine
        Router["aegis-router<br/>(Light / Core / Expert · $4 & 40-call Breaker)"]:::engine
        Provider["aegis-provider<br/>(Claude / Bedrock / Ollama · BP1–BP4 Cache)"]:::engine
    end

    subgraph CodeAndDelivery["5. Code Intelligence, Verification & Delivery"]
        ACI["aegis-aci<br/>(MCP Server: open, search, syntax-safe edit)"]:::tools
        MemMCP["codebase-memory-mcp<br/>(SQLite Call Graph: CALLS, IMPORTS, TESTS)"]:::tools
        Lang["aegis-lang<br/>(Tree-sitter AST · LSP · 6 Language Profiles)"]:::tools
        Gate["aegis-gate<br/>(6-Step Docker Sandbox Runner)"]:::tools
        PR["aegis-pr<br/>(Git Branching · PR Template · Golden Master)"]:::tools
        SonarBatch["aegis-sonar-batch<br/>(AST / Dataflow Alert Clustering)"]:::tools
        Tracker["aegis-tracker<br/>(Jira & Backlog REST Client)"]:::tools
    end

    Client -->|"REST /api/v1"| API
    API --> Store
    UI --> Store
    Poller --> Tracker
    Poller --> SonarBatch
    Poller --> Store
    WorkerPool <-->|"SELECT ... FOR UPDATE SKIP LOCKED"| Store
    WorkerPool --> Lifecycle
    LocalCLI --> Lifecycle

    Lifecycle --> Secrets
    Lifecycle --> Orch
    Lifecycle --> Router
    Lifecycle --> Gate
    Lifecycle --> PR
    Lifecycle --> Ledger
    Lifecycle --> Tracker
    Lifecycle --> SonarBatch
    Store --> Ledger

    Orch --> Router
    Router --> Provider
    Provider --> Secrets
    Orch <-->|"MCP over stdio (rmcp)"| ACI
    Orch <-->|"MCP over stdio (rmcp)"| MemMCP

    ACI --> Lang
    ACI --> Gate
    Gate --> Lang
    SonarBatch --> Lang
    SonarBatch --> Gate
    PR --> Gate
    PR --> Provider
```

</details>

#### How Internal Components Work Together
1. **Service & Scheduling (`aegis-server`, `aegis-store`, `aegis-secrets`):**
   * **Ingress & UI:** `aegis-server` exposes the REST API, HMAC-verified webhook endpoints, and an **Askama + htmx** server-rendered dashboard secured by OIDC SSO and in-memory token-bucket rate limiting.
   * **Transactional PostgreSQL Queue:** Instead of external brokers (Redis/SQS), `aegis-store` manages the job queue directly in PostgreSQL using `SELECT ... FOR UPDATE SKIP LOCKED` with distributed worker leases (`locked_until`), per-repo concurrency caps, and graceful `SIGTERM` draining.
2. **Lifecycle Coordination (`aegis-lifecycle`):**
   * Serves as the central facade invoked by both `aegis-server` workers (in production) and `aegis-cli` (for local single-issue runs and evaluation benchmarks).
   * Prepares an isolated Git worktree per job, builds the `codebase-memory-mcp` SQLite call graph index, and orchestrates the transition from triage $\rightarrow$ agent loop $\rightarrow$ verification gate $\rightarrow$ PR creation.
3. **Agent Execution Loop (`aegis-orchestrator`, `aegis-router`, `aegis-provider`, `aegis-ledger`):**
   * `aegis-orchestrator` manages multi-turn agent sessions, dispatches tool calls over MCP (`rmcp` child processes), and compacts older tool observations once context usage exceeds $60\%$.
   * `aegis-router` selects the active tier (`Light` $\rightarrow$ `Core` $\rightarrow$ `Expert`) based on failure signals and enforces the economic circuit breaker ($\$4$ or 40 model calls) backed by real-time accounting in `aegis-ledger`.
   * `aegis-provider` serializes requests with deterministic 4-breakpoint prompt caching (`BP1`–`BP4`) and enforces regional routing policies (`ModelPolicy::JapanOnly` for `customer-jp.toml`).
4. **Code Intelligence & Sandboxed Verification (`aegis-aci`, `aegis-lang`, `aegis-gate`):**
   * `aegis-aci` runs as an MCP tool server providing bounded code windows (`open`, `search_symbol`, `find_definition` via LSP) and syntax-guarded edits (`edit`).
   * `aegis-lang` supplies ecosystem-specific rules (`profiles/*.toml`), Tree-sitter AST grammars, lockfile parsers, and test runners for **Rust, Python, TypeScript/JavaScript, Java, Go, and C#**.
   * `aegis-gate` executes the 6-step verification suite inside a network-isolated Docker container (`--network none`), including automatic re-runs to filter out flaky tests.
5. **External Delivery & Clustering (`aegis-pr`, `aegis-tracker`, `aegis-sonar-batch`):**
   * `aegis-pr` pushes `agent/<issue-id>` branches, formats the bilingual/structured PR description (RCA, diff rationale, gate proof, Golden Master declarations, token ledger), and handles follow-up review rounds on the same branch.
   * `aegis-sonar-batch` groups related SonarQube warnings by enclosing AST node or tainted dataflow trace and merges verified fixes into an integration branch.
   * `aegis-tracker` synchronizes ticket context and final PR status with Jira and Backlog.

---

### 4.3 4-Layer Agent Control Pipeline

Inside each job execution, the agent engine enforces a strict **4-Layer Control Pipeline** to maximize fix accuracy while bounding cost and preventing regressions:

<p align="center">
  <img src="assets/architecture-pipeline.svg" alt="Aegis 4-Layer Agent Control Pipeline" width="100%" />
</p>

<details>
<summary><b>🔍 View Interactive Mermaid Flowchart (4-Layer Pipeline)</b></summary>

```mermaid
flowchart TD
    classDef input fill:#0c2d48,stroke:#38BDF8,stroke-width:1.5px,color:#F8FAFC;
    classDef layer fill:#111a29,stroke:#00F5D4,stroke-width:1.8px,color:#F8FAFC;
    classDef pass fill:#064e3b,stroke:#10B981,stroke-width:2px,color:#ECFDF5;
    classDef stop fill:#450a0a,stroke:#EF4444,stroke-width:2px,color:#FEF2F2;

    Issue["Input: Webhook / Poller / aegis-job submit"]:::input --> L1["LAYER 1: CONTEXT ENGINE (Tree-sitter AST + Bounded ACI)"]:::layer
    L1 --> L2["LAYER 2: PROMPT CACHING (4 Breakpoints BP1 → BP4)"]:::layer
    L2 --> L3["LAYER 3: MODEL ROUTING (Light → Core → Expert)"]:::layer
    L3 --> L4["LAYER 4: REGRESSION VERIFICATION GATE (Docker Sandbox --network none)"]:::layer
    
    L4 -->|Passes 6-Step Gate| PR["Open Pull Request (RCA + Golden Master + Token Ledger)"]:::pass
    L4 -->|Gate Fail < 2 times| L3
    L4 -->|Circuit Breaker Hit: $4 limit / 40 calls / Expert 2x fail| Stop["CIRCUIT BREAKER: Stop & Hand off to Engineer"]:::stop
```

</details>

1. **Layer 1 – Context Engine (`aegis-aci`, `aegis-lang`, `codebase-memory-mcp`):**
   * Tree-sitter AST queries per language profile. Builds in-memory module maps and relational call graphs (`CALLS`, `IMPORTS`, `TESTS`) in SQLite via Codebase-Memory MCP and language servers (LSP).
   * **Bounded ACI Tools:** Reads code in capped windows (100 lines at a time, SWE-agent standard) and bounds search output to 50 results. The `edit` tool validates syntax via Tree-sitter and profile linters prior to persisting changes.
   * **Batch History Collapse:** Once context usage reaches $60\%$ of the model window, older tool observations collapse into `[Observation collapsed]` tokens, preserving reasoning while retaining cache efficiency.
2. **Layer 2 – Deterministic Prompt Caching (`aegis-provider`):**
   * Strict 4-breakpoint hierarchy: `BP1` (Tool schemas) $\rightarrow$ `BP2` (System prompt, conventions) $\rightarrow$ `BP3` (Issue context, repo metadata, repro test) $\rightarrow$ `BP4` (Dynamic message history).
   * Byte-stable serialization (sorted keys, omitted dynamic timestamps) achieves **$\ge 70\%$ cache hit rates**, cutting input token costs by up to 90–95%.
3. **Layer 3 – Model Hierarchy & Circuit Breakers (`aegis-router`, `aegis-ledger`):**
   * **Light (`Haiku 5.5`):** Issue classification, stack trace pruning, and log summarization.
   * **Core (`Sonnet 5.5`):** Root-cause localization, reproducing test creation, patch synthesis.
   * **Expert (`Opus 5.5`):** Complex architecture refactoring spanning $\ge 3$ files or escalated after consecutive Core failures.
   * **Economic Circuit Breaker:** Stops execution immediately if cost exceeds $\$4$ or reaches 40 model calls (excluding Light preprocessing).
4. **Layer 4 – 6-Step Verification Gate in Docker (`aegis-gate`):**
   * Format $\rightarrow$ Lint $\rightarrow$ Repro Test (proves FAIL $\rightarrow$ PASS) $\rightarrow$ Full Test Suite $\rightarrow$ Coverage $\rightarrow$ Sonar Quality Gate.
   * Runs in network-isolated containers (`--network none`, `--cap-drop ALL`, non-root user).
   * **Golden Master Technique:** For legacy code without test coverage, the agent writes characterization tests capturing existing behavior prior to patching; any modified output must be declared under *"Permitted Behavior Changes"* in the PR description.

---

## 🛠️ 5. Deployment & Operations

### Deployment Flavors
| Deployment Target | Orchestration | State Store | Secrets Source | Best For | Runbook |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **AWS (Production)** | EC2 + systemd | RDS PostgreSQL | AWS Secrets Manager | Cloud enterprise, High availability | [AWS Runbook](docs/runbook/README.md) |
| **On-Premise Compose** | Docker Compose | Local PostgreSQL | File-based (`./secrets/`) | Internal data centers, Private clouds | [On-Premise Runbook](docs/runbook/on-premise.md) |
| **On-Premise systemd** | Host systemd | Dedicated Postgres | File-based (`/etc/aegis/secrets/`) | Bare-metal Linux servers | [ADR 0007](docs/adr/0007-on-premise-deployment.md) |
| **Kubernetes** | Worker Pods | External Postgres | K8s Secrets / CSI | Cloud-native multi-tenant clusters | [ADR 0008](docs/adr/0008-gate-sandbox-on-kubernetes.md) |

### Monitoring & Observability
* **Prometheus Metrics:** Exported at `:9464/metrics` (or `:8080/metrics`). Tracks job durations, gate pass rates, token consumption, and model spending.
* **Alerting:** Pre-configured Alertmanager rules for job queue stalling, consecutive gate failures, and budget threshold breaches (`deploy/alerts/`).
* **Grafana Dashboards:** Ready-to-import dashboards for operational performance and financial accounting (`deploy/grafana/`).

### Backups & Rollbacks
* **Database Backup:** Run `sudo ./deploy/compose/backup.sh` for atomic `pg_dump` snapshots.
* **Instant Rollbacks:** Re-run `./aegis-deploy.sh <previous-version>` (e.g. `0.1.0`, no `v`). Aegis utilizes expand/contract database migrations ensuring backwards compatibility across releases.

---

## ⚡ 6. Core Capabilities

* 📥 **Multi-Channel Ingestion:** Webhooks for GitHub/GitLab (e.g. `aegis` label), polling for Jira/Backlog, automatic detection of broken CI runs on `main`, and SonarQube alerts.
* 🔁 **Review Rounds:** When an engineer clicks *"Request changes"* or comments `/aegis fix`, Aegis returns to the same branch, runs the gate, pushes an additional commit without force-pushing, and replies to review comments.
* 🔍 **Automated Triage:** Light models prune stack traces and pinpoint likely candidate crates/modules before heavy synthesis begins.
* 🧪 **Isolated Reproducing Tests:** Synthesizes dedicated test cases to verify the bug condition before proposing code changes.
* 🛠️ **Syntax-Guarded Patching:** Rejects AST-invalid code edits immediately without wasting full sandbox test cycles.
* 📦 **Sonar Batching:** Clusters related static analysis warnings by AST node or tainted dataflow, resolving them concurrently.
* 🛡️ **Zero Regression Enforcement:** Guarantees 100% test suite passage and enforces coverage thresholds ($\ge 80\%$) prior to submission.
* 💰 **Auditable Cost Ledger:** `aegis-ledger` tracks token expenditures per call and terminates tasks before budgets are exceeded.

---

## 📂 7. Repository Structure

The Cargo workspace (`crates/`) is organized into 5 architectural layers matching the component topology in [Section 4.2](#42-internal-architecture--component-topology-inside-aegis), alongside operational and evaluation assets:

```
aegis-code/
├── crates/
│   ├── # 1. Entrypoints & Developer Clients
│   ├── aegis-client/         # Developer CLI binary `aegis-job`: submit, watch, cancel jobs via REST API (ADR 0006)
│   ├── aegis-cli/            # Local CLI binary `aegis`: single-issue runner and evaluation benchmark harness
│   │
│   ├── # 2. Platform Service, State & Secrets
│   ├── aegis-server/         # Unified service (`--role api|worker|all`): HTTP API, webhooks, pollers, workers, SSR UI (ADR 0003)
│   ├── aegis-store/          # PostgreSQL persistence (`sqlx`): transactional job queue (`SKIP LOCKED`), leases, repos, audit logs
│   ├── aegis-secrets/        # Secret loading & live rotation: AWS Secrets Manager, on-prem files, or environment variables
│   ├── aegis-ledger/         # Financial accounting: per-call token ledger, prompt cache metrics, and USD cost calculation
│   │
│   ├── # 3. Issue Lifecycle & Core Agent Engine
│   ├── aegis-lifecycle/      # End-to-end job coordinator: worktree setup, triage, agent loop, gate, escalation, PR handoff
│   ├── aegis-orchestrator/   # Multi-turn agent loop, MCP tool dispatch (`rmcp`), and batch observation collapsing
│   ├── aegis-router/         # Model tier selection (Light / Core / Expert), escalation matrix, and $4 / 40-call circuit breaker
│   ├── aegis-provider/       # LLM backends (Claude API, AWS Bedrock `jp.*`, Ollama) and 4-breakpoint prompt caching (BP1–BP4)
│   │
│   ├── # 4. Code Intelligence & Sandboxed Verification
│   ├── aegis-aci/            # MCP tool server (`rmcp` stdio): bounded `open`, `search_symbol`, LSP navigation, syntax-safe `edit`
│   ├── aegis-lang/           # Language profiles (Rust, Python, TS/JS, Java, Go, C#): Tree-sitter AST, linters, lockfiles (ADR 0004)
│   ├── aegis-gate/           # 6-step Docker sandbox runner (`--network none`): fmt, lint, repro test, suite, coverage, Sonar
│   │
│   └── # 5. External Integrations & PR Delivery
│       ├── aegis-pr/             # Git branch management (`agent/<id>`), structured PR synthesis, Golden Master & review rounds
│       ├── aegis-tracker/        # Issue tracker connectors: Jira and Backlog polling (JQL/filters) and status write-back
│       └── aegis-sonar-batch/    # SonarQube integration: AST/dataflow alert clustering and integration branch merging
│
├── config/                   # Server mode configurations (`poc.toml`, `customer-jp.toml`, `local.toml`)
├── profiles/                 # Per-language profile definitions (`rust.toml`, `python.toml`, `go.toml`, etc.)
├── policies/                 # Security and dependency allow-list policies
├── prompts/                  # System prompt templates and role instructions
├── deploy/                   # Production deployment IaC & configs (compose, systemd, OpenTofu, Grafana, Alertmanager)
├── docker/                   # Dockerfiles for `aegis-server` and per-language verification sandboxes
├── docs/                     # Architectural Decision Records (ADRs), design specs, measurements, and operator runbooks
├── eval/                     # Empirical benchmark datasets and harnesses across all 6 supported languages
├── npm/                      # Pre-bundled npm package wrapper for `@hoanghainh1188/aegis-job`
├── scripts/                  # Operational scripts (30-minute local trial, release bundling, benchmarks)
└── tests/                    # Workspace-level end-to-end and integration test suites
```

---

## 🗺️ 8. Roadmap & Project Status

Progress is tracked via [GitHub Milestones](https://github.com/hoanghainh1188/aegis-code/milestones). Status as of October 2026:

* ✅ **Phase 0 – Setup** (Completed): CI/CD pipelines, Docker sandbox hardening, build time benchmarking, 30–50 issue evaluation set ([Proposal §9](docs/design/00-proposal.md)).
* ✅ **Phase 1 – MVP / PoC** (Completed): Core agent loop, Rust and Python language profiles, empirical validation against 3 Core KPIs ([ADR 0002](docs/adr/0002-full-platform-mvp-as-checkpoint.md), [ADR 0005](docs/adr/0005-kpi-thresholds-after-phase-1.md)).
* 🚧 **Phase 2 – Platform Service** (~90% Completed – Final Stage):
  - **Completed:** TypeScript/JavaScript, Java, Go, C#/.NET profiles; unified `aegis-server` service; PostgreSQL transactional queue (`SKIP LOCKED`); GitHub/GitLab webhooks; Jira/Backlog polling; Askama + htmx dashboard with OIDC; Prometheus/Grafana monitoring; automated review rounds; developer CLI `aegis-job` on npm; on-premise Docker Compose and systemd bundles; Kubernetes dind documentation ([ADR 0008](docs/adr/0008-gate-sandbox-on-kubernetes.md)).
  - **In Progress / Remaining:** Staging cloud deployment via OpenTofu (#33), KPI evaluations on the 4 new language profiles (52 issues, [eval/README.md](eval/README.md), #259), and real-world team pilot (#158).
* ⏳ **Phase 3 – Advanced Scaling** (Planned): Spectrum-Based Fault Localization (SBFL), deep context compression (SWEzze/SaraCoder), and expansion to additional language ecosystems.

---

## 📖 9. References & Documentation

### Architectural Decision Records (ADRs)
* [ADR 0001: Platform Written in Rust](docs/adr/0001-platform-in-rust.md)
* [ADR 0002: Full Platform MVP as Checkpoint](docs/adr/0002-full-platform-mvp-as-checkpoint.md)
* [ADR 0003: Platform Service Architecture](docs/adr/0003-platform-service-architecture.md)
* [ADR 0004: Multi-Language Profiles](docs/adr/0004-multi-language-profiles.md)
* [ADR 0005: KPI Thresholds After Phase 1](docs/adr/0005-kpi-thresholds-after-phase-1.md)
* [ADR 0006: Developer Client (`aegis-job`)](docs/adr/0006-developer-client.md)
* [ADR 0007: On-Premise Deployment](docs/adr/0007-on-premise-deployment.md)
* [ADR 0008: Gate Sandbox on Kubernetes](docs/adr/0008-gate-sandbox-on-kubernetes.md)

### Detailed Guides & Runbooks
* [Reviewer Guide](docs/reviewing.md) – Reading an Aegis PR, asking for review rounds, recording the verdict
* [Developer Client Guide (`aegis-job`)](docs/client.md) – Submitting and monitoring jobs from terminal
* [Local End-to-End Trial Guide](docs/local-trial.md) – Step-by-step local platform walkthrough
* [Production AWS Runbook](docs/runbook/README.md) – Deployment, secret rotation, and incident triage
* [On-Premise Operations Runbook](docs/runbook/on-premise.md) – Single-machine Docker Compose and systemd management
* [Release Operations](docs/release.md) – Release-please, GHCR container images, and offline packages
* [Contributing & Development Conventions](CLAUDE.md) – Commit formats, branching, and pre-PR verification

> *Note: All architectural specifications (ADRs), operations runbooks, and developer procedural guides across `docs/` are fully maintained in English.*

---

## License

Licensed under the Apache License, Version 2.0: [LICENSE](LICENSE).
